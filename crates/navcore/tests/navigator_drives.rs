//! Phase 4 acceptance: full simulated drives over every fixture.
#![cfg(feature = "serde")]

use navcore::fixtures::{fixtures_dir, Fixture};
use navcore::geo::RouteIndex;
use navcore::{Navigator, NavigatorConfig, TripProgress, TripState};

fn load(name: &str) -> Fixture {
    Fixture::load(fixtures_dir().join(format!("{name}.json"))).unwrap()
}

/// Drive the fixture; returns one state per accepted fix (all fixture fixes
/// are plausible, so this is one per fix).
fn drive(fx: &Fixture) -> Vec<TripState> {
    let mut nav = Navigator::new(fx.route.clone(), NavigatorConfig::default()).unwrap();
    fx.trace
        .iter()
        .map(|raw| nav.update_location(*raw).unwrap())
        .collect()
}

fn first_appearances(states: &[TripState]) -> Vec<usize> {
    let mut seq = Vec::new();
    for s in states {
        if seq.last() != Some(&s.current_step_index) {
            seq.push(s.current_step_index);
        }
    }
    seq
}

fn assert_common(fx: &Fixture, states: &[TripState]) {
    assert_eq!(
        first_appearances(states),
        fx.expected.step_sequence,
        "{}: step sequence",
        fx.name
    );
    let last = states.last().unwrap();
    assert_eq!(
        last.progress == TripProgress::Arrived,
        fx.expected.arrives,
        "{}: arrival",
        fx.name
    );
    // Steps never regress.
    for w in states.windows(2) {
        assert!(
            w[1].current_step_index >= w[0].current_step_index,
            "{}: step regressed {} → {}",
            fx.name,
            w[0].current_step_index,
            w[1].current_step_index
        );
    }
    // Remaining distance is consistent with distance to manoeuvre.
    for s in states {
        assert!(s.distance_to_next_maneuver_m <= s.distance_remaining_m + 1e-6);
        assert!(s.distance_remaining_m >= 0.0);
    }
}

#[test]
fn route_simple_drive() {
    let fx = load("route_simple");
    let states = drive(&fx);
    assert_common(&fx, &states);
    assert!(states.iter().all(|s| !s.is_off_route), "never off-route");
    assert!(states.iter().all(|s| !s.needs_reroute));
    // Instruction sequence follows the steps.
    let s0 = &states[0];
    assert_eq!(
        &*s0.next_instruction,
        fx.route.steps[1].instruction.as_str()
    );
}

#[test]
fn route_parallel_roads_drive() {
    let fx = load("route_parallel_roads");
    let states = drive(&fx);
    assert_common(&fx, &states);
    assert!(states.iter().all(|s| !s.is_off_route));
}

#[test]
fn route_uturn_drive() {
    let fx = load("route_uturn");
    let states = drive(&fx);
    assert_common(&fx, &states);
    assert!(states.iter().all(|s| !s.is_off_route));
}

#[test]
fn route_detour_goes_off_route_requests_reroute_and_recovers() {
    let fx = load("route_detour");
    let cfg = NavigatorConfig::default();
    let states = drive(&fx);
    assert_common(&fx, &states);

    // Ground truth says which fixes are really off the route.
    let index = RouteIndex::new(&fx.route.geometry).unwrap();
    let frame = *index.frame();
    let truly_off: Vec<bool> = fx
        .truth
        .iter()
        .map(|p| {
            index
                .nearest_segment(frame.to_enu(*p))
                .distance_from_route_m
                > cfg.off_route_distance_m
        })
        .collect();
    let first_off = truly_off.iter().position(|b| *b).unwrap();
    let last_off = truly_off.iter().rposition(|b| *b).unwrap();
    assert!(last_off > first_off + 10, "detour should last a while");

    // Not flagged before the minimum consecutive count could be reached.
    let earliest_allowed = first_off + cfg.off_route_min_consecutive as usize - 1;
    for (i, s) in states.iter().enumerate().take(earliest_allowed) {
        assert!(!s.is_off_route, "flagged off-route too early at fix {i}");
    }
    // Flagged within a few fixes of that (the filter lags the raw position).
    let flagged_at = states
        .iter()
        .position(|s| s.is_off_route)
        .expect("must go off-route");
    assert!(
        flagged_at <= earliest_allowed + 4,
        "flagged at {flagged_at}, expected by {}",
        earliest_allowed + 4
    );
    // Stays flagged through the middle of the detour.
    let mid = (first_off + last_off) / 2;
    assert!(states[mid].is_off_route);
    // Cleared shortly after truth is back on the route, and stays cleared.
    let cleared_at = states[mid..]
        .iter()
        .position(|s| !s.is_off_route)
        .map(|k| mid + k)
        .expect("must recover");
    assert!(
        cleared_at <= last_off + cfg.on_route_min_consecutive as usize + 6,
        "cleared at {cleared_at}, truth back on route at {last_off}"
    );
    assert!(states[cleared_at..].iter().all(|s| !s.is_off_route));

    // Reroute requests: at least one, none while on-route, rate-limited.
    let requests: Vec<usize> = states
        .iter()
        .enumerate()
        .filter(|(_, s)| s.needs_reroute)
        .map(|(i, _)| i)
        .collect();
    assert!(!requests.is_empty());
    assert_eq!(
        requests[0], flagged_at,
        "first request on the fix that flags off-route"
    );
    for &i in &requests {
        assert!(states[i].is_off_route);
    }
    for w in requests.windows(2) {
        let dt_s = (fx.trace[w[1]].timestamp_ms - fx.trace[w[0]].timestamp_ms) as f64 / 1000.0;
        assert!(
            dt_s >= cfg.min_time_between_reroutes_s,
            "requests {w:?} only {dt_s} s apart"
        );
    }
    // The step does not advance while off-route.
    for w in states.windows(2) {
        if w[0].is_off_route && w[1].is_off_route {
            assert_eq!(w[0].current_step_index, w[1].current_step_index);
        }
    }
}

#[test]
fn drives_are_deterministic() {
    for name in [
        "route_simple",
        "route_parallel_roads",
        "route_uturn",
        "route_detour",
    ] {
        let fx = load(name);
        assert_eq!(drive(&fx), drive(&fx), "{name}");
    }
}
