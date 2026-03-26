//! Hot path benchmarks for TokioParasite.
//!
//! Measures execution time of critical hot path components.

use criterion::{black_box, criterion_group, criterion_main, Criterion};
use tokioparasite::signal::{CrossCorrelator, RingBuffer};

fn bench_ring_buffer_push(c: &mut Criterion) {
    c.bench_function("ring_buffer_push", |b| {
        let mut buf = RingBuffer::<256>::new();
        b.iter(|| {
            buf.push(black_box(60000.0));
        });
    });
}

fn bench_correlation(c: &mut Criterion) {
    c.bench_function("correlation_calc", |b| {
        let mut corr = CrossCorrelator::<256>::new();
        // Fill buffer
        for i in 0..200 {
            corr.push(black_box(i as f64), black_box(i as f64));
        }
        b.iter(|| {
            corr.correlation();
        });
    });
}

fn bench_correlation_with_push(c: &mut Criterion) {
    c.bench_function("correlation_push_and_calc", |b| {
        let mut corr = CrossCorrelator::<256>::new();
        let mut i = 0u64;
        b.iter(|| {
            let val = (i as f64).sin() * 60000.0;
            corr.push(black_box(val), black_box(val + 1.0));
            corr.correlation();
            i += 1;
        });
    });
}

criterion_group!(
    benches,
    bench_ring_buffer_push,
    bench_correlation,
    bench_correlation_with_push,
);
criterion_main!(benches);