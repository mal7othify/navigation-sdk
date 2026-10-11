//! API tests for the FFI layer. Kept out of `src/` so the "no unwrap/expect"
//! rule can be enforced on the panic-free code with a plain grep.

use navcore_ffi::*;

#[test]
fn defaults_match_core() {
    let ffi: navcore::NavigatorConfig = NavigatorConfig::default().into();
    assert_eq!(ffi, navcore::NavigatorConfig::default());
}

#[test]
fn json_roundtrip_and_navigate() {
    let json = std::fs::read_to_string(navcore::fixtures::fixtures_dir().join("route_simple.json"))
        .expect("fixture present");
    let fixture = navcore::fixtures::Fixture::from_json(&json).expect("valid fixture");
    let route_json = serde_json_route(&fixture.route);
    let route = route_from_json(&route_json).expect("route parses");
    assert_eq!(route.steps.len(), fixture.route.steps.len());

    let nav = Navigator::new(route, NavigatorConfig::default()).expect("navigator");
    assert!(nav.state().is_none());
    let mut last = None;
    for raw in &fixture.trace {
        let raw = RawLocation {
            point: raw.point.into(),
            accuracy_m: raw.accuracy_m,
            speed_mps: raw.speed_mps,
            course_deg: raw.course_deg,
            timestamp_ms: raw.timestamp_ms,
        };
        last = Some(nav.update_location(raw).expect("plausible fix"));
    }
    let last = last.expect("states");
    assert_eq!(last.trip.progress, TripProgress::Arrived);
    assert_eq!(last.position.progress, TripProgress::Arrived);
    assert_eq!(last.trip.step_count as usize, fixture.route.steps.len());
    assert!(last.trip.reroute.is_none());
    let state = nav.state().expect("state after updates");
    assert_eq!(state.position, last.position);
    assert_eq!(state.trip, last.trip);
    assert_eq!(nav.position(), Some(last.position));
    assert_eq!(nav.trip(), Some(last.trip));
    assert!(nav.pending_reroute().is_none());
    assert_eq!(nav.reroute_attempts(), 0);
}

#[test]
fn detour_surfaces_reroute_request_and_set_route_answers_it() {
    let json = std::fs::read_to_string(navcore::fixtures::fixtures_dir().join("route_detour.json"))
        .expect("fixture present");
    let fixture = navcore::fixtures::Fixture::from_json(&json).expect("valid fixture");
    let route = route_from_json(&serde_json_route(&fixture.route)).expect("route parses");
    let nav = Navigator::new(route.clone(), NavigatorConfig::default()).expect("navigator");
    let mut request = None;
    for raw in &fixture.trace {
        let raw = RawLocation {
            point: raw.point.into(),
            accuracy_m: raw.accuracy_m,
            speed_mps: raw.speed_mps,
            course_deg: raw.course_deg,
            timestamp_ms: raw.timestamp_ms,
        };
        let state = nav.update_location(raw).expect("plausible fix");
        if let Some(req) = state.trip.reroute {
            assert!(state.trip_changed);
            request = Some(req);
            break;
        }
    }
    let request = request.expect("detour must request a reroute");
    assert_eq!(request.reason, RerouteReason::OffRoute);
    assert_eq!(request.attempt, 1);
    assert_eq!(nav.pending_reroute(), Some(request));
    assert_eq!(nav.reroute_attempts(), 1);
    nav.set_route(route).expect("route installs");
    assert!(nav.pending_reroute().is_none());
    assert_eq!(nav.reroute_attempts(), 0);
    assert!(nav.state().is_none());
}

#[test]
fn simulator_is_deterministic_and_drives_navigator_to_arrival() {
    let json = std::fs::read_to_string(navcore::fixtures::fixtures_dir().join("route_simple.json"))
        .expect("fixture present");
    let fixture = navcore::fixtures::Fixture::from_json(&json).expect("valid fixture");
    let route = route_from_json(&serde_json_route(&fixture.route)).expect("route parses");
    let cfg = SimulatorConfig::default();
    let a = RouteSimulator::new(route.clone(), cfg).expect("sim");
    let b = RouteSimulator::new(route.clone(), cfg).expect("sim");
    assert_eq!(a.next_fix(), b.next_fix());
    a.reset();
    assert_eq!(a.fixes_emitted(), 0);

    let nav = Navigator::new(route, NavigatorConfig::default()).expect("navigator");
    let mut last = None;
    for _ in 0..5000 {
        let fix = a.next_fix();
        last = Some(nav.update_location(fix).expect("simulated fix accepted"));
        if a.is_finished()
            && last.as_ref().map(|s| s.position.progress) == Some(TripProgress::Arrived)
        {
            break;
        }
    }
    assert_eq!(
        last.expect("states").position.progress,
        TripProgress::Arrived
    );
    assert!(a.distance_along_m() >= a.route_length_m());
}

#[test]
fn errors_map() {
    let bad = route_from_json("{not json");
    assert!(matches!(bad, Err(NavError::InvalidJson { .. })));
    let cfg = NavigatorConfig {
        off_route_distance_m: 0.0,
        ..NavigatorConfig::default()
    };
    let route = Route {
        geometry: vec![
            GeoPoint { lat: 0.0, lng: 0.0 },
            GeoPoint {
                lat: 0.0,
                lng: 0.001,
            },
        ],
        steps: vec![
            RouteStep {
                instruction: "go".into(),
                maneuver: ManeuverType::Depart,
                start_index: 0,
                end_index: 1,
                distance_m: 111.0,
            },
            RouteStep {
                instruction: "arrive".into(),
                maneuver: ManeuverType::Arrive,
                start_index: 1,
                end_index: 1,
                distance_m: 0.0,
            },
        ],
    };
    assert!(matches!(
        Navigator::new(route.clone(), cfg),
        Err(NavError::InvalidConfig { .. })
    ));
    let mut no_arrive = route;
    no_arrive.steps.pop();
    assert!(matches!(
        Navigator::new(no_arrive, NavigatorConfig::default()),
        Err(NavError::InvalidRoute { .. })
    ));
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
