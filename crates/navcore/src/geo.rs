//! Geometry: great-circle helpers, a local planar (ENU) frame, segment
//! projection, and [`RouteIndex`] — the spatially indexed route polyline.
//!
//! Angles are radians internally; conversion helpers are provided for the
//! degree-based public types. Distances are metres.

use core::f64::consts::{PI, TAU};

use rstar::{PointDistance, RTree, RTreeObject, AABB};

use crate::types::{GeoPoint, NavError, SnappedLocation};

/// Mean Earth radius (IUGG), metres.
pub const EARTH_RADIUS_M: f64 = 6_371_008.8;

/// Degrees → radians.
#[inline]
pub fn deg_to_rad(deg: f64) -> f64 {
    deg * (PI / 180.0)
}

/// Radians → degrees.
#[inline]
pub fn rad_to_deg(rad: f64) -> f64 {
    rad * (180.0 / PI)
}

/// Normalise a bearing to `[0, 2π)`.
#[inline]
pub fn normalize_bearing_rad(b: f64) -> f64 {
    let r = b.rem_euclid(TAU);
    // rem_euclid can return exactly TAU for tiny negative inputs.
    if r >= TAU {
        0.0
    } else {
        r
    }
}

/// Bearing in radians → degrees clockwise from north, `[0, 360)`.
#[inline]
pub fn bearing_rad_to_deg(b: f64) -> f64 {
    let d = rad_to_deg(normalize_bearing_rad(b));
    if d >= 360.0 {
        0.0
    } else {
        d
    }
}

/// Signed smallest angular difference `b - a`, in `(-π, π]`.
#[inline]
pub fn bearing_diff_rad(a: f64, b: f64) -> f64 {
    let d = (b - a).rem_euclid(TAU);
    if d > PI {
        d - TAU
    } else {
        d
    }
}

/// Great-circle distance between two points, metres (haversine).
pub fn haversine_m(a: GeoPoint, b: GeoPoint) -> f64 {
    let lat1 = deg_to_rad(a.lat);
    let lat2 = deg_to_rad(b.lat);
    let dlat = lat2 - lat1;
    let dlng = deg_to_rad(b.lng - a.lng);
    let s1 = (dlat * 0.5).sin();
    let s2 = (dlng * 0.5).sin();
    let h = s1 * s1 + lat1.cos() * lat2.cos() * s2 * s2;
    2.0 * EARTH_RADIUS_M * h.min(1.0).sqrt().asin()
}

/// Initial great-circle bearing from `a` to `b`, radians in `[0, 2π)`.
pub fn initial_bearing_rad(a: GeoPoint, b: GeoPoint) -> f64 {
    let lat1 = deg_to_rad(a.lat);
    let lat2 = deg_to_rad(b.lat);
    let dlng = deg_to_rad(b.lng - a.lng);
    let y = dlng.sin() * lat2.cos();
    let x = lat1.cos() * lat2.sin() - lat1.sin() * lat2.cos() * dlng.cos();
    normalize_bearing_rad(y.atan2(x))
}

/// A point in a [`LocalFrame`]: metres east (`x`) and north (`y`) of the origin.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Enu {
    /// Metres east of the frame origin.
    pub x: f64,
    /// Metres north of the frame origin.
    pub y: f64,
}

impl Enu {
    /// Construct from east/north metres.
    pub const fn new(x: f64, y: f64) -> Self {
        Self { x, y }
    }

    /// Squared Euclidean distance, metres².
    #[inline]
    pub fn distance_sq_to(self, other: Enu) -> f64 {
        let dx = other.x - self.x;
        let dy = other.y - self.y;
        dx * dx + dy * dy
    }

    /// Euclidean distance, metres.
    #[inline]
    pub fn distance_to(self, other: Enu) -> f64 {
        self.distance_sq_to(other).sqrt()
    }

    /// Bearing from `self` to `other`, radians clockwise from north, `[0, 2π)`.
    /// Returns `0` when the points coincide.
    #[inline]
    pub fn bearing_to_rad(self, other: Enu) -> f64 {
        let dx = other.x - self.x;
        let dy = other.y - self.y;
        if dx == 0.0 && dy == 0.0 {
            0.0
        } else {
            normalize_bearing_rad(dx.atan2(dy))
        }
    }
}

