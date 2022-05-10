use std::time::Duration;

use criterion::{criterion_group, criterion_main, Criterion};

fn criterion_benchmark(c: &mut Criterion) {
    c.bench_function("system allocation", |b| {
        // This simply measures the overhead of using the system allocator normally.
        b.iter(|| Vec::<String>::with_capacity(128));
    });
}

criterion_group!(
    name = benches;
    config = Criterion::default()
        .significance_level(0.02)
        .noise_threshold(0.05)
        .measurement_time(Duration::from_secs(30))
        .warm_up_time(Duration::from_secs(10));
    targets = criterion_benchmark
);
criterion_main!(benches);
