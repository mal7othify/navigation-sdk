//! Integration test: the public API is enough to build and query an index on
//! a realistic-size route.

use navcore::geo::{Enu, RouteIndex};
use navcore::GeoPoint;

/// A 2,000-vertex meandering route, ~40 m between vertices.
fn long_route() -> Vec<GeoPoint> {
    (0..2000)
        .map(|i| {
            let t = i as f64;
            GeoPoint::new(
                51.5 + 0.0003 * (t * 0.05).sin() + t * 0.00002,
                -0.1 + t * 0.0005,
            )
        })
        .collect()
}

#[test]
fn nearest_segment_matches_brute_force() {
    let g = long_route();
    let idx = RouteIndex::new(&g).unwrap();
    let verts = idx.vertices();
    let mut out = Vec::new();
    for k in 0..200 {
        let seg = (k * 37) % idx.segment_count();
        let a = verts[seg];
        let b = verts[seg + 1];
        // Point offset ~7 m perpendicular from 40 % along the segment.
        let mx = a.x + 0.4 * (b.x - a.x);
        let my = a.y + 0.4 * (b.y - a.y);
        let len = a.distance_to(b);
        let (nx, ny) = (-(b.y - a.y) / len, (b.x - a.x) / len);
        let p = Enu::new(mx + 7.0 * nx, my + 7.0 * ny);

        let brute = (0..idx.segment_count())
            .map(|s| (s, idx.project_onto(s, p).distance_from_route_m))
            .min_by(|x, y| x.1.total_cmp(&y.1))
            .unwrap();
        let fast = idx.nearest_segment(p);
        assert!(
            (fast.distance_from_route_m - brute.1).abs() < 1e-9,
            "seg {seg}: tree {} vs brute {}",
            fast.segment_index,
            brute.0
        );

        idx.segments_within(p, 50.0, &mut out);
        assert!(out.contains(&fast.segment_index));
        for &s in &out {
            assert!(idx.project_onto(s, p).distance_from_route_m <= 50.0 + 1e-9);
        }
    }
}

#[test]
fn cumulative_is_monotonic_on_long_route() {
    let idx = RouteIndex::new(&long_route()).unwrap();
    for i in 1..idx.vertex_count() {
        assert!(idx.cumulative_m(i) >= idx.cumulative_m(i - 1));
    }
    assert!(idx.total_length_m() > 50_000.0);
}
