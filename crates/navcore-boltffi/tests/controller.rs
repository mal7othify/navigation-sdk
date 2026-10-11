//! In-process tests for the push controller: an in-memory listener and a
//! fake replanner stand in for the host. Kept out of `src/` so the
//! "no unwrap/expect" rule can be enforced on the panic-free code.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use navcore_boltffi::*;

#[derive(Default)]
struct Recorder {
    positions: Mutex<Vec<PositionState>>,
    trips: Mutex<Vec<TripState>>,
    sessions: Mutex<Vec<SessionState>>,
    errors: Mutex<Vec<(ErrorKind, String)>>,
}

impl NavigationListener for Recorder {
    fn on_position(&self, position: PositionState) {
        self.positions.lock().unwrap().push(position);
    }
    fn on_trip(&self, trip: TripState) {
        self.trips.lock().unwrap().push(trip);
    }
    fn on_session(&self, session: SessionState) {
        self.sessions.lock().unwrap().push(session);
    }
    fn on_error(&self, kind: ErrorKind, message: String) {
        self.errors.lock().unwrap().push((kind, message));
    }
}

/// Answers every request with the same route after yielding once, so the
/// controller's helper thread really has to poll twice.
struct SameRoute {
    route: Route,
    calls: Mutex<Vec<RerouteRequest>>,
    decline: bool,
}

#[async_trait]
impl ReplanningService for SameRoute {
    async fn replan(&self, request: RerouteRequest) -> Option<Route> {
        self.calls.lock().unwrap().push(request);
        YieldOnce(false).await;
        if self.decline {
            None
        } else {
            Some(self.route.clone())
        }
    }
}

struct YieldOnce(bool);