/// An equirectangular local tangent frame around an origin.
///
/// Accurate to well under 0.1 % for the tens-of-kilometre extents a single
/// route covers, and it keeps trigonometry out of the per-fix hot loop.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LocalFrame {
    origin_lat_rad: f64,
    origin_lng_rad: f64,
    cos_lat0: f64,
}

impl LocalFrame {
    /// Frame centred on `origin`.
    pub fn new(origin: GeoPoint) -> Self {
        let origin_lat_rad = deg_to_rad(origin.lat);
        Self {
            origin_lat_rad,
            origin_lng_rad: deg_to_rad(origin.lng),
            cos_lat0: origin_lat_rad.cos(),
        }
    }

    /// The frame origin in degrees.
    pub fn origin(&self) -> GeoPoint {
        GeoPoint::new(
            rad_to_deg(self.origin_lat_rad),
            rad_to_deg(self.origin_lng_rad),
        )
    }

    /// Project a geographic point into the frame.
    #[inline]
    pub fn to_enu(&self, p: GeoPoint) -> Enu {
        let dlat = deg_to_rad(p.lat) - self.origin_lat_rad;
        let mut dlng = deg_to_rad(p.lng) - self.origin_lng_rad;
        // Handle antimeridian wrap.
        if dlng > PI {
            dlng -= TAU;
        } else if dlng < -PI {
            dlng += TAU;
        }
        Enu::new(EARTH_RADIUS_M * dlng * self.cos_lat0, EARTH_RADIUS_M * dlat)
    }

    /// Unproject a frame point back to degrees.
    #[inline]
    pub fn to_geo(&self, e: Enu) -> GeoPoint {
        let lat = self.origin_lat_rad + e.y / EARTH_RADIUS_M;
        let dlng = if self.cos_lat0.abs() < 1e-12 {
            0.0
        } else {
            e.x / (EARTH_RADIUS_M * self.cos_lat0)
        };
        let mut lng = self.origin_lng_rad + dlng;
        if lng > PI {
            lng -= TAU;
        } else if lng < -PI {
            lng += TAU;
        }
        GeoPoint::new(rad_to_deg(lat), rad_to_deg(lng))
    }
}

/// Result of projecting a point onto a single segment.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SegmentProjection {
    /// Closest point on the segment (clamped to its extent).
    pub point: Enu,
    /// Fraction along the segment, clamped to `[0, 1]`.
    pub t: f64,
    /// Distance from the segment start to `point`, metres (along-track).
    pub along_m: f64,
    /// Distance from the query to `point`, metres (cross-track, unsigned).
    pub cross_m: f64,
}

/// Project `p` onto segment `a → b`, clamping to the segment's extent.
/// Zero-length segments project onto `a`.
pub fn project_onto_segment(p: Enu, a: Enu, b: Enu) -> SegmentProjection {
    let abx = b.x - a.x;
    let aby = b.y - a.y;
    let len_sq = abx * abx + aby * aby;
    if len_sq <= 0.0 {
        return SegmentProjection {
            point: a,
            t: 0.0,
            along_m: 0.0,
            cross_m: p.distance_to(a),
        };
    }
    let apx = p.x - a.x;
    let apy = p.y - a.y;
    let t = ((apx * abx + apy * aby) / len_sq).clamp(0.0, 1.0);
    let point = Enu::new(a.x + t * abx, a.y + t * aby);
    SegmentProjection {
        point,
        t,
        along_m: t * len_sq.sqrt(),
        cross_m: p.distance_to(point),
    }
}

/// Squared distance from `p` to segment `a → b`, metres². Cheaper than a
/// full projection; used by the R-tree.
#[inline]
pub fn point_segment_distance_sq(p: Enu, a: Enu, b: Enu) -> f64 {
    let abx = b.x - a.x;
    let aby = b.y - a.y;
    let len_sq = abx * abx + aby * aby;
    let apx = p.x - a.x;
    let apy = p.y - a.y;
    if len_sq <= 0.0 {
        return apx * apx + apy * apy;
    }
    let t = ((apx * abx + apy * aby) / len_sq).clamp(0.0, 1.0);
    let dx = apx - t * abx;
    let dy = apy - t * aby;
    dx * dx + dy * dy
}

