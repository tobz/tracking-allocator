use criterion::{criterion_group, criterion_main, Criterion};
use tracking_allocator::AllocationGroupToken;

static TAGS: &'static [(&'static str, &'static str)] = &[("component", "foo"), ("service", "http")];

fn criterion_benchmark(c: &mut Criterion) {
    c.bench_function("group registration (no tags)", |b| {
        b.iter(|| AllocationGroupToken::acquire());
    });

    c.bench_function("group registration (tags)", |b| {
        b.iter(|| AllocationGroupToken::acquire_with_tags(TAGS));
    });
}

criterion_group!(benches, criterion_benchmark);
criterion_main!(benches);