impl std::future::Future for YieldOnce {
    type Output = ();
    fn poll(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<()> {
        if self.0 {
            std::task::Poll::Ready(())
        } else {
            self.0 = true;
            cx.waker().wake_by_ref();
            std::task::Poll::Pending
        }
    }
}

fn fixture(name: &str) -> (Route, Vec<RawLocation>) {
    let json =
        std::fs::read_to_string(navcore::fixtures::fixtures_dir().join(format!("{name}.json")))
            .expect("fixture present");
    let fx = navcore::fixtures::Fixture::from_json(&json).expect("valid fixture");
    let route = route_from_json(&serde_json_route(&fx.route)).expect("route parses");
    let trace = fx
        .trace
        .iter()
        .map(|raw| RawLocation {
            point: GeoPoint {
                lat: raw.point.lat,
                lng: raw.point.lng,
            },
            accuracy_m: raw.accuracy_m,
            speed_mps: raw.speed_mps,
            course_deg: raw.course_deg,
            timestamp_ms: raw.timestamp_ms,
        })
        .collect();
    (route, trace)
}

fn wait_for(mut done: impl FnMut() -> bool) {
    for _ in 0..500 {
        if done() {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    panic!("condition not met within 5 s");
}

#[test]
fn listener_gets_every_position_and_few_trips() {
    let (route, trace) = fixture("route_simple");
    let controller =
        NavigationController::new(route.clone(), NavigatorConfig::default()).expect("controller");
    let rec = Arc::new(Recorder::default());
    let id = controller.add_listener(rec.clone());
    assert_eq!(
        rec.sessions.lock().unwrap().len(),
        1,
        "session on subscribe"
    );

    for raw in &trace {
        controller.update_location(*raw);
    }
    let positions = rec.positions.lock().unwrap();
    let trips = rec.trips.lock().unwrap();
    assert_eq!(positions.len(), trace.len());
    assert!(trips.len() >= route.steps.len());
    assert!(trips.len() <= route.steps.len() + 1, "{}", trips.len());
    assert_eq!(positions.last().unwrap().progress, TripProgress::Arrived);
    assert_eq!(trips.last().unwrap().progress, TripProgress::Arrived);
    assert!(rec.errors.lock().unwrap().is_empty());
    assert_eq!(controller.position(), positions.last().copied());
    assert_eq!(controller.trip().as_ref(), trips.last());
    drop(positions);
    drop(trips);

    // After removal nothing more arrives.
    controller.remove_listener(id);
    let before = rec.positions.lock().unwrap().len();
    controller.update_location(RawLocation {
        timestamp_ms: trace.last().unwrap().timestamp_ms + 1000,
        ..*trace.last().unwrap()
    });
    assert_eq!(rec.positions.lock().unwrap().len(), before);
}

#[test]
fn rejected_fix_reports_error_and_keeps_state() {
    let (route, trace) = fixture("route_simple");
    let controller =
        NavigationController::new(route, NavigatorConfig::default()).expect("controller");
    let rec = Arc::new(Recorder::default());
    controller.add_listener(rec.clone());
    controller.update_location(trace[5]);
    let before = controller.position();
    controller.update_location(trace[3]);
    let errors = rec.errors.lock().unwrap();
    assert_eq!(errors.len(), 1);
    assert_eq!(errors[0].0, ErrorKind::StaleLocation);
    assert_eq!(controller.position(), before);
}

#[test]
fn detour_triggers_core_owned_replan_and_installs_route() {
    let (route, trace) = fixture("route_detour");
    let controller =
        NavigationController::new(route.clone(), NavigatorConfig::default()).expect("controller");
    let rec = Arc::new(Recorder::default());
    controller.add_listener(rec.clone());
    let replanner = Arc::new(SameRoute {
        route: route.clone(),
        calls: Mutex::new(Vec::new()),
        decline: false,
    });
    controller.set_replanner(replanner.clone());

    for raw in &trace {
        controller.update_location(*raw);
        if controller.session().routes_installed > 0 {
            break;
        }
        // Give the helper thread a chance to finish an in-flight replan so
        // the next fixes land on the installed route.
        if controller.session().replan_in_flight {
            wait_for(|| !controller.session().replan_in_flight);
        }
    }
    wait_for(|| !controller.session().replan_in_flight);

    let calls = replanner.calls.lock().unwrap();
    assert!(!calls.is_empty(), "replanner must be called");
    assert_eq!(calls[0].reason, RerouteReason::OffRoute);
    assert_eq!(calls[0].attempt, 1);
    let session = controller.session();
    assert!(session.routes_installed >= 1);
    assert!(session.replans_succeeded >= 1);
    assert_eq!(session.replans_failed, 0);
    assert!(!session.replan_in_flight);
    // The session stream saw the replan start and finish.
    let sessions = rec.sessions.lock().unwrap();
    assert!(sessions.iter().any(|s| s.replan_in_flight));
    assert!(sessions.iter().any(|s| s.routes_installed >= 1));
}

#[test]
fn declined_replan_is_counted_and_retried() {
    let (route, trace) = fixture("route_detour");
    let cfg = NavigatorConfig {
        min_time_between_reroutes_s: 3.0,
        ..NavigatorConfig::default()
    };
    let controller = NavigationController::new(route.clone(), cfg).expect("controller");
    let replanner = Arc::new(SameRoute {
        route,
        calls: Mutex::new(Vec::new()),
        decline: true,
    });
    controller.set_replanner(replanner.clone());
    for raw in &trace {
        controller.update_location(*raw);
        if controller.session().replan_in_flight {
            wait_for(|| !controller.session().replan_in_flight);
        }
    }
    let calls = replanner.calls.lock().unwrap();
    assert!(
        calls.len() >= 2,
        "declined replans are retried: {}",
        calls.len()
    );
    for (i, c) in calls.iter().enumerate() {
        assert_eq!(c.attempt as usize, i + 1);
    }
    let session = controller.session();
    assert_eq!(session.routes_installed, 0);
    assert_eq!(session.replans_succeeded, 0);
    assert_eq!(session.replans_failed as usize, calls.len());
}

#[test]
fn stop_ignores_fixes_until_set_route() {
    let (route, trace) = fixture("route_simple");
    let controller =
        NavigationController::new(route.clone(), NavigatorConfig::default()).expect("controller");
    let rec = Arc::new(Recorder::default());
    controller.add_listener(rec.clone());
    controller.update_location(trace[0]);
    controller.stop();
    assert_eq!(controller.session().status, SessionStatus::Stopped);
    controller.update_location(trace[1]);
    assert_eq!(rec.positions.lock().unwrap().len(), 1);
    controller.set_route(route).expect("route installs");
    assert_eq!(controller.session().status, SessionStatus::Navigating);
    assert_eq!(controller.session().routes_installed, 1);
    controller.update_location(trace[2]);
    assert_eq!(rec.positions.lock().unwrap().len(), 2);
}

#[test]
fn batch_matches_single_updates() {
    let (route, trace) = fixture("route_simple");
    let a = NavigationController::new(route.clone(), NavigatorConfig::default()).expect("a");
    let b = NavigationController::new(route, NavigatorConfig::default()).expect("b");
    for raw in &trace {
        a.update_location(*raw);
    }
    b.update_location_batch(trace.clone());
    assert_eq!(a.position(), b.position());
    assert_eq!(a.trip(), b.trip());
}

#[test]
fn simulator_and_defaults_match_core() {
    let core: navcore::NavigatorConfig = NavigatorConfig::default().into();
    assert_eq!(core, navcore::NavigatorConfig::default());
    let (route, _) = fixture("route_simple");
    let sim = RouteSimulator::new(route.clone(), SimulatorConfig::default()).expect("sim");
    let controller =
        NavigationController::new(route, NavigatorConfig::default()).expect("controller");
    for _ in 0..5000 {
        controller.update_location(sim.next_fix());
        if sim.is_finished()
            && controller.position().map(|p| p.progress) == Some(TripProgress::Arrived)
        {
            break;
        }
    }
    assert_eq!(
        controller.position().map(|p| p.progress),
        Some(TripProgress::Arrived)
    );
}

/// Serialise a core route with the same serde shape `route_from_json` expects.
fn serde_json_route(route: &navcore::Route) -> String {
    let mut geometry = String::new();
    for p in &route.geometry {
        if !geometry.is_empty() {
            geometry.push(',');
        }
        geometry.push_str(&format!("{{\"lat\":{},\"lng\":{}}}", p.lat, p.lng));
    }
    let mut steps = String::new();
    for s in &route.steps {
        if !steps.is_empty() {
            steps.push(',');
        }
        steps.push_str(&format!(
            "{{\"instruction\":{:?},\"maneuver\":\"{:?}\",\"start_index\":{},\"end_index\":{},\"distance_m\":{}}}",
            s.instruction, s.maneuver, s.start_index, s.end_index, s.distance_m
        ));
    }
    format!("{{\"geometry\":[{geometry}],\"steps\":[{steps}]}}")
}