/// A route segment stored in the R-tree.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Segment {
    index: usize,
    a: Enu,
    b: Enu,
}

impl RTreeObject for Segment {
    type Envelope = AABB<[f64; 2]>;

    fn envelope(&self) -> Self::Envelope {
        AABB::from_corners(
            [self.a.x.min(self.b.x), self.a.y.min(self.b.y)],
            [self.a.x.max(self.b.x), self.a.y.max(self.b.y)],
        )
    }
}

impl PointDistance for Segment {
    fn distance_2(&self, point: &[f64; 2]) -> f64 {
        point_segment_distance_sq(Enu::new(point[0], point[1]), self.a, self.b)
    }
}

/// A point projected onto the route, with route-relative measures.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct RouteProjection {
    /// Segment `geometry[i]..geometry[i+1]` the projection lies on.
    pub segment_index: usize,
    /// Projected point in the index's frame.
    pub point: Enu,
    /// Distance from the route origin to `point`, metres.
    pub distance_along_route_m: f64,
    /// Distance from the query point to `point`, metres.
    pub distance_from_route_m: f64,
    /// Bearing of the segment, radians clockwise from north, `[0, 2π)`.
    pub bearing_rad: f64,
}

/// A route polyline in a local frame with precomputed cumulative distances
/// and an R-tree over its segments for `O(log n)` candidate lookup.
#[derive(Debug, Clone)]
pub struct RouteIndex {
    frame: LocalFrame,
    vertices: Vec<Enu>,
    /// `cumulative_m[i]` is the distance from vertex 0 to vertex `i`; `len == vertices.len()`.
    cumulative_m: Vec<f64>,
    /// `segment_bearings_rad[i]` is the bearing of segment `i`; `len == vertices.len() - 1`.
    segment_bearings_rad: Vec<f64>,
    tree: RTree<Segment>,
}

impl RouteIndex {
    /// Build the index. The frame origin is the centre of the geometry's
    /// bounding box to minimise projection distortion across the route.
    pub fn new(geometry: &[GeoPoint]) -> Result<Self, NavError> {
        if geometry.len() < 2 {
            return Err(NavError::InvalidRoute(
                "route geometry needs at least two vertices".into(),
            ));
        }
        if let Some(i) = geometry.iter().position(|p| !p.is_valid()) {
            return Err(NavError::InvalidRoute(format!(
                "geometry vertex {i} has an invalid coordinate"
            )));
        }

        let (mut min_lat, mut max_lat, mut min_lng, mut max_lng) =
            (f64::MAX, f64::MIN, f64::MAX, f64::MIN);
        for p in geometry {
            min_lat = min_lat.min(p.lat);
            max_lat = max_lat.max(p.lat);
            min_lng = min_lng.min(p.lng);
            max_lng = max_lng.max(p.lng);
        }
        let frame = LocalFrame::new(GeoPoint::new(
            0.5 * (min_lat + max_lat),
            0.5 * (min_lng + max_lng),
        ));

        let vertices: Vec<Enu> = geometry.iter().map(|&p| frame.to_enu(p)).collect();

        let mut cumulative_m = Vec::with_capacity(vertices.len());
        let mut segment_bearings_rad = Vec::with_capacity(vertices.len() - 1);
        let mut segments = Vec::with_capacity(vertices.len() - 1);
        let mut acc = 0.0;
        cumulative_m.push(0.0);
        let mut last_bearing = 0.0;
        for i in 0..vertices.len() - 1 {
            let (a, b) = (vertices[i], vertices[i + 1]);
            acc += a.distance_to(b);
            cumulative_m.push(acc);
            // Zero-length segments inherit the previous bearing so heading
            // comparisons stay meaningful.
            if a != b {
                last_bearing = a.bearing_to_rad(b);
            }
            segment_bearings_rad.push(last_bearing);
            segments.push(Segment { index: i, a, b });
        }

        Ok(Self {
            frame,
            vertices,
            cumulative_m,
            segment_bearings_rad,
            tree: RTree::bulk_load(segments),
        })
    }

