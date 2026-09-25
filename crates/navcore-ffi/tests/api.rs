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
    assert_eq!(last.expect("states").progress, TripProgress::Arrived);
    assert!(nav.state().is_some());
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
