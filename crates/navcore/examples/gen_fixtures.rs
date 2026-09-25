//! Regenerates the JSON fixtures under `fixtures/` deterministically.
//!
//! ```sh
//! cargo run -p navcore --features serde --example gen_fixtures
//! ```

use std::f64::consts::TAU;

use navcore::fixtures::{fixtures_dir, Expected, Fixture};
use navcore::geo::{bearing_rad_to_deg, Enu, LocalFrame, RouteIndex};
use navcore::{GeoPoint, ManeuverType, RawLocation, Route, RouteStep};

/// xorshift64* — small, deterministic, dependency-free.
struct Rng(u64);

impl Rng {
    fn uniform(&mut self) -> f64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        (self.0.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 11) as f64 / (1u64 << 53) as f64
    }
    fn gauss(&mut self) -> f64 {
        let u1 = self.uniform().max(1e-12);
        let u2 = self.uniform();
        (-2.0 * u1.ln()).sqrt() * (TAU * u2).cos()
    }
}

/// A route described as corner waypoints in a local frame, each corner
/// carrying the manoeuvre performed *at* it.
struct Sketch {
    frame: LocalFrame,
    /// `(corner, maneuver, instruction)`; the first is `Depart`, the last `Arrive`.
    corners: Vec<(Enu, ManeuverType, &'static str)>,
    /// Vertex spacing when densifying straight legs, metres.
    spacing_m: f64,
}

impl Sketch {
    fn build(&self) -> Route {
        let mut geometry: Vec<GeoPoint> = Vec::new();
        let mut corner_vertex: Vec<usize> = Vec::new();
        for (i, (c, _, _)) in self.corners.iter().enumerate() {
            if i == 0 {
                geometry.push(self.frame.to_geo(*c));
                corner_vertex.push(0);
                continue;
            }
            let prev = self.corners[i - 1].0;
            let len = prev.distance_to(*c);
            let n = (len / self.spacing_m).ceil().max(1.0) as usize;
            for k in 1..=n {
                let t = k as f64 / n as f64;
                let p = Enu::new(prev.x + t * (c.x - prev.x), prev.y + t * (c.y - prev.y));
                geometry.push(self.frame.to_geo(p));
            }
            corner_vertex.push(geometry.len() - 1);
        }
        let index = RouteIndex::new(&geometry).expect("sketch geometry valid");
        let steps = (0..self.corners.len() - 1)
            .map(|i| {
                let (start, end) = (corner_vertex[i], corner_vertex[i + 1]);
                RouteStep {
                    instruction: self.corners[i].2.to_string(),
                    maneuver: self.corners[i].1,
                    start_index: start,
                    end_index: end,
                    distance_m: index.cumulative_m(end) - index.cumulative_m(start),
                }
            })
            .collect();
        Route { geometry, steps }
    }
}

struct DriveParams {
    cruise_mps: f64,
    corner_mps: f64,
    /// Distance over which speed ramps between cruise and corner speed.
    ramp_m: f64,
    /// Stationary fixes emitted before departure.
    idle_fixes: usize,
    /// Reported accuracy is uniform in this range; noise σ equals it.
    accuracy_range_m: (f64, f64),
    speed_noise_mps: f64,
    course_noise_deg: f64,
    seed: u64,
}

/// Drive the route at 1 Hz and emit noisy fixes plus ground truth.
fn simulate(
    route: &Route,
    corners_m: &[f64],
    p: &DriveParams,
) -> (Vec<RawLocation>, Vec<GeoPoint>) {
    let index = RouteIndex::new(&route.geometry).expect("route valid");
    let frame = *index.frame();
    let total = index.total_length_m();
    let mut rng = Rng(p.seed);
    let mut trace = Vec::new();
    let mut truth = Vec::new();
    let mut ts: u64 = 1_700_000_000_000;

    let mut emit = |pos: Enu, speed: f64, course_rad: Option<f64>, ts: u64, rng: &mut Rng| {
        let acc =
            p.accuracy_range_m.0 + (p.accuracy_range_m.1 - p.accuracy_range_m.0) * rng.uniform();
        let noisy = Enu::new(pos.x + acc * rng.gauss(), pos.y + acc * rng.gauss());
        trace.push(RawLocation {
            point: frame.to_geo(noisy),
            accuracy_m: Some((acc * 10.0).round() / 10.0),
            speed_mps: Some(
                ((speed + p.speed_noise_mps * rng.gauss()).max(0.0) * 100.0).round() / 100.0,
            ),
            course_deg: course_rad.map(|c| {
                (bearing_rad_to_deg(c + (p.course_noise_deg * rng.gauss()).to_radians()) * 10.0)
                    .round()
                    / 10.0
            }),
            timestamp_ms: ts,
        });
        truth.push(frame.to_geo(pos));
    };

    let start = index.point_at_distance(0.0);
    for _ in 0..p.idle_fixes {
        emit(start, 0.0, None, ts, &mut rng);
        ts += 1000;
    }

    let mut d = 0.0;
    while d < total {
        let nearest_corner = corners_m
            .iter()
            .map(|c| (c - d).abs())
            .fold(f64::INFINITY, f64::min);
        let ramp = (nearest_corner / p.ramp_m).min(1.0);
        let speed = p.corner_mps + (p.cruise_mps - p.corner_mps) * ramp;
        let pos = index.point_at_distance(d);
        let seg = index.segment_at_distance(d);
        emit(
            pos,
            speed,
            Some(index.segment_bearing_rad(seg)),
            ts,
            &mut rng,
        );
        ts += 1000;
        d += speed;
    }
    // A few fixes sitting at the destination.
    let end = index.point_at_distance(total);
    for _ in 0..3 {
        emit(end, 0.0, None, ts, &mut rng);
        ts += 1000;
    }
    (trace, truth)
}

fn corner_distances(route: &Route) -> Vec<f64> {
    let index = RouteIndex::new(&route.geometry).expect("route valid");
    route
        .steps
        .iter()
        .skip(1)
        .map(|s| index.cumulative_m(s.start_index))
        .collect()
}

fn route_simple() -> Fixture {
    let frame = LocalFrame::new(GeoPoint::new(52.5200, 13.4050));
    let sketch = Sketch {
        frame,
        corners: vec![
            (
                Enu::new(0.0, 0.0),
                ManeuverType::Depart,
                "Head east on Start Street",
            ),
            (
                Enu::new(800.0, 0.0),
                ManeuverType::TurnRight,
                "Turn right onto South Avenue",
            ),
            (
                Enu::new(800.0, -600.0),
                ManeuverType::TurnLeft,
                "Turn left onto East Road",
            ),
            (
                Enu::new(1700.0, -600.0),
                ManeuverType::Arrive,
                "Arrive at destination",
            ),
        ],
        spacing_m: 50.0,
    };
    let route = sketch.build();
    let corners = corner_distances(&route);
    let (trace, truth) = simulate(
        &route,
        &corners,
        &DriveParams {
            cruise_mps: 12.0,
            corner_mps: 4.0,
            ramp_m: 60.0,
            idle_fixes: 5,
            accuracy_range_m: (6.0, 12.0),
            speed_noise_mps: 0.5,
            course_noise_deg: 6.0,
            seed: 0x5EED_0001,
        },
    );
    Fixture {
        name: "route_simple".into(),
        route,
        trace,
        truth,
        expected: Expected {
            step_sequence: vec![0, 1, 2],
            off_route_ranges: vec![],
            arrives: true,
        },
    }
}

/// Every fixture the repo ships. Add new generators here.
fn all_fixtures() -> Vec<Fixture> {
    vec![route_simple()]
}

fn main() {
    let dir = fixtures_dir();
    std::fs::create_dir_all(&dir).expect("create fixtures dir");
    for fixture in all_fixtures() {
        let path = dir.join(format!("{}.json", fixture.name));
        std::fs::write(&path, fixture.to_json()).expect("write fixture");
        println!(
            "wrote {} ({} vertices, {} steps, {} fixes)",
            path.display(),
            fixture.route.geometry.len(),
            fixture.route.steps.len(),
            fixture.trace.len()
        );
    }
}
