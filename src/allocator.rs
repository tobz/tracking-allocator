use std::alloc::{GlobalAlloc, Layout, System};

use crate::get_global_tracker;
use crate::token::get_active_allocation_group_id;

/// Tracking allocator implementation.
///
/// This allocator must be installed via `#[global_allocator]` in order to take effect.  More
/// information on using this allocator can be found in the examples, or directly in the standard
/// library docs for [`GlobalAlloc`].
pub struct Allocator<A> {
    inner: A,
}

impl<A> Allocator<A> {
    /// Creates a new `Allocator` that wraps another allocator.
    pub const fn from_allocator(allocator: A) -> Self {
        Self { inner: allocator }
    }
}

impl Allocator<System> {
    /// Creates a new `Allocator` that wraps the system allocator.
    pub const fn system() -> Allocator<System> {
        Self::from_allocator(System)
    }
}

impl Default for Allocator<System> {
    fn default() -> Self {
        Self::from_allocator(System)
    }
}

unsafe impl<A: GlobalAlloc> GlobalAlloc for Allocator<A> {
    #[track_caller]
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let size = layout.size();
        let ptr = self.inner.alloc(layout);
        let addr = ptr as usize;

        // TODO: Should we have a thread-local that stops reentrant tracking of allocations?  This
        // would make it much, much easier to actually accomplish the storage of allocation data for
        // tallying and analysis since we need data structures that grow over time, and might
        // register new allocation groups over time as well, requiring resizing and so on.
        //
        // Biggest downside is that it's potentially another chunk of overhead that each call must
        // endure as we set and unset the "don't track reentrant-ly" flag.  We should do it and
        // benchmark, though.
        if let Some(tracker) = get_global_tracker() {
            let group_id = get_active_allocation_group_id();
            tracker.allocated(addr, size, group_id);
        }

        ptr
    }

    #[track_caller]
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        let addr = ptr as usize;
        self.inner.dealloc(ptr, layout);

        if let Some(tracker) = get_global_tracker() {
            let group_id = get_active_allocation_group_id();
            tracker.deallocated(addr, group_id);
        }
    }
}
