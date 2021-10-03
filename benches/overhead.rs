use std::time::Duration;

use criterion::{criterion_group, criterion_main, Criterion};
use tracking_allocator::{AllocationRegistry, AllocationTracker, Allocator};

// Every benchmark will now run through the tracking allocator.  All we're measuring here is the
// various amounts of overhead depending on whether an allocation tracker is set, whether or not
// tracking is enabled, and so on.  The `baseline.rs` benches are what we use to establish our
// baselines for performance of various basic tasks that involve allocation and deallocation.
#[global_allocator]
static ALLOCATOR: Allocator = Allocator;

struct NoopTracker;

impl AllocationTracker for NoopTracker {
    fn allocated(
        &self,
        _addr: usize,
        _size: usize,
        _group_id: usize,
        _tags: Option<&'static [(&'static str, &'static str)]>,
    ) {
    }

    fn deallocated(&self, _addr: usize) {}
}

fn criterion_benchmark(c: &mut Criterion) {
    c.bench_function("disabled/no tracker", |b| {
        // This configuration should have the lowest overhead, which is a simple atomic load on top
        // of passing the allocation call to the system allocation.
        AllocationRegistry::disable_tracking();
        unsafe {
            AllocationRegistry::clear_global_tracker();
        }

        b.iter(|| Vec::<String>::with_capacity(128));
    });

    c.bench_function("disabled/noop tracker", |b| {
        // This should not change the timing because we always check to see if tracking enabled
        // first, so the tracker being set won't drive any other operations.
        AllocationRegistry::disable_tracking();
        unsafe {
            AllocationRegistry::clear_global_tracker();
        }
        let _ = AllocationRegistry::set_global_tracker(NoopTracker)
            .expect("no other global tracker should be set");

        b.iter(|| Vec::<String>::with_capacity(128));
    });

    c.bench_function("enabled/noop tracker", |b| {
        // This should not change the timing because we always check to see if tracking enabled
        // first, so the tracker being set won't drive any other operations.
        unsafe {
            AllocationRegistry::clear_global_tracker();
        }
        let _ = AllocationRegistry::set_global_tracker(NoopTracker)
            .expect("no other global tracker should be set");
        AllocationRegistry::enable_tracking();

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