    /// The local frame all ENU values are expressed in.
    #[inline]
    pub fn frame(&self) -> &LocalFrame {
        &self.frame
    }

    /// Route vertices in the local frame.
    #[inline]
    pub fn vertices(&self) -> &[Enu] {
        &self.vertices
    }

    /// Number of vertices.
    #[inline]
    pub fn vertex_count(&self) -> usize {
        self.vertices.len()
    }

    /// Number of segments (`vertex_count - 1`).
    #[inline]
    pub fn segment_count(&self) -> usize {
        self.vertices.len() - 1
    }

    /// Total polyline length, metres.
    #[inline]
    pub fn total_length_m(&self) -> f64 {
        *self.cumulative_m.last().unwrap_or(&0.0)
    }

    /// Distance from the route origin to vertex `i`, metres.
    #[inline]
    pub fn cumulative_m(&self, vertex: usize) -> f64 {
        self.cumulative_m[vertex]
    }

    /// Bearing of segment `i`, radians clockwise from north.
    #[inline]
    pub fn segment_bearing_rad(&self, segment: usize) -> f64 {
        self.segment_bearings_rad[segment]
    }

    /// Project `p` onto a specific segment.
    pub fn project_onto(&self, segment: usize, p: Enu) -> RouteProjection {
        let a = self.vertices[segment];
        let b = self.vertices[segment + 1];
        let proj = project_onto_segment(p, a, b);
        RouteProjection {
            segment_index: segment,
            point: proj.point,
            distance_along_route_m: self.cumulative_m[segment] + proj.along_m,
            distance_from_route_m: proj.cross_m,
            bearing_rad: self.segment_bearings_rad[segment],
        }
    }

    /// Project `p` onto the nearest segment of the whole route.
    pub fn nearest_segment(&self, p: Enu) -> RouteProjection {
        // The tree always holds ≥1 segment, so nearest_neighbor is Some.
        let seg = self
            .tree
            .nearest_neighbor(&[p.x, p.y])
            .map(|s| s.index)
            .unwrap_or(0);
        self.project_onto(seg, p)
    }

    /// Indices of all segments within `radius_m` of `p`, ascending, written
    /// into `out` (cleared first). Reusing `out` avoids per-call allocation.
    pub fn segments_within(&self, p: Enu, radius_m: f64, out: &mut Vec<usize>) {
        out.clear();
        let r2 = radius_m * radius_m;
        out.extend(
            self.tree
                .locate_within_distance([p.x, p.y], r2)
                .map(|s| s.index),
        );
        out.sort_unstable();
    }

    /// Index of the segment containing route distance `d_m` (clamped).
    pub fn segment_at_distance(&self, d_m: f64) -> usize {
        if d_m <= 0.0 {
            return 0;
        }
        let n = self.segment_count();
        // First vertex whose cumulative distance exceeds d_m.
        let idx = self.cumulative_m.partition_point(|&c| c <= d_m);
        idx.saturating_sub(1).min(n - 1)
    }

    /// Point at route distance `d_m` from the origin (clamped to the route).
    pub fn point_at_distance(&self, d_m: f64) -> Enu {
        let seg = self.segment_at_distance(d_m);
        let a = self.vertices[seg];
        let b = self.vertices[seg + 1];
        let seg_len = self.cumulative_m[seg + 1] - self.cumulative_m[seg];
        if seg_len <= 0.0 {
            return a;
        }
        let t = ((d_m - self.cumulative_m[seg]) / seg_len).clamp(0.0, 1.0);
        Enu::new(a.x + t * (b.x - a.x), a.y + t * (b.y - a.y))
    }

