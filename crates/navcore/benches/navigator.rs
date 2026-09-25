//! Phase 4 target: a 10-minute 1 Hz drive (600 fixes) in < 50 ms.
use core::f64::consts::TAU;

use criterion::{black_box, criterion_group, criterion_main, BatchSize, Criterion};
use navcore::geo::{bearing_rad_to_deg, LocalFrame, RouteIndex};
use navcore::{GeoPoint, ManeuverType, Navigator, NavigatorConfig, RawLocation, Route, RouteStep};

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

/// ~8 km meandering route with a step every 400 m and a final arrival step.
fn route() -> Route {
    let frame = LocalFrame::new(GeoPoint::new(51.5, -0.1));
    let geometry: Vec<GeoPoint> = (0..400)
        .map(|i| {
            let t = i as f64 * 20.0;
            frame.to_geo(navcore::geo::Enu::new(t, 60.0 * (t / 400.0).sin()))
        })
        .collect();
    let index = RouteIndex::new(&geometry).unwrap();
    let mut steps = Vec::new();
    let mut start = 0;
    while start < 399 {
        let end = (start + 20).min(399);
        steps.push(RouteStep {
            instruction: format!("Continue for step at {start}"),
            maneuver: if start == 0 {
                ManeuverType::Depart
            } else {
                ManeuverType::Straight
            },
            start_index: start,
            end_index: end,
            distance_m: index.cumulative_m(end) - index.cumulative_m(start),
        });
        start = end;
    }
    steps.push(RouteStep {
        instruction: "Arrive".into(),
        maneuver: ManeuverType::Arrive,
        start_index: 399,
        end_index: 399,
        distance_m: 0.0,
    });
    Route { geometry, steps }
}

/// 600 noisy fixes at 1 Hz, ~13 m/s.
fn drive(route: &Route) -> Vec<RawLocation> {
    let index = RouteIndex::new(&route.geometry).unwrap();
    let frame = *index.frame();
    let mut rng = Rng(7);
    (0..600u64)
        .map(|k| {
            let d = (k as f64 * 13.0).min(index.total_length_m());
            let p = index.point_at_distance(d);
            let seg = index.segment_at_distance(d);
            RawLocation {
                point: frame.to_geo(navcore::geo::Enu::new(
                    p.x + 8.0 * rng.gauss(),
                    p.y + 8.0 * rng.gauss(),
                )),
                accuracy_m: Some(8.0),
                speed_mps: Some(13.0 + 0.5 * rng.gauss()),
                course_deg: Some(bearing_rad_to_deg(
                    index.segment_bearing_rad(seg) + 0.1 * rng.gauss(),
                )),
                timestamp_ms: 1_700_000_000_000 + k * 1000,
            }
        })
        .collect()
}

fn bench_navigator(c: &mut Criterion) {
    let route = route();
    let fixes = drive(&route);

    c.bench_function("navigator_drive_600_fixes", |b| {
        b.iter_batched(
            || Navigator::new(route.clone(), NavigatorConfig::default()).unwrap(),
            |mut nav| {
                for f in &fixes {
                    black_box(nav.update_location(*f).unwrap());
                }
                nav
            },
            BatchSize::SmallInput,
        )
    });

    c.bench_function("navigator_update_location", |b| {
        let mut nav = Navigator::new(route.clone(), NavigatorConfig::default()).unwrap();
        let mut i = 0usize;
        b.iter(|| {
            if i == fixes.len() {
                nav = Navigator::new(route.clone(), NavigatorConfig::default()).unwrap();
                i = 0;
            }
            let s = nav.update_location(black_box(fixes[i])).unwrap();
            i += 1;
            black_box(s)
        })
    });
}

criterion_group!(benches, bench_navigator);
criterion_main!(benches);
