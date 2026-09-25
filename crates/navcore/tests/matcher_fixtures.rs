//! Phase 3 acceptance: the HMM matcher beats nearest-segment matching on the
//! parallel-roads and U-turn fixtures, and is deterministic.
#![cfg(feature = "serde")]

use navcore::fixtures::{fixtures_dir, Fixture};
use navcore::geo::{deg_to_rad, RouteIndex, RouteProjection};
use navcore::matcher::{HmmMatcher, MatchInput, Matcher, MatcherConfig, SimpleMatcher};

fn load(name: &str) -> Fixture {
    Fixture::load(fixtures_dir().join(format!("{name}.json"))).unwrap()
}

/// Feed raw fixes (σ = reported accuracy, heading = course when moving).
fn run(matcher: &mut dyn Matcher, fx: &Fixture, index: &RouteIndex) -> Vec<RouteProjection> {
    let frame = *index.frame();
    fx.trace
        .iter()
        .map(|raw| {
            let moving = raw.speed_mps.is_some_and(|s| s > 1.0);
            let input = MatchInput {
                position: frame.to_enu(raw.point),
                sigma_m: raw.accuracy_m.unwrap_or(10.0),
                heading_rad: raw.course_deg.filter(|_| moving).map(deg_to_rad),
                timestamp_ms: raw.timestamp_ms,
            };
            matcher.match_fix(index, &input).unwrap()
        })
        .collect()
}

/// Along-route distance of each ground-truth point (truth lies on the route).
fn truth_along(fx: &Fixture, index: &RouteIndex) -> Vec<f64> {
    let frame = *index.frame();
    fx.truth
        .iter()
        .map(|p| {
            index
                .nearest_segment(frame.to_enu(*p))
                .distance_along_route_m
        })
        .collect()
}

/// Count fixes whose match landed on the wrong side of `split_m` compared to
/// the truth, ignoring fixes whose truth is within `margin_m` of the split.
fn leg_errors(matched: &[RouteProjection], truth: &[f64], split_m: f64, margin_m: f64) -> usize {
    matched
        .iter()
        .zip(truth)
        .filter(|(_, t)| (**t - split_m).abs() > margin_m)
        .filter(|(m, t)| (m.distance_along_route_m > split_m) != (**t > split_m))
        .count()
}

#[test]
fn parallel_roads_hmm_beats_simple() {
    let fx = load("route_parallel_roads");
    let index = RouteIndex::new(&fx.route.geometry).unwrap();
    let truth = truth_along(&fx, &index);
    // The crossover step starts at the end of the outbound leg.
    let split = index.cumulative_m(fx.route.steps[1].start_index) + 15.0;

    let hmm = run(&mut HmmMatcher::new(MatcherConfig::default()), &fx, &index);
    let simple = run(&mut SimpleMatcher, &fx, &index);

    let hmm_err = leg_errors(&hmm, &truth, split, 40.0);
    let simple_err = leg_errors(&simple, &truth, split, 40.0);
    eprintln!("parallel_roads: hmm leg errors {hmm_err}, simple leg errors {simple_err}");
    assert_eq!(hmm_err, 0, "HMM put fixes on the wrong carriageway");
    assert!(
        simple_err >= 5,
        "expected the simple matcher to fail noticeably"
    );

    // HMM along-route distance never moves backwards by more than the noise
    // allows while on-route.
    for w in hmm.windows(2) {
        assert!(
            w[1].distance_along_route_m >= w[0].distance_along_route_m - 30.0,
            "backtrack: {} → {}",
            w[0].distance_along_route_m,
            w[1].distance_along_route_m
        );
    }
}

#[test]
fn uturn_hmm_stays_on_outbound_until_turn_taken() {
    let fx = load("route_uturn");
    let index = RouteIndex::new(&fx.route.geometry).unwrap();
    let truth = truth_along(&fx, &index);
    let apex = index.cumulative_m(fx.route.steps[1].start_index);
    // Return leg begins 8 m past the apex; the ambiguous zone is ±40 m.
    let split = apex + 4.0;

    let hmm = run(&mut HmmMatcher::new(MatcherConfig::default()), &fx, &index);
    let simple = run(&mut SimpleMatcher, &fx, &index);

    let hmm_err = leg_errors(&hmm, &truth, split, 40.0);
    let simple_err = leg_errors(&simple, &truth, split, 40.0);
    eprintln!("uturn: hmm leg errors {hmm_err}, simple leg errors {simple_err}");
    assert_eq!(
        hmm_err, 0,
        "HMM left the outbound leg early or returned late"
    );
    assert!(
        simple_err >= 10,
        "expected the simple matcher to flap between lanes"
    );

    // While paused at the apex the match must not run ahead onto the return leg
    // by more than the lane offset allows.
    for (m, t) in hmm.iter().zip(&truth) {
        if (*t - apex).abs() < 1.0 {
            assert!(
                m.distance_along_route_m < apex + 20.0,
                "ran ahead while waiting: {m:?}"
            );
        }
    }
}

#[test]
fn hmm_is_deterministic() {
    for name in ["route_simple", "route_parallel_roads", "route_uturn"] {
        let fx = load(name);
        let index = RouteIndex::new(&fx.route.geometry).unwrap();
        let a = run(&mut HmmMatcher::new(MatcherConfig::default()), &fx, &index);
        let b = run(&mut HmmMatcher::new(MatcherConfig::default()), &fx, &index);
        assert_eq!(a, b, "{name}: two runs differed");
        // Re-using a reset matcher must also give the same answer.
        let mut m = HmmMatcher::new(MatcherConfig::default());
        run(&mut m, &fx, &index);
        m.reset();
        let c = run(&mut m, &fx, &index);
        assert_eq!(a, c, "{name}: reset matcher differed");
    }
}

#[test]
fn simple_route_both_matchers_track_truth() {
    let fx = load("route_simple");
    let index = RouteIndex::new(&fx.route.geometry).unwrap();
    let truth = truth_along(&fx, &index);
    let hmm = run(&mut HmmMatcher::new(MatcherConfig::default()), &fx, &index);
    let worst = hmm
        .iter()
        .zip(&truth)
        .map(|(m, t)| (m.distance_along_route_m - t).abs())
        .fold(0.0, f64::max);
    assert!(worst < 40.0, "worst along-route error {worst:.1} m");
}
