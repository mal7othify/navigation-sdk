use criterion::{black_box, criterion_group, criterion_main, BatchSize, Criterion};
use navcore::geo::{haversine_m, Enu, RouteIndex};
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

/// Deterministic query points scattered ~10 m off the route.
fn queries(idx: &RouteIndex, n: usize) -> Vec<Enu> {
    let verts = idx.vertices();
    (0..n)
        .map(|k| {
            let seg = (k * 7919) % idx.segment_count();
            let a = verts[seg];
            let b = verts[seg + 1];
            let t = ((k * 31) % 100) as f64 / 100.0;
            let off = (((k * 13) % 21) as f64) - 10.0;
            Enu::new(a.x + t * (b.x - a.x) + off, a.y + t * (b.y - a.y) - off)
        })
        .collect()
}

fn bench_geo(c: &mut Criterion) {
    let g = route_2000();
    let idx = RouteIndex::new(&g).unwrap();
    let qs = queries(&idx, 1024);

    c.bench_function("route_index_build_2000", |b| {
        b.iter(|| RouteIndex::new(black_box(&g)).unwrap())
    });

    c.bench_function("nearest_segment_2000", |b| {
        let mut i = 0usize;
        b.iter(|| {
            let q = qs[i % qs.len()];
            i += 1;
            black_box(idx.nearest_segment(black_box(q)))
        })
    });

    c.bench_function("segments_within_50m_2000", |b| {
        let mut i = 0usize;
        b.iter_batched(
            || Vec::with_capacity(16),
            |mut out| {
                let q = qs[i % qs.len()];
                i += 1;
                idx.segments_within(black_box(q), 50.0, &mut out);
                black_box(out)
            },
            BatchSize::SmallInput,
        )
    });

    c.bench_function("haversine", |b| {
        b.iter(|| haversine_m(black_box(g[0]), black_box(g[1])))
    });
}

criterion_group!(benches, bench_geo);
criterion_main!(benches);
