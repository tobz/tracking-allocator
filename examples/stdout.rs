use tracking_allocator::{
    AllocationGroupId, AllocationGroupToken, AllocationRegistry, AllocationTracker, Allocator,
};

use std::alloc::System;

// This is where we actually set the global allocator to be the shim allocator implementation from `tracking_allocator`.
// This allocator is purely a facade to the logic provided by the crate, which is controlled by setting a global tracker
// and registering allocation groups.  All of that is covered below.
//
// As well, you can see here that we're wrapping the system allocator.  If you want, you can construct `Allocator` by
// wrapping another allocator that implements `GlobalAlloc`.  Since this is a static, you need a way to construct ther
// allocator to be wrapped in a const fashion, but it _is_ possible.
#[global_allocator]
static GLOBAL: Allocator<System> = Allocator::system();

struct StdoutTracker;

// This is our tracker implementation.  You will always need to create an implementation of `AllocationTracker` in order
// to actually handle allocation events.  The interface is straightforward: you're notified when an allocation occurs,
// and when a deallocation occurs.
impl AllocationTracker for StdoutTracker {
    fn allocated(&self, addr: usize, size: usize, group_id: AllocationGroupId) {
        // Allocations have all the pertinent information upfront, which you may or may not want to store for further
        // analysis. Notably, deallocations also know how large they are, and what group ID they came from, so you
        // typically don't have to store much data for correlating deallocations with their original allocation.
        println!(
            "allocation -> addr=0x{:0x} size={} group_id={:?}",
            addr, size, group_id
        );
    }

    fn deallocated(
        &self,
        addr: usize,
        size: usize,
        source_group_id: AllocationGroupId,
        current_group_id: AllocationGroupId,
    ) {
        // When a deallocation occurs, as mentioned above, you have full access to the address, size of the allocation,
        // as well as the group ID the allocation was made under _and_ the active allocation group ID.
        //
        // This can be useful beyond just the obvious "track how many current bytes are allocated by the group", instead
        // going further to see the chain of where allocations end up, and so on.
        println!(
            "deallocation -> addr=0x{:0x} size={} source_group_id={:?} current_group_id={:?}",
            addr, size, source_group_id, current_group_id
        );
    }
}

fn main() {
    // Create and set our allocation tracker.  Even with the tracker set, we're still not tracking allocations yet.  We
    // need to enable tracking explicitly.
    let _ = AllocationRegistry::set_global_tracker(StdoutTracker)
        .expect("no other global tracker should be set yet");

    AllocationRegistry::enable_tracking();

    // Register an allocation group.  Allocation groups are what allocations are associated with, and allocations are
    // only tracked if an allocation group is "active".  This gives us a way to actually have another task or thread
    // processing the allocation events -- which may require allocating storage to do so -- without ending up in a weird
    // re-entrant situation if we just instrumented all allocations throughout the process.
    //
    // Callers get back a token which is required for entering/exiting the group, which causes allocations and
    // deallocations within that scope to be tracked. Additionally, a group ID can be retrieved via
    // `AllocationGroupToken::id`. Group IDs implement the necessary traits to allow them to be used as a key/value in
    // many standard collections, which allows implementors to more easily store whatever information is necessary.
    let mut local_token =
        AllocationGroupToken::register().expect("failed to register allocation group");

    // Now, get an allocation guard from our token.  This guard ensures the allocation group is marked as the current
    // allocation group, so that our allocations are properly associated.
    let local_guard = local_token.enter();

    // Now we can finally make some allocations!
    let s = String::from("Hello world!");
    let mut v = Vec::new();
    v.push(s);

    // Drop our "local" group guard.  You can also call `exit` on `AllocationGuard` to transform it back to an
    // `AllocationToken` for further reuse.  Exiting/dropping the guard will update the thread state so that any
    // allocations afterwards are once again attributed to the "global" allocation group.
    drop(local_guard);

    // Drop the vector to generate some deallocations.
    drop(v);

    // Disable tracking and read the allocation events from our receiver.
    AllocationRegistry::disable_tracking();

    // We should end up seeing four events total: two allocations for the `String` and the `Vec` associated with the
    // local allocation group, and two deallocations when we drop the `Vec`.
    //
    // The two allocations should be attributed to our local allocation group, which is group ID #2. The deallocations
    // will occur within the "root" allocation group, which is group ID #1, but they will be marked as originating from
    // group ID #2.
}
