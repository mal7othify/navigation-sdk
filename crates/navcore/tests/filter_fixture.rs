//! Phase 2 acceptance: the Kalman filter cuts RMS position error on
//! `fixtures/route_simple.json` by at least 40 % versus the raw fixes.
#![cfg(feature = "serde")]

use navcore::filter::{FilterConfig, LocationFilter};
use navcore::fixtures::{fixtures_dir, Fixture};
use navcore::geo::{haversine_m, RouteIndex};

#[test]
fn kalman_reduces_rms_by_40_percent_on_route_simple() {
    let fx = Fixture::load(fixtures_dir().join("route_simple.json")).unwrap();
    assert_eq!(
        fx.truth.len(),
        fx.trace.len(),
        "fixture must carry ground truth"
    );

    let index = RouteIndex::new(&fx.route.geometry).unwrap();
    let frame = *index.frame();
    let mut filter = LocationFilter::new(FilterConfig::default());

    let mut raw_sq = 0.0;
    let mut filt_sq = 0.0;
    let mut n = 0usize;
    for (raw, truth) in fx.trace.iter().zip(&fx.truth) {
        let out = filter
            .update(frame.to_enu(raw.point), raw)
            .expect("fixture fixes are all plausible");
        let raw_err = haversine_m(raw.point, *truth);
        let filt_err = haversine_m(frame.to_geo(out.position), *truth);
        raw_sq += raw_err * raw_err;
        filt_sq += filt_err * filt_err;
        n += 1;
    }
    let raw_rms = (raw_sq / n as f64).sqrt();
    let filt_rms = (filt_sq / n as f64).sqrt();
    eprintln!(
        "route_simple: raw RMS {raw_rms:.2} m, filtered RMS {filt_rms:.2} m, reduction {:.1} %",
        100.0 * (1.0 - filt_rms / raw_rms)
    );
    assert!(
        filt_rms <= 0.6 * raw_rms,
        "filtered RMS {filt_rms:.2} m is not ≤ 60 % of raw RMS {raw_rms:.2} m"
    );
}
