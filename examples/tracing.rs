use tokio::{
    runtime::Builder,
    sync::{mpsc, Barrier},
};
use tracing::{info_span, Instrument};
use tracing_subscriber::{layer::SubscriberExt, Registry};
use tracking_allocator::{
    AllocationGroupToken, AllocationLayer, AllocationRegistry, AllocationTracker, Allocator,
};

use std::{
    alloc::System,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{sync_channel, SyncSender},
        Arc, Barrier as StdBarrier,
    },
    thread,
    time::Duration,
};

// This is where we actually set the global allocator to be the shim allocator implementation from
// `tracking_allocator`.  This allocator is purely a facade to the logic provided by the crate,
// which is controlled by setting a global tracker and registering allocation groups.  All of that
// is covered below.
//
// As well, you can see here that we're wrapping the system allocator.  If you want, you can
// construct `Allocator` by wrapping another allocator that implements `GlobalAlloc`.  Since this is
// a static, you need a way to construct ther allocator to be wrapped in a const fashion, but it
// _is_ possible.
#[global_allocator]
static GLOBAL: Allocator<System> = Allocator::system();

enum AllocationEvent {
    Allocated {
        addr: usize,
        size: usize,
        group_id: usize,
        tags: Option<&'static [(&'static str, &'static str)]>,
    },
    Deallocated {
        addr: usize,
    },
}

struct ChannelBackedTracker {
    // Our sender is using a bounded (fixed-size) channel to push allocation events to.  This is
    // important because we do _not_ want to allocate in order to actually send our allocation
    // events.  We would end up in a reentrant loop that could either overflow the stack, or
    // deadlock, depending on what the tracker does under the hood.
    //
    // This can happen for many different resources we might take for granted.  For example, we
    // can't write to stdout via `println!` as that would allocate, which causes an issue where the
    // reentrant call to `println!` ends up hitting a mutex around stdout, deadlocking the process.
    //
    // Also take care to note that the `AllocationEvent` structure has a fixed size and requires no
    // allocations itself to create.  This follows the same principle as using a bounded channel.
    sender: SyncSender<AllocationEvent>,
}

// This is our tracker implementation.  You will always need to create an implementation of
// `AllocationTracker` in order to actually handle allocation events.  The interface is
// straightforward: you're notified when an allocation occurs, and when a deallocation occurs.
impl AllocationTracker for ChannelBackedTracker {
    fn allocated(
        &self,
        addr: usize,
        size: usize,
        group_id: usize,
        tags: Option<&'static [(&'static str, &'static str)]>,
    ) {
        // Allocations have all the pertinent information upfront, which you must store if you want
        // to do any correlation with deallocations.
        let _ = self.sender.send(AllocationEvent::Allocated {
            addr,
            size,
            group_id,
            tags,
        });
    }

    fn deallocated(&self, addr: usize) {
        // As `tracking_allocator` itself strives to add as little overhead as possible, we only
        // forward the address being deallocated.  Your tracker implementation will need to handle
        // mapping the allocation address back to allocation group if you need to know the total
        // in-use memory, vs simply knowing how many or when allocations are occurring.
        let _ = self.sender.send(AllocationEvent::Deallocated { addr });
    }
}

impl From<SyncSender<AllocationEvent>> for ChannelBackedTracker {
    fn from(sender: SyncSender<AllocationEvent>) -> Self {
        ChannelBackedTracker { sender }
    }
}

