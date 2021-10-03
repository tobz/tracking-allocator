use criterion::{criterion_group, criterion_main, Criterion};
use tracking_allocator::AllocationRegistry;

fn criterion_benchmark(c: &mut Criterion) {
    c.bench_function("group registration (no tags)", |b| {
        b.iter(|| AllocationRegistry::register());
    });
}

criterion_group!(benches, criterion_benchmark);
criterion_main!(benches);
