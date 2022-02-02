use std::alloc::{GlobalAlloc, Layout, System};

use crate::get_global_tracker;
use crate::token::{get_active_allocation_group, within_token_registry_rcu};

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

        if !within_token_registry_rcu() {
            if let Some(tracker) = get_global_tracker() {
                if let Some(group) = get_active_allocation_group() {
                    tracker.allocated(addr, size, group.id(), group.tags())
                }
            }
        }

        ptr
    }

    #[track_caller]
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        let addr = ptr as usize;
        self.inner.dealloc(ptr, layout);

        if let Some(tracker) = get_global_tracker() {
            tracker.deallocated(addr)
        }
    }
}