fn main() {
    // Create our channels for receiving the allocation events.
    let (tx, rx) = sync_channel(32);
    let (done, should_exit) = create_arc_pair(AtomicBool::new(false));
    let (is_flushed, mark_flushed) = create_arc_pair(StdBarrier::new(2));

    // We spawn off our processing thread so the channels don't back up as we're exeucting.
    let _ = thread::spawn(move || {
        // We should end up seeing six events here: an allocation and deallocation related to
        // registering our local allocation group, two allocations for the `String` and the `Vec`
        // associated with the local allocation group, and two deallocations when we drop the `Vec`.
        //
        // The allocation/deallocation from registering the local allocation group will be attributed to
        // group ID #0, aka our "global" allocation group, while the two allocations for our `String`
        // and `Vec` will be associated with group ID #1, aka our "local" allocation group.
        loop {
            // We're only using a timeout here so that we ensure that we're checking to see if we
            // should actually finish up and exit.
            if let Ok(event) = rx.recv_timeout(Duration::from_millis(10)) {
                match event {
                    AllocationEvent::Allocated {
                        addr,
                        size,
                        group_id,
                        tags,
                    } => {
                        println!(
                            "allocation -> addr={:#x} size={} group_id={} tags={:?}",
                            addr, size, group_id, tags
                        );
                    }
                    AllocationEvent::Deallocated { addr } => {
                        println!("deallocation -> addr={:#x}", addr);
                    }
                }
            } else {
                // No more events, we're done.
                break;
            }

            // NOTE: Since the global tracker holds the sender side of the channel, if we just did
            // blocking receives until we got `None` back, then we would hang... because the sender
            // won't actually ever drop.
            //
            // If we don't need 100% accurate reporting, your worker thread/task could likely just
            // skip doing any sort of synchronization, and use blocking receives, since the process
            // exit would kill the thread no matter what.
            //
            // Otherwise, you need some sort of "try receiving with a timeout, check the shutdown
            // flag, then try to receive again" loop, like we have here.
            if should_exit.load(Ordering::Relaxed) {
                break;
            }
        }

        // Let the main thread know that we're done.
        let _ = mark_flushed.wait();
    });

    // Configure tracing with our [`AllocationLayer`] so that enter/exit events are handled correctly.
    let registry = Registry::default().with(AllocationLayer::new());
    tracing::subscriber::set_global_default(registry)
        .expect("failed to install tracing subscriber");

    // Create and set our allocation tracker.  Even with the tracker set, we're still not tracking
    // allocations yet.  We need to enable tracking explicitly.
    let _ = AllocationRegistry::set_global_tracker(ChannelBackedTracker::from(tx))
        .expect("no other global tracker should be set yet");

    // Acquire two allocation groups.  Allocation groups are what allocations are associated with,
    // and allocations are only tracked if an allocation group is "active".  This gives us a way to
    // actually have another task or thread processing the allocation events -- which may require
    // allocating storage to do so -- without ending up in a weird re-entrant situation if we just
    // instrumented all allocations throughout the process.
    //
    // Callers can attach tags to their group by calling `AllocationRegistry::acquire_with_tags`
    // instead, which is what we're going to do in order to provide some more useful metadata.
    let task1_token = AllocationGroupToken::acquire_with_tags(&[("task_id", "1")]);
    let task2_token = AllocationGroupToken::acquire_with_tags(&[("task_id", "2")]);

    // Even with the tracker set, we're still not tracking allocations yet.  We need to enable tracking explicitly.
    AllocationRegistry::enable_tracking();

    // Now we create our asynchronous runtime (Tokio) and spawn two simple tasks that ping-pong
    // messages to each other.  This runtime runs on the current (main) thread, so we're guaranteed
    // to have both of these tasks running on the same thread, demonstrating how tokens nest and
    // unwind themselves.
    //
    // More importantly, though, we're demonstrating how allocation groups can be implicitly
    // associated with tracing spans to enter and exit for you, automatically.
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
    });

    // Disable tracking and read the allocation events from our receiver.
    AllocationRegistry::disable_tracking();

    // Mark ourselves as done and wait for the reporting thread to process the remaining events.
    done.store(true, Ordering::Relaxed);
    let _ = is_flushed.wait();

    // We should see a lot of output on the console, and we're primarily looking for two types of
    // allocations: a 384 byte allocation from task 1, and a 3072 byte allocation from task 2.
    // These are the allocations for a `Vec<String>` with initial capacities of 16 elements and 128
    // elements, respectively.
    //
    // We'll also see a lot of deallocations: remember, `tracking-allocator` doesn't internally
    // store the allocation group/memory address mappings, so we can't limit the deallocation events
    // to only things we know about.  This means we'll see deallocations for things we didn't
    // allocate, and so with real workloads, deallocations might appear to dominate over allocations.
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

fn create_arc_pair<T>(inner: T) -> (Arc<T>, Arc<T>) {
    let first = Arc::new(inner);
    let second = Arc::clone(&first);

    (first, second)
}