    /// Convert a projection into the public degree-based type.
    pub fn to_snapped(&self, proj: &RouteProjection) -> SnappedLocation {
        SnappedLocation {
            point: self.frame.to_geo(proj.point),
            segment_index: proj.segment_index,
            distance_along_route_m: proj.distance_along_route_m,
            distance_from_route_m: proj.distance_from_route_m,
            bearing_deg: bearing_rad_to_deg(proj.bearing_rad),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LONDON: GeoPoint = GeoPoint::new(51.5074, -0.1278);
    const PARIS: GeoPoint = GeoPoint::new(48.8566, 2.3522);

    fn approx(a: f64, b: f64, tol: f64) -> bool {
        (a - b).abs() <= tol
    }

    #[test]
    fn haversine_known_distances() {
        // 0.009° of latitude = 1000.75 m with the IUGG radius.
        let d = haversine_m(GeoPoint::new(0.0, 0.0), GeoPoint::new(0.009, 0.0));
        assert!(approx(d, 1000.75, 0.02), "{d}");
        // 1° of longitude on the equator.
        let d = haversine_m(GeoPoint::new(0.0, 0.0), GeoPoint::new(0.0, 1.0));
        assert!(approx(d, 111_195.0, 1.0), "{d}");
        // London → Paris ≈ 343.5 km.
        let d = haversine_m(LONDON, PARIS);
        assert!(approx(d, 343_500.0, 700.0), "{d}");
        // Symmetric and zero for identical points.
        assert_eq!(haversine_m(LONDON, LONDON), 0.0);
        assert!(approx(
            haversine_m(LONDON, PARIS),
            haversine_m(PARIS, LONDON),
            1e-9
        ));
    }

    #[test]
    fn bearings_cardinal() {
        let o = GeoPoint::new(10.0, 10.0);
        let n = initial_bearing_rad(o, GeoPoint::new(11.0, 10.0));
        let e = initial_bearing_rad(o, GeoPoint::new(10.0, 11.0));
        let s = initial_bearing_rad(o, GeoPoint::new(9.0, 10.0));
        let w = initial_bearing_rad(o, GeoPoint::new(10.0, 9.0));
        assert!(approx(n, 0.0, 1e-9));
        assert!(approx(e, PI / 2.0, 0.01)); // slight great-circle deviation
        assert!(approx(s, PI, 1e-9));
        assert!(approx(w, 1.5 * PI, 0.01));
    }

    #[test]
    fn bearing_helpers() {
        assert!(approx(normalize_bearing_rad(-0.1), TAU - 0.1, 1e-12));
        assert!(approx(normalize_bearing_rad(TAU + 0.1), 0.1, 1e-12));
        assert_eq!(normalize_bearing_rad(-1e-18), 0.0);
        assert!(approx(bearing_diff_rad(0.1, TAU - 0.1), -0.2, 1e-12));
        assert!(approx(bearing_diff_rad(TAU - 0.1, 0.1), 0.2, 1e-12));
        assert!(approx(bearing_diff_rad(0.0, PI), PI, 1e-12));
        assert!(approx(bearing_rad_to_deg(PI / 2.0), 90.0, 1e-9));
        assert_eq!(bearing_rad_to_deg(TAU), 0.0);
    }

    #[test]
    fn local_frame_roundtrip_and_accuracy() {
        let frame = LocalFrame::new(LONDON);
        let p = GeoPoint::new(51.52, -0.09);
        let e = frame.to_enu(p);
        let back = frame.to_geo(e);
        assert!(approx(back.lat, p.lat, 1e-9));
        assert!(approx(back.lng, p.lng, 1e-9));
        // Planar distance within 0.05 % of haversine for ~3 km.
        let planar = frame.to_enu(LONDON).distance_to(e);
        let gc = haversine_m(LONDON, p);
        assert!((planar - gc).abs() / gc < 5e-4, "planar {planar} gc {gc}");
        // Axes point the right way.
        assert!(e.x > 0.0 && e.y > 0.0);
    }

    #[test]
    fn local_frame_antimeridian() {
        let frame = LocalFrame::new(GeoPoint::new(0.0, 179.9));
        let e = frame.to_enu(GeoPoint::new(0.0, -179.9));
        assert!(e.x > 0.0 && e.x < 30_000.0, "{e:?}");
        let back = frame.to_geo(e);
        assert!(approx(back.lng, -179.9, 1e-9));
    }

    #[test]
    fn projection_interior_and_clamped() {
        let a = Enu::new(0.0, 0.0);
        let b = Enu::new(100.0, 0.0);
        let p = project_onto_segment(Enu::new(30.0, 5.0), a, b);
        assert!(approx(p.t, 0.3, 1e-12));
        assert!(approx(p.along_m, 30.0, 1e-9));
        assert!(approx(p.cross_m, 5.0, 1e-9));
        assert!(approx(p.point.x, 30.0, 1e-9) && approx(p.point.y, 0.0, 1e-9));

        let before = project_onto_segment(Enu::new(-10.0, 3.0), a, b);
        assert_eq!(before.t, 0.0);
        assert_eq!(before.point, a);
        assert!(approx(before.cross_m, (100.0f64 + 9.0).sqrt(), 1e-9));

        let after = project_onto_segment(Enu::new(120.0, 0.0), a, b);
        assert_eq!(after.t, 1.0);
        assert_eq!(after.point, b);
        assert!(approx(after.along_m, 100.0, 1e-9));
        assert!(approx(after.cross_m, 20.0, 1e-9));

        let degenerate = project_onto_segment(Enu::new(3.0, 4.0), a, a);
        assert_eq!(degenerate.point, a);
        assert!(approx(degenerate.cross_m, 5.0, 1e-9));

        assert!(approx(
            point_segment_distance_sq(Enu::new(30.0, 5.0), a, b),
            25.0,
            1e-9
        ));
    }

    /// An L-shaped route: 10 vertices east along the equator, then 10 north.
    fn l_route() -> Vec<GeoPoint> {
        let mut g = Vec::new();
        for i in 0..10 {
            g.push(GeoPoint::new(0.0, i as f64 * 0.001));
        }
        for j in 1..=10 {
            g.push(GeoPoint::new(j as f64 * 0.001, 0.009));
        }
        g
    }

    #[test]
    fn index_rejects_bad_geometry() {
        assert!(RouteIndex::new(&[]).is_err());
        assert!(RouteIndex::new(&[GeoPoint::new(0.0, 0.0)]).is_err());
        assert!(RouteIndex::new(&[GeoPoint::new(0.0, 0.0), GeoPoint::new(f64::NAN, 0.0)]).is_err());
    }

    #[test]
    fn cumulative_monotonic_and_total_matches_haversine() {
        let g = l_route();
        let idx = RouteIndex::new(&g).unwrap();
        assert_eq!(idx.vertex_count(), g.len());
        assert_eq!(idx.segment_count(), g.len() - 1);
        for i in 1..idx.vertex_count() {
            assert!(idx.cumulative_m(i) > idx.cumulative_m(i - 1));
        }
        let gc: f64 = g.windows(2).map(|w| haversine_m(w[0], w[1])).sum();
        let total = idx.total_length_m();
        assert!((total - gc).abs() / gc < 1e-4, "planar {total} gc {gc}");
    }

    #[test]
    fn nearest_segment_interior_and_near_vertices() {
        let g = l_route();
        let idx = RouteIndex::new(&g).unwrap();
        let f = *idx.frame();

        // Interior of segment 3, offset 5 m north.
        let p = f.to_enu(GeoPoint::new(0.0, 0.0035));
        let p = Enu::new(p.x, p.y + 5.0);
        let r = idx.nearest_segment(p);
        assert_eq!(r.segment_index, 3);
        assert!(approx(r.distance_from_route_m, 5.0, 1e-6));
        assert!(approx(r.bearing_rad, PI / 2.0, 1e-9)); // heading east
        let expected_along =
            idx.cumulative_m(3) + 0.5 * (idx.cumulative_m(4) - idx.cumulative_m(3));
        assert!(approx(r.distance_along_route_m, expected_along, 1e-6));

        // Just past vertex 9 (the corner) on the northbound leg → segment 9.
        let corner = idx.vertices()[9];
        let r = idx.nearest_segment(Enu::new(corner.x + 2.0, corner.y + 3.0));
        assert_eq!(r.segment_index, 9);
        assert!(bearing_diff_rad(r.bearing_rad, 0.0).abs() < 1e-9); // heading north

        // Just before the corner on the eastbound leg → segment 8.
        let r = idx.nearest_segment(Enu::new(corner.x - 3.0, corner.y + 2.0));
        assert_eq!(r.segment_index, 8);

        // Off the far end → last segment, clamped to the final vertex.
        let end = *idx.vertices().last().unwrap();
        let r = idx.nearest_segment(Enu::new(end.x, end.y + 50.0));
        assert_eq!(r.segment_index, idx.segment_count() - 1);
        assert!(approx(r.distance_along_route_m, idx.total_length_m(), 1e-9));
        assert!(approx(r.distance_from_route_m, 50.0, 1e-9));
    }

    #[test]
    fn segments_within_radius() {
        let g = l_route();
        let idx = RouteIndex::new(&g).unwrap();
        let mut out = Vec::new();

        // Near the corner vertex 9, both adjacent segments within 20 m.
        let corner = idx.vertices()[9];
        idx.segments_within(Enu::new(corner.x + 1.0, corner.y + 1.0), 20.0, &mut out);
        assert_eq!(out, vec![8, 9]);

        // Middle of segment 3, tight radius → only segment 3 (segments are ~111 m).
        let mid = idx.point_at_distance(0.5 * (idx.cumulative_m(3) + idx.cumulative_m(4)));
        idx.segments_within(Enu::new(mid.x, mid.y + 10.0), 20.0, &mut out);
        assert_eq!(out, vec![3]);

        // Far away → nothing.
        idx.segments_within(Enu::new(mid.x, mid.y + 10_000.0), 50.0, &mut out);
        assert!(out.is_empty());
    }

    #[test]
    fn point_at_distance_interpolates() {
        let g = l_route();
        let idx = RouteIndex::new(&g).unwrap();
        for i in 0..idx.vertex_count() {
            let p = idx.point_at_distance(idx.cumulative_m(i));
            assert!(p.distance_to(idx.vertices()[i]) < 1e-6, "vertex {i}");
        }
        let d = 0.25 * idx.total_length_m();
        let p = idx.point_at_distance(d);
        let back = idx.nearest_segment(p);
        assert!(approx(back.distance_along_route_m, d, 1e-6));
        assert!(back.distance_from_route_m < 1e-9);
        // Clamping.
        assert_eq!(idx.point_at_distance(-5.0), idx.vertices()[0]);
        assert_eq!(
            idx.point_at_distance(idx.total_length_m() + 5.0),
            *idx.vertices().last().unwrap()
        );
        assert_eq!(
            idx.segment_at_distance(idx.total_length_m()),
            idx.segment_count() - 1
        );
    }

    #[test]
    fn zero_length_segment_inherits_bearing() {
        let g = vec![
            GeoPoint::new(0.0, 0.0),
            GeoPoint::new(0.0, 0.001),
            GeoPoint::new(0.0, 0.001),
            GeoPoint::new(0.0, 0.002),
        ];
        let idx = RouteIndex::new(&g).unwrap();
        assert!(bearing_diff_rad(idx.segment_bearing_rad(1), PI / 2.0).abs() < 1e-9);
        assert_eq!(idx.cumulative_m(1), idx.cumulative_m(2));
        let p = idx.point_at_distance(idx.cumulative_m(1));
        assert!(p.distance_to(idx.vertices()[1]) < 1e-9);
    }

    #[test]
    fn to_snapped_converts_units() {
        let g = l_route();
        let idx = RouteIndex::new(&g).unwrap();
        let p = idx.frame().to_enu(GeoPoint::new(0.00001, 0.0025));
        let proj = idx.nearest_segment(p);
        let s = idx.to_snapped(&proj);
        assert_eq!(s.segment_index, 2);
        assert!(approx(s.point.lat, 0.0, 1e-9));
        assert!(approx(s.point.lng, 0.0025, 1e-9));
        assert!(approx(s.bearing_deg, 90.0, 1e-9));
        assert!(approx(
            s.distance_from_route_m,
            haversine_m(GeoPoint::new(0.0, 0.0025), GeoPoint::new(0.00001, 0.0025)),
            0.01
        ));
    }
}
