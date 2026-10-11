//! Phase 4 acceptance: full simulated drives over every fixture.
#![cfg(feature = "serde")]

use navcore::fixtures::{fixtures_dir, Fixture};
use navcore::geo::RouteIndex;
use navcore::{FullState, Navigator, NavigatorConfig, TripProgress};

fn load(name: &str) -> Fixture {
    Fixture::load(fixtures_dir().join(format!("{name}.json"))).unwrap()
}

/// One accepted fix: the combined state plus the `trip_changed` flag.
#[derive(Debug, Clone, PartialEq)]
struct Step {
    state: FullState,
    trip_changed: bool,
}

/// Drive the fixture; returns one entry per accepted fix (all fixture fixes
/// are plausible, so this is one per fix).
fn drive(fx: &Fixture) -> Vec<Step> {
    drive_with(fx, NavigatorConfig::default())
}

fn drive_with(fx: &Fixture, cfg: NavigatorConfig) -> Vec<Step> {
    let mut nav = Navigator::new(fx.route.clone(), cfg).unwrap();
    fx.trace
        .iter()
        .map(|raw| {
            let u = nav.update_location(*raw).unwrap();
            let state = nav.full_state().unwrap();
            assert_eq!(state.position, u.position);
            Step {
                state,
                trip_changed: u.trip_changed,
            }
        })
        .collect()
}

fn first_appearances(steps: &[Step]) -> Vec<usize> {
    let mut seq = Vec::new();
    for s in steps {
        if seq.last() != Some(&s.state.trip.current_step_index) {
            seq.push(s.state.trip.current_step_index);
        }
    }
    seq
}

fn assert_common(fx: &Fixture, steps: &[Step]) {
    assert_eq!(
        first_appearances(steps),
        fx.expected.step_sequence,
        "{}: step sequence",
        fx.name
    );
    let last = &steps.last().unwrap().state;
    assert_eq!(
        last.position.progress == TripProgress::Arrived,
        fx.expected.arrives,
        "{}: arrival",
        fx.name
    );
    // Steps never regress.
    for w in steps.windows(2) {
        assert!(
            w[1].state.trip.current_step_index >= w[0].state.trip.current_step_index,
            "{}: step regressed {} → {}",
            fx.name,
            w[0].state.trip.current_step_index,
            w[1].state.trip.current_step_index
        );
    }
    for s in steps {
        let p = &s.state.position;
        let t = &s.state.trip;
        // Remaining distance is consistent with distance to manoeuvre.
        assert!(p.distance_to_next_maneuver_m <= p.distance_remaining_m + 1e-6);
        assert!(p.distance_remaining_m >= 0.0);
        // The two halves agree on what they both carry.
        assert_eq!(p.is_off_route, t.is_off_route);
        assert_eq!(p.progress, t.progress);
        assert_eq!(t.step_count, fx.route.steps.len());
    }
    // `trip_changed` is exactly "the trip state differs from the previous
    // one, or carries a reroute request".
    assert!(steps[0].trip_changed, "{}: first update", fx.name);
    for (i, w) in steps.windows(2).enumerate() {
        let (a, b) = (&w[0].state.trip, &w[1].state.trip);
        let differs = a.current_step_index != b.current_step_index
            || a.next_instruction != b.next_instruction
            || a.is_off_route != b.is_off_route
            || a.progress != b.progress
            || b.reroute.is_some();
        assert_eq!(
            w[1].trip_changed,
            differs,
            "{}: trip_changed mismatch at fix {}",
            fx.name,
            i + 1
        );
    }
}

#[test]
fn route_simple_drive() {
    let fx = load("route_simple");
    let states = drive(&fx);
    assert_common(&fx, &states);
    assert!(
        states.iter().all(|s| !s.state.position.is_off_route),
        "never off-route"
    );
    assert!(states.iter().all(|s| !s.state.needs_reroute()));
    // Instruction sequence follows the steps.
    let s0 = &states[0].state;
    assert_eq!(
        &*s0.trip.next_instruction,
        fx.route.steps[1].instruction.as_str()
    );
    // Push-style hosts would forward the trip state once per step change,
    // once for the first fix and at most once more for arrival (when it does
    // not coincide with the last step change): far fewer than one per fix.
    let changes = states.iter().filter(|s| s.trip_changed).count();
    assert!(changes >= fx.expected.step_sequence.len());
    assert!(changes <= fx.expected.step_sequence.len() + 1, "{changes}");
    assert!(changes * 10 < states.len());
}

