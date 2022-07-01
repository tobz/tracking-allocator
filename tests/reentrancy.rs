//! The `allocated` and `deallocated` methods of the `AllocationTracker` used in this test,
//! themselves, allocate. This test ensures that these allocations do not lead to infinite
//! recursion.

use std::{
    alloc::System,
    sync::atomic::{AtomicU64, Ordering},
};
use tracking_allocator::{
    AllocationGroupId, AllocationGroupToken, AllocationRegistry, AllocationTracker, Allocator,
};

#[global_allocator]
static ALLOCATOR: Allocator<System> = Allocator::system();

static ALLOCATIONS: AtomicU64 = AtomicU64::new(0);
static DEALLOCATIONS: AtomicU64 = AtomicU64::new(0);

struct AllocatingTracker;

impl AllocationTracker for AllocatingTracker {
    fn allocated(
        &self,
        _addr: usize,
        _object_size: usize,
        _wrapped_size: usize,
        _group_id: AllocationGroupId,
    ) {
        ALLOCATIONS.fetch_add(1, Ordering::SeqCst);
        let _ = Box::new([0u64; 64]);
    }

    fn deallocated(
        &self,
        _addr: usize,
        _object_size: usize,
        _wrapped_size: usize,
        _source_group_id: AllocationGroupId,
        _current_group_id: AllocationGroupId,
    ) {
        DEALLOCATIONS.fetch_add(1, Ordering::SeqCst);
        let _ = Box::new([0u64; 64]);
    }
}

#[test]
fn test() {
    let _ = AllocationRegistry::set_global_tracker(AllocatingTracker)
        .expect("no other global tracker should be set");
    AllocationRegistry::enable_tracking();
    let mut local_token =
        AllocationGroupToken::register().expect("failed to register allocation group");
    let _guard = local_token.enter();

    let allocations = || ALLOCATIONS.load(Ordering::SeqCst);
    let deallocations = || DEALLOCATIONS.load(Ordering::SeqCst);

    assert_eq!(0, allocations());
    assert_eq!(0, deallocations());

    let alloc = Box::new(10); // allocate

    assert_eq!(1, allocations());
    assert_eq!(0, deallocations());

    drop(alloc); // deallocate

    assert_eq!(1, allocations());
    assert_eq!(1, deallocations());
}
