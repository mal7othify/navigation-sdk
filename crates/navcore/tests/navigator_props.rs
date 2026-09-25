//! Property test: random GNSS noise never makes `distance_remaining_m` grow
//! between consecutive on-route fixes by more than the tolerated backtrack
//! plus the noise bound.
#![cfg(feature = "serde")]

use navcore::fixtures::{fixtures_dir, Fixture};
use navcore::geo::{Enu, RouteIndex};
use navcore::{Navigator, NavigatorConfig, RawLocation};
use proptest::prelude::*;

fn route_simple() -> Fixture {
    Fixture::load(fixtures_dir().join("route_simple.json")).unwrap()
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 48, ..ProptestConfig::default() })]

    #[test]
    fn remaining_distance_never_jumps_up(
        sigma in 1.0f64..15.0,
        noise in prop::collection::vec((-3.0f64..3.0, -3.0f64..3.0), 64),
    ) {
        let fx = route_simple();
        let cfg = NavigatorConfig::default();
        let index = RouteIndex::new(&fx.route.geometry).unwrap();
        let frame = *index.frame();
        let mut nav = Navigator::new(fx.route.clone(), cfg).unwrap();

        // Noise is bounded to ±3σ per axis, so a fix moves at most 3σ·√2.
        let noise_bound_m = 3.0 * sigma * 2f64.sqrt();
        let allowed_increase = cfg.max_backtrack_m + noise_bound_m;

        let mut prev: Option<(bool, f64)> = None;
        for (k, (truth, (nx, ny))) in fx.truth.iter().zip(noise.iter().cycle()).enumerate() {
            let e = frame.to_enu(*truth);
            let raw = RawLocation {
                point: frame.to_geo(Enu::new(e.x + nx * sigma, e.y + ny * sigma)),
                accuracy_m: Some(sigma),
                speed_mps: None,
                course_deg: None,
                timestamp_ms: 1_000_000 + k as u64 * 1000,
            };
            let s = nav.update_location(raw).unwrap();
            if let Some((was_on, prev_remaining)) = prev {
                if was_on && !s.is_off_route {
                    prop_assert!(
                        s.distance_remaining_m <= prev_remaining + allowed_increase,
                        "fix {k}: remaining jumped {prev_remaining:.1} → {:.1} (σ={sigma:.1})",
                        s.distance_remaining_m
                    );
                }
            }
            prev = Some((!s.is_off_route, s.distance_remaining_m));
        }
    }
}
