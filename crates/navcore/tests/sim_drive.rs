//! The Rust simulator driving the navigator end to end: arrival on a clean
//! drive, off-route and reroute on a planned deviation, and determinism.
#![cfg(feature = "serde")]

use navcore::fixtures::{fixtures_dir, Fixture};
use navcore::{
    Navigator, NavigatorConfig, PlannedDeviation, RerouteReason, RouteSimulator, SimulatorConfig,
    TripProgress, Update,
};

fn route_simple() -> Fixture {
    Fixture::load(fixtures_dir().join("route_simple.json")).unwrap()
}

fn drive(sim: &mut RouteSimulator, nav: &mut Navigator, max_fixes: usize) -> Vec<Update> {
    let mut out = Vec::new();
    // Keep going a few fixes past the end so arrival can be detected.
    let mut tail = 5;
    for _ in 0..max_fixes {
        let raw = sim.next_fix();
        out.push(nav.update_location(raw).expect("simulated fix accepted"));
        if sim.is_finished() {
            tail -= 1;
            if tail == 0 {
                break;
            }
        }
    }
    out
}

#[test]
fn clean_simulated_drive_arrives_without_going_off_route() {
    let fx = route_simple();
    let mut sim = RouteSimulator::new(&fx.route, SimulatorConfig::default()).unwrap();
    let mut nav = Navigator::new(fx.route.clone(), NavigatorConfig::default()).unwrap();
    let updates = drive(&mut sim, &mut nav, 2000);
    assert!(sim.is_finished());
    assert!(updates.iter().all(|u| !u.position.is_off_route));
    assert_eq!(
        updates.last().unwrap().position.progress,
        TripProgress::Arrived
    );
    // First fix, one per step change, at most one more for arrival.
    let changes = updates.iter().filter(|u| u.trip_changed).count();
    assert!(changes >= fx.route.steps.len(), "{changes}");
    assert!(changes <= fx.route.steps.len() + 1, "{changes}");
}

#[test]
fn planned_deviation_triggers_off_route_and_a_reroute_request() {
    let fx = route_simple();
    let cfg = SimulatorConfig {
        speed_mps: 12.0,
        ..SimulatorConfig::default()
    };
    let mut sim = RouteSimulator::new(&fx.route, cfg).unwrap();
    sim.set_planned_deviation(PlannedDeviation {
        at_m: 300.0,
        lateral_m: 80.0,
        length_m: 300.0,
    });
    let mut nav = Navigator::new(fx.route.clone(), NavigatorConfig::default()).unwrap();
    let mut requests = Vec::new();
    let mut went_off = false;
    let mut recovered = false;
    let mut last_progress = TripProgress::NotStarted;
    for _ in 0..2000 {
        let raw = sim.next_fix();
        let u = nav.update_location(raw).unwrap();
        if u.position.is_off_route {
            went_off = true;
        } else if went_off {
            recovered = true;
        }
        if let Some(req) = nav.trip().unwrap().reroute {
            requests.push(req);
        }
        last_progress = u.position.progress;
        if sim.is_finished() && last_progress == TripProgress::Arrived {
            break;
        }
    }
    assert!(went_off, "80 m lateral offset must count as off-route");
    assert!(recovered, "must rejoin after the deviation");
    assert!(!requests.is_empty());
    assert_eq!(requests[0].reason, RerouteReason::OffRoute);
    assert_eq!(requests[0].attempt, 1);
    assert_eq!(last_progress, TripProgress::Arrived);
    // Once back on the route the request is dropped.
    assert!(nav.pending_reroute().is_none());
}

#[test]
fn simulated_drives_are_deterministic() {
    let fx = route_simple();
    let cfg = SimulatorConfig {
        seed: 7,
        ..SimulatorConfig::default()
    };
    let run = || {
        let mut sim = RouteSimulator::new(&fx.route, cfg).unwrap();
        let mut nav = Navigator::new(fx.route.clone(), NavigatorConfig::default()).unwrap();
        drive(&mut sim, &mut nav, 2000)
    };
    assert_eq!(run(), run());
}
