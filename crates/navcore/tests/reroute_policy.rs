//! Property test for the reroute policy: under random GNSS noise over the
//! detour fixture, requests are rate-limited, numbered, bounded by the
//! attempt limit, and at most one is pending at any time.
#![cfg(feature = "serde")]

use navcore::fixtures::{fixtures_dir, Fixture};
use navcore::geo::{Enu, RouteIndex};
use navcore::{Navigator, NavigatorConfig, RawLocation, RerouteReason};
use proptest::prelude::*;

fn route_detour() -> Fixture {
    Fixture::load(fixtures_dir().join("route_detour.json")).unwrap()
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 32, ..ProptestConfig::default() })]

    #[test]
    fn reroute_requests_are_bounded_rate_limited_and_singly_pending(
        sigma in 1.0f64..12.0,
        min_gap_s in 2.0f64..15.0,
        max_attempts in 1u32..5,
        noise in prop::collection::vec((-3.0f64..3.0, -3.0f64..3.0), 64),
    ) {
        let fx = route_detour();
        let cfg = NavigatorConfig {
            min_time_between_reroutes_s: min_gap_s,
            max_reroute_attempts: max_attempts,
            ..NavigatorConfig::default()
        };
        let index = RouteIndex::new(&fx.route.geometry).unwrap();
        let frame = *index.frame();
        let mut nav = Navigator::new(fx.route.clone(), cfg).unwrap();

        let mut last_request_ms: Option<u64> = None;
        let mut gave_up = false;
        let mut expected_attempt = 0u32;
        for (k, (truth, (nx, ny))) in fx.truth.iter().zip(noise.iter().cycle()).enumerate() {
            let e = frame.to_enu(*truth);
            let ts = 1_000_000 + k as u64 * 1000;
            let raw = RawLocation {
                point: frame.to_geo(Enu::new(e.x + nx * sigma, e.y + ny * sigma)),
                accuracy_m: Some(sigma),
                speed_mps: None,
                course_deg: None,
                timestamp_ms: ts,
            };
            let u = nav.update_location(raw).unwrap();
            let trip = nav.trip().unwrap().clone();

            if !u.position.is_off_route {
                prop_assert!(nav.pending_reroute().is_none(), "fix {k}: pending while on-route");
                prop_assert_eq!(nav.reroute_attempts(), 0);
                expected_attempt = 0;
                gave_up = false;
            }
            match trip.reroute {
                Some(req) if req.reason == RerouteReason::OffRoute => {
                    prop_assert!(u.trip_changed);
                    prop_assert!(u.position.is_off_route);
                    prop_assert!(!gave_up, "fix {k}: request after giving up");
                    prop_assert_eq!(req.timestamp_ms, ts);
                    expected_attempt += 1;
                    prop_assert_eq!(req.attempt, expected_attempt);
                    prop_assert!(req.attempt <= max_attempts);
                    if let Some(last) = last_request_ms {
                        let gap_s = (ts - last) as f64 / 1000.0;
                        prop_assert!(gap_s >= min_gap_s, "fix {k}: gap {gap_s} s");
                    }
                    last_request_ms = Some(ts);
                    prop_assert_eq!(nav.pending_reroute(), Some(&req));
                }
                Some(req) => {
                    prop_assert_eq!(req.reason, RerouteReason::GaveUp);
                    prop_assert_eq!(req.attempt, max_attempts);
                    prop_assert!(!gave_up, "fix {k}: gave up twice");
                    gave_up = true;
                    prop_assert!(nav.pending_reroute().is_none());
                }
                None => {}
            }
            prop_assert!(nav.reroute_attempts() <= max_attempts);
        }
    }
}
