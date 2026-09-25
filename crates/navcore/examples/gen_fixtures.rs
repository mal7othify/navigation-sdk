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
    /// `(corner, maneuver, instruction)`; the first is `Depart`, the last
    /// `Arrive`. `None` is a geometry-only bend with no step boundary.
    corners: Vec<(Enu, Option<ManeuverType>, &'static str)>,
    /// Vertex spacing when densifying straight legs, metres.
    spacing_m: f64,
}

impl Sketch {
    fn build(&self) -> Route {
        let mut geometry: Vec<GeoPoint> = Vec::new();
        // (vertex index, maneuver, instruction) for step boundaries only.
        let mut boundaries: Vec<(usize, ManeuverType, &'static str)> = Vec::new();
        for (i, (c, maneuver, text)) in self.corners.iter().enumerate() {
            if i == 0 {
                geometry.push(self.frame.to_geo(*c));
            } else {
                let prev = self.corners[i - 1].0;
                let len = prev.distance_to(*c);
                let n = (len / self.spacing_m).ceil().max(1.0) as usize;
                for k in 1..=n {
                    let t = k as f64 / n as f64;
                    let p = Enu::new(prev.x + t * (c.x - prev.x), prev.y + t * (c.y - prev.y));
                    geometry.push(self.frame.to_geo(p));
                }
            }
            if let Some(m) = maneuver {
                boundaries.push((geometry.len() - 1, *m, text));
            }
        }
        let index = RouteIndex::new(&geometry).expect("sketch geometry valid");
        let mut steps: Vec<RouteStep> = boundaries
            .windows(2)
            .map(|w| {
                let (start, maneuver, text) = w[0];
                let end = w[1].0;
                RouteStep {
                    instruction: text.to_string(),
                    maneuver,
                    start_index: start,
                    end_index: end,
                    distance_m: index.cumulative_m(end) - index.cumulative_m(start),
                }
            })
            .collect();
        // OSRM convention: zero-length arrival step at the final vertex.
        let (last_vertex, maneuver, text) = *boundaries.last().expect("at least two corners");
        steps.push(RouteStep {
            instruction: text.to_string(),
            maneuver,
            start_index: last_vertex,
            end_index: last_vertex,
            distance_m: 0.0,
        });
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
    /// `(route distance, fix count)`: stop for this many fixes on first
    /// reaching that distance (e.g. waiting to make a U-turn).
    pause: Option<(f64, usize)>,
    /// Leave the route on first reaching `at_m`: drive `length_m` along
    /// `bearing_rad`, wait `pause_fixes`, drive back, then continue.
    detour: Option<Detour>,
    /// Reported accuracy is uniform in this range; noise σ equals it.
    accuracy_range_m: (f64, f64),
    speed_noise_mps: f64,
    course_noise_deg: f64,
    seed: u64,
}

struct Detour {
    at_m: f64,
    bearing_rad: f64,
    length_m: f64,
    pause_fixes: usize,
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
    let mut paused = false;
    let mut detoured = false;
    while d < total {
        if let Some(det) = &p.detour {
            if !detoured && d >= det.at_m {
                detoured = true;
                let origin = index.point_at_distance(d);
                let (dx, dy) = (det.bearing_rad.sin(), det.bearing_rad.cos());
                let speed = p.cruise_mps;
                let n = (det.length_m / speed).ceil() as usize;
                // Out.
                for k in 1..=n {
                    let s = (k as f64 * speed).min(det.length_m);
                    emit(
                        Enu::new(origin.x + dx * s, origin.y + dy * s),
                        speed,
                        Some(det.bearing_rad),
                        ts,
                        &mut rng,
                    );
                    ts += 1000;
                }
                let far = Enu::new(origin.x + dx * det.length_m, origin.y + dy * det.length_m);
                for _ in 0..det.pause_fixes {
                    emit(far, 0.0, None, ts, &mut rng);
                    ts += 1000;
                }
                // Back.
                let back = det.bearing_rad + std::f64::consts::PI;
                for k in 1..=n {
                    let s = (det.length_m - k as f64 * speed).max(0.0);
                    emit(
                        Enu::new(origin.x + dx * s, origin.y + dy * s),
                        speed,
                        Some(back),
                        ts,
                        &mut rng,
                    );
                    ts += 1000;
                }
            }
        }
        if let Some((at, count)) = p.pause {
            if !paused && d >= at {
                paused = true;
                let pos = index.point_at_distance(d);
                let seg = index.segment_at_distance(d);
                for _ in 0..count {
                    emit(pos, 0.0, Some(index.segment_bearing_rad(seg)), ts, &mut rng);
                    ts += 1000;
                }
            }
        }
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
                Some(ManeuverType::Depart),
                "Head east on Start Street",
            ),
            (
                Enu::new(800.0, 0.0),
                Some(ManeuverType::TurnRight),
                "Turn right onto South Avenue",
            ),
            (
                Enu::new(800.0, -600.0),
                Some(ManeuverType::TurnLeft),
                "Turn left onto East Road",
            ),
            (
                Enu::new(1700.0, -600.0),
                Some(ManeuverType::Arrive),
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
            pause: None,
            detour: None,
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
            step_sequence: vec![0, 1, 2, 3],
            off_route_ranges: vec![],
            arrives: true,
        },
    }
}

/// Out-and-back on a divided road: the return carriageway runs 30 m north of
/// the outbound one in the opposite direction. Noisy fixes mid-leg regularly
/// land closer to the wrong carriageway; heading and continuity disambiguate.
fn route_parallel_roads() -> Fixture {
    let frame = LocalFrame::new(GeoPoint::new(48.8566, 2.3522));
    let sketch = Sketch {
        frame,
        corners: vec![
            (
                Enu::new(0.0, 0.0),
                Some(ManeuverType::Depart),
                "Head east on Divided Highway",
            ),
            (
                Enu::new(1000.0, 0.0),
                Some(ManeuverType::UTurn),
                "Make a U-turn at the crossover",
            ),
            (Enu::new(1000.0, 30.0), None, ""),
            (
                Enu::new(0.0, 30.0),
                Some(ManeuverType::Arrive),
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
            cruise_mps: 15.0,
            corner_mps: 5.0,
            ramp_m: 60.0,
            idle_fixes: 3,
            pause: None,
            detour: None,
            accuracy_range_m: (8.0, 12.0),
            speed_noise_mps: 0.5,
            course_noise_deg: 6.0,
            seed: 0x5EED_0002,
        },
    );
    Fixture {
        name: "route_parallel_roads".into(),
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

/// A U-turn on a two-lane road: the return lane is only 8 m from the
/// outbound lane. The vehicle waits 6 s at the turn before taking it. The
/// matcher must stay on the outbound leg until the vehicle actually heads
/// back west. The destination is 100 m short of the start so the first fixes
/// are unambiguous, as in a real trip.
fn route_uturn() -> Fixture {
    let frame = LocalFrame::new(GeoPoint::new(40.7128, -74.0060));
    let sketch = Sketch {
        frame,
        corners: vec![
            (
                Enu::new(0.0, 0.0),
                Some(ManeuverType::Depart),
                "Head east on Main Street",
            ),
            (
                Enu::new(600.0, 0.0),
                Some(ManeuverType::UTurn),
                "Make a U-turn",
            ),
            (Enu::new(600.0, -8.0), None, ""),
            (
                Enu::new(100.0, -8.0),
                Some(ManeuverType::Arrive),
                "Arrive at destination",
            ),
        ],
        spacing_m: 40.0,
    };
    let route = sketch.build();
    let corners = corner_distances(&route);
    let (trace, truth) = simulate(
        &route,
        &corners,
        &DriveParams {
            cruise_mps: 12.0,
            corner_mps: 3.0,
            ramp_m: 50.0,
            idle_fixes: 3,
            pause: Some((600.0, 6)),
            detour: None,
            accuracy_range_m: (8.0, 12.0),
            speed_noise_mps: 0.5,
            course_noise_deg: 6.0,
            seed: 0x5EED_0003,
        },
    );
    Fixture {
        name: "route_uturn".into(),
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

/// `route_simple` again, but the driver overshoots the first right turn by
/// 150 m, stops, comes back and completes the route. Exercises off-route
/// detection, rate-limited reroute requests and recovery.
fn route_detour() -> Fixture {
    let mut fx = route_simple();
    let corners = corner_distances(&fx.route);
    let (trace, truth) = simulate(
        &fx.route,
        &corners,
        &DriveParams {
            cruise_mps: 12.0,
            corner_mps: 4.0,
            ramp_m: 60.0,
            idle_fixes: 2,
            pause: None,
            detour: Some(Detour {
                at_m: 800.0,
                bearing_rad: std::f64::consts::FRAC_PI_2, // keep heading east
                length_m: 150.0,
                pause_fixes: 3,
            }),
            accuracy_range_m: (6.0, 12.0),
            speed_noise_mps: 0.5,
            course_noise_deg: 6.0,
            seed: 0x5EED_0004,
        },
    );
    fx.name = "route_detour".into();
    fx.trace = trace;
    fx.truth = truth;
    fx.expected = Expected {
        step_sequence: vec![0, 1, 2, 3],
        // Filled in by the test from ground truth; kept empty here.
        off_route_ranges: vec![],
        arrives: true,
    };
    fx
}

/// Every fixture the repo ships. Add new generators here.
fn all_fixtures() -> Vec<Fixture> {
    vec![
        route_simple(),
        route_parallel_roads(),
        route_uturn(),
        route_detour(),
    ]
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
