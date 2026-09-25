use core::f64::consts::TAU;

use criterion::{black_box, criterion_group, criterion_main, Criterion};
use navcore::geo::{Enu, RouteIndex};
use navcore::matcher::{HmmMatcher, MatchInput, Matcher, MatcherConfig, SimpleMatcher};
use navcore::GeoPoint;

fn route_2000() -> Vec<GeoPoint> {
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

/// A noisy 1 Hz drive along the whole route at ~13 m/s.
fn drive(index: &RouteIndex) -> Vec<MatchInput> {
    let mut rng = Rng(42);
    let mut out = Vec::new();
    let mut d = 0.0;
    let mut t = 0u64;
    while d < index.total_length_m() {
        let p = index.point_at_distance(d);
        let seg = index.segment_at_distance(d);
        out.push(MatchInput {
            position: Enu::new(p.x + 8.0 * rng.gauss(), p.y + 8.0 * rng.gauss()),
            sigma_m: 8.0,
            heading_rad: Some(index.segment_bearing_rad(seg) + 0.1 * rng.gauss()),
            timestamp_ms: t,
        });
        d += 13.0;
        t += 1000;
    }
    out
}

fn bench_matcher(c: &mut Criterion) {
    let g = route_2000();
    let index = RouteIndex::new(&g).unwrap();
    let fixes = drive(&index);

    c.bench_function("hmm_match_fix_2000", |b| {
        let mut m = HmmMatcher::new(MatcherConfig::default());
        let mut i = 0usize;
        b.iter(|| {
            if i == fixes.len() {
                i = 0;
                m.reset();
            }
            let r = m.match_fix(&index, black_box(&fixes[i])).unwrap();
            i += 1;
            black_box(r)
        })
    });

    c.bench_function("simple_match_fix_2000", |b| {
        let mut m = SimpleMatcher;
        let mut i = 0usize;
        b.iter(|| {
            let r = m
                .match_fix(&index, black_box(&fixes[i % fixes.len()]))
                .unwrap();
            i += 1;
            black_box(r)
        })
    });
}

criterion_group!(benches, bench_matcher);
criterion_main!(benches);
