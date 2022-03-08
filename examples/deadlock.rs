
use std::{
    alloc::System,
};
use tracking_allocator::{
    AllocationGroupId, AllocationRegistry, AllocationTracker, Allocator,
};
use tracing::{trace, trace_span, subscriber};

#[global_allocator]
static ALLOCATOR: Allocator<System> = Allocator::system();

struct AllocatingTracker;

impl AllocationTracker for AllocatingTracker {
    fn allocated(&self, addr: usize, size: usize, _group_id: AllocationGroupId) {
        trace!{
            size = size,
            addr = addr,
            "alloc"
        };
    }

    fn deallocated(&self, addr: usize, _current_group_id: AllocationGroupId) {
        trace!{
            addr = addr,
            "free"
        };
    }
}

fn main() {
    let _ = AllocationRegistry::set_global_tracker(AllocatingTracker)
        .expect("no other global tracker should be set");

    subscriber::set_global_default(subscriber::NoSubscriber::default())
        .expect("setting tracing default failed");

    AllocationRegistry::enable_tracking();

    // uncommenting the next line avoids the deadlock:
    // let _ = std::io::Write::flush(&mut std::io::stdout());

    trace_span!("test");
}