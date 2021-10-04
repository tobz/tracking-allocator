use std::alloc::{GlobalAlloc, Layout, System};

use crate::get_global_tracker;
use crate::token::get_active_allocation_group;

/// Tracking allocator implementation.
///
/// This allocator must be installed via `#[global_allocator]` in order to take effect.  More
/// information and examples on how to do so can be found in the top-level crate documentation.
pub struct Allocator;

unsafe impl GlobalAlloc for Allocator {
    #[track_caller]
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let size = layout.size();
        let ptr = System.alloc(layout);
        let addr = ptr as usize;
        track_allocation(addr, size);
        ptr
    }

    #[track_caller]
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        let addr = ptr as usize;
        System.dealloc(ptr, layout);
        track_deallocation(addr);
    }
}

fn track_allocation(addr: usize, size: usize) {
    if let Some(tracker) = get_global_tracker() {
        if let Some(group) = get_active_allocation_group() {
            tracker.allocated(addr, size, group.id(), group.tags())
        }
    }
}

fn track_deallocation(addr: usize) {
    if let Some(tracker) = get_global_tracker() {
        tracker.deallocated(addr)
    }
}