#[test]
fn route_parallel_roads_drive() {
    let fx = load("route_parallel_roads");
    let states = drive(&fx);
    assert_common(&fx, &states);
    assert!(states.iter().all(|s| !s.state.position.is_off_route));
}

#[test]
fn route_uturn_drive() {
    let fx = load("route_uturn");
    let states = drive(&fx);
    assert_common(&fx, &states);
    assert!(states.iter().all(|s| !s.state.position.is_off_route));
}

#[test]
fn route_detour_goes_off_route_requests_reroute_and_recovers() {
    let fx = load("route_detour");
    // Unlimited attempts here; the give-up path has its own unit test.
    let cfg = NavigatorConfig {
        max_reroute_attempts: 0,
        ..NavigatorConfig::default()
    };
    let states = drive_with(&fx, cfg);
    assert_common(&fx, &states);
    let off: Vec<bool> = states
        .iter()
        .map(|s| s.state.position.is_off_route)
        .collect();

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
    for (i, is_off) in off.iter().enumerate().take(earliest_allowed) {
        assert!(!is_off, "flagged off-route too early at fix {i}");
    }
    // Flagged within a few fixes of that (the filter lags the raw position).
    let flagged_at = off.iter().position(|b| *b).expect("must go off-route");
    assert!(
        flagged_at <= earliest_allowed + 4,
        "flagged at {flagged_at}, expected by {}",
        earliest_allowed + 4
    );
    // Stays flagged through the middle of the detour.
    let mid = (first_off + last_off) / 2;
    assert!(off[mid]);
    // Cleared shortly after truth is back on the route, and stays cleared.
    let cleared_at = off[mid..]
        .iter()
        .position(|b| !*b)
        .map(|k| mid + k)
        .expect("must recover");
    assert!(
        cleared_at <= last_off + cfg.on_route_min_consecutive as usize + 6,
        "cleared at {cleared_at}, truth back on route at {last_off}"
    );
    assert!(off[cleared_at..].iter().all(|b| !*b));

    // Reroute requests: at least one, none while on-route, rate-limited,
    // numbered from 1, and the pending request is dropped on recovery.
    let requests: Vec<usize> = states
        .iter()
        .enumerate()
        .filter(|(_, s)| s.state.needs_reroute())
        .map(|(i, _)| i)
        .collect();
    assert!(!requests.is_empty());
    assert_eq!(
        requests[0], flagged_at,
        "first request on the fix that flags off-route"
    );
    for (n, &i) in requests.iter().enumerate() {
        assert!(off[i]);
        let req = states[i].state.trip.reroute.unwrap();
        assert_eq!(req.attempt as usize, n + 1);
        assert_eq!(req.timestamp_ms, fx.trace[i].timestamp_ms);
        let dest = fx.route.geometry.last().unwrap();
        assert_eq!(req.destination.lat, dest.lat);
        assert_eq!(req.destination.lng, dest.lng);
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
        if w[0].state.position.is_off_route && w[1].state.position.is_off_route {
            assert_eq!(
                w[0].state.trip.current_step_index,
                w[1].state.trip.current_step_index
            );
        }
    }
}

#[test]
fn route_detour_gives_up_after_three_unanswered_requests() {
    let fx = load("route_detour");
    let cfg = NavigatorConfig::default();
    assert_eq!(cfg.max_reroute_attempts, 3);
    let states = drive_with(&fx, cfg);
    let events: Vec<_> = states
        .iter()
        .filter_map(|s| s.state.trip.reroute)
        .map(|r| (r.reason, r.attempt))
        .collect();
    // The detour lasts long enough for the limit to matter only if the
    // vehicle stays off-route for > 3 × 10 s; either way the sequence is a
    // prefix of 1, 2, 3, GaveUp and nothing after GaveUp.
    let expected = [
        (navcore::RerouteReason::OffRoute, 1),
        (navcore::RerouteReason::OffRoute, 2),
        (navcore::RerouteReason::OffRoute, 3),
        (navcore::RerouteReason::GaveUp, 3),
    ];
    assert!(!events.is_empty());
    assert!(events.len() <= expected.len());
    assert_eq!(&events[..], &expected[..events.len()]);
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
