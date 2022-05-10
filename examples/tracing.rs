use tokio::{
    runtime::Builder,
    sync::{mpsc, Barrier},
};
use tracing::{info_span, Instrument};
use tracing_subscriber::{layer::SubscriberExt, Registry};
use tracking_allocator::{
    AllocationGroupId, AllocationGroupToken, AllocationLayer, AllocationRegistry,
    AllocationTracker, Allocator,
};

use std::{alloc::System, sync::Arc};

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
        // as well as the group ID the allocation was made under _and_ the current allocation group ID.
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
    // Configure tracing with our [`AllocationLayer`] so that enter/exit events are handled correctly.
    let registry = Registry::default().with(AllocationLayer::new());
    tracing::subscriber::set_global_default(registry)
        .expect("failed to install tracing subscriber");

    // Create and set our allocation tracker.  Even with the tracker set, we're still not tracking allocations yet.  We
    // need to enable tracking explicitly.
    let _ = AllocationRegistry::set_global_tracker(StdoutTracker)
        .expect("no other global tracker should be set yet");

    // Register two allocation groups.  Allocation groups are what allocations are associated with.  and if there is no
    // user-register allocation group active during an allocation, the "root" allocation group is used.  This matches
    // the value returned by `AllocationGroupId::ROOT`.
    //
    // This gives us a way to actually have another task or thread processing the allocation events -- which may require
    // allocating storage to do so -- without ending up in a weird re-entrant situation if we just instrumented all
    // allocations throughout the process.
    let task1_token =
        AllocationGroupToken::register().expect("failed to register allocation group");
    let task2_token =
        AllocationGroupToken::register().expect("failed to register allocation group");

    // Even with the tracker set, we're still not tracking allocations yet.  We need to enable tracking explicitly.
    AllocationRegistry::enable_tracking();

    // Now we create our asynchronous runtime (Tokio) and spawn two simple tasks that ping-pong messages to each other.
    // This runtime runs on the current (main) thread, so we're guaranteed to have both of these tasks running on the
    // same thread, demonstrating how tokens nest and unwind themselves.
    //
    // More importantly, though, we're demonstrating how allocation groups can be implicitly associated with tracing
    // spans to enter and exit for you, automatically.
    let basic_rt = Builder::new_current_thread()
        .build()
        .expect("failed to build current-thread runtime");

    basic_rt.block_on(async move {
        // Create a barrier so our tasks start only after they've both been created.
        let barrier1 = Arc::new(Barrier::new(2));
        let barrier2 = Arc::clone(&barrier1);

        // Create the ping-pong channels.
        let (tx1, rx2) = mpsc::channel(1);
        let (tx2, rx1) = mpsc::channel(1);

        // Create our two tasks, attaching their respective allocation groups.
        let task1_span = info_span!("task1");
        task1_token.attach_to_span(&task1_span);
        let task1 = ping_pong(barrier1, 16, tx1, rx1).instrument(task1_span);

        let task2_span = info_span!("task2");
        task2_token.attach_to_span(&task2_span);
        let task2 = ping_pong(barrier2, 128, tx2, rx2).instrument(task2_span);

        // Now let them run and wait for them to complete.
        let handle1 = tokio::spawn(task1);
        let handle2 = tokio::spawn(task2);

        let _ = handle1.await.expect("task1 panicked unexpectedly");
        let _ = handle2.await.expect("task2 panicked unexpectedly");

        println!("Done.");
    });

    // Disable tracking and read the allocation events from our receiver.
    AllocationRegistry::disable_tracking();

    // We should see a lot of output on the console, and we're primarily looking for two types of allocations: a 384
    // byte allocation from task 1, and a 3072 byte allocation from task 2.  These are the allocations for a
    // `Vec<String>` with initial capacities of 16 elements and 128 elements, respectively.
    //
    // Like the `stdout` example mentions, allocations will always know which allocation group they were allocated
    // within, and deallocations will not only list which allocation group the pointer was allocated within, but also
    // the current allocation group.
}

async fn ping_pong(
    barrier: Arc<Barrier>,
    buf_size: usize,
    tx: mpsc::Sender<Vec<String>>,
    mut rx: mpsc::Receiver<Vec<String>>,
) {
    barrier.wait().await;

    let mut counter = 3;
    while counter > 0 {
        // We allocate this vector on our side, and send it to the other task to be deallocated.
        let buf: Vec<String> = Vec::with_capacity(buf_size);
        let _ = tx.send(buf).await.expect("tx send should not fail");

        // We receive another buffer from the other, and deallocate it for them.
        let their_buf = rx.recv().await.expect("rx recv should not be empty");
        drop(their_buf);

        counter -= 1;
    }
}
