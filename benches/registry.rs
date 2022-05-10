use criterion::{criterion_group, criterion_main, Criterion};
use tracking_allocator::AllocationGroupToken;

fn criterion_benchmark(c: &mut Criterion) {
    c.bench_function("group token registration", |b| {
        b.iter(|| AllocationGroupToken::register());
    });
}

criterion_group!(benches, criterion_benchmark);
criterion_main!(benches);
