//! # tracking-allocator
//!
//! This crate provides a global allocator implementation (compatible with [`GlobalAlloc`]) that
//! allows users to trace allocations and deallocations directly.  Allocation tokens can also be
//! registered, which allows users to get an identifier that has associated metadata, which when
//! used, can enhance the overall tracking of allocations.
//!
//! While this allocator must be installed as the global allocator for the process, it ultimately
//! defers to the [`std::alloc::System`] allocator to perform the actual allocations.
#![deny(missing_docs)]
#![warn(clippy::all)]
#![warn(clippy::cargo)]
use std::{
    error, fmt,
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc,
    },
};

mod allocator;
mod token;
#[cfg(feature = "tracing-compat")]
mod tracing;
mod util;

pub use crate::allocator::Allocator;
pub use crate::token::{AllocationGroupToken, AllocationGuard};
#[cfg(feature = "tracing-compat")]
pub use crate::tracing::AllocationLayer;

/// Whether or not allocations should be tracked.
static TRACKING_ENABLED: AtomicBool = AtomicBool::new(false);

// The global tracker.  This is called for all allocations, passing through the information to
// whichever implementation is currently set.
static mut GLOBAL_TRACKER: Option<Tracker> = None;
static GLOBAL_INIT: AtomicUsize = AtomicUsize::new(UNINITIALIZED);

const UNINITIALIZED: usize = 0;
const INITIALIZING: usize = 1;
const INITIALIZED: usize = 2;

/// Tracks allocations and deallocations.
pub trait AllocationTracker {
    /// Tracks when an allocation has occurred.
    ///
    /// If any tags were associated with the allocation group, they will be provided.
    fn allocated(
        &self,
        addr: usize,
        size: usize,
        group_id: usize,
        tags: Option<&'static [(&'static str, &'static str)]>,
    );

    /// Tracks when a deallocation has occurred.
    fn deallocated(&self, addr: usize);
}

struct Tracker {
    tracker: Arc<dyn AllocationTracker + Send + Sync + 'static>,
}

impl Tracker {
    fn from_allocation_tracker<T>(allocation_tracker: T) -> Self
    where
        T: AllocationTracker + Send + Sync + 'static,
    {
        Self {
            tracker: Arc::new(allocation_tracker),
        }
    }

    /// Tracks when an allocation has occurred.
    ///
    /// If there was an active allocation group,
    fn allocated(
        &self,
        addr: usize,
        size: usize,
        group_id: usize,
        tags: Option<&'static [(&'static str, &'static str)]>,
    ) {
        self.tracker.allocated(addr, size, group_id, tags)
    }

    /// Tracks when a deallocation has occurred.
    fn deallocated(&self, addr: usize) {
        self.tracker.deallocated(addr)
    }
}

/// Returned if trying to set the global tracker fails.
#[derive(Debug)]
pub struct SetTrackerError {
    _sealed: (),
}

impl fmt::Display for SetTrackerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.pad("a global tracker has already been set")
    }
}

impl error::Error for SetTrackerError {}

/// Handles registering tokens for tracking different allocation groups.
pub struct AllocationRegistry;

impl AllocationRegistry {
    /// Enables the tracking of allocations.
    pub fn enable_tracking() {
        TRACKING_ENABLED.store(true, Ordering::SeqCst);
    }

    /// Disables the tracking of allocations.
    pub fn disable_tracking() {
        TRACKING_ENABLED.store(false, Ordering::SeqCst);
    }

    /// Sets the global tracker.
    ///
    /// Setting a global tracker does not enable or disable the tracking of allocations, so callers
    /// still need to call `enable_tracking` after this in order to fully enable tracking.
    ///
    /// # Errors
    /// `Err(SetTrackerError)` is returned if a global tracker has already been set, otherwise `Ok(())`.
    pub fn set_global_tracker<T>(tracker: T) -> Result<(), SetTrackerError>
    where
        T: AllocationTracker + Send + Sync + 'static,
    {
        if GLOBAL_INIT
            .compare_exchange(
                UNINITIALIZED,
                INITIALIZING,
                Ordering::SeqCst,
                Ordering::SeqCst,
            )
            .is_ok()
        {
            unsafe {
                GLOBAL_TRACKER = Some(Tracker::from_allocation_tracker(tracker));
            }
            GLOBAL_INIT.store(INITIALIZED, Ordering::SeqCst);
            Ok(())
        } else {
            Err(SetTrackerError { _sealed: () })
        }
    }

    /// Clears the global tracker.
    ///
    /// # Safety
    /// Well, there is none.  It's not safe.  This method clears the static reference to the
    /// tracker, which means we're violating the central assumption that a reference with a
    /// `'static` lifetime is valid for the lifetime of the process.
    ///
    /// All of this said, you're looking at the code comments for a function that is intended to be
    /// hidden from the docs, so here's where this function may be useful: in tests.
    ///
    /// If you can ensure that only one thread is running, thus ensuring there will be no competing
    /// concurrent accesses, then this is safe.  Also, of course, this leaks whatever allocation
    /// tracker was set before. Likely not a problem in tests, but for posterityy's sake..
    ///
    /// YOU'VE BEEN WARNED. :)
    #[doc(hidden)]
    pub unsafe fn clear_global_tracker() {
        GLOBAL_INIT.store(INITIALIZING, Ordering::SeqCst);
        GLOBAL_TRACKER = None;
        GLOBAL_INIT.store(UNINITIALIZED, Ordering::SeqCst);
    }
}

#[inline(always)]
fn get_global_tracker() -> Option<&'static Tracker> {
    // If tracking isn't enabled, then there's no point returning the tracker.
    if !TRACKING_ENABLED.load(Ordering::Relaxed) {
        return None;
    }

    // Tracker has to actually be installed.
    if GLOBAL_INIT.load(Ordering::SeqCst) != INITIALIZED {
        return None;
    }

    unsafe {
        let tracker = GLOBAL_TRACKER
            .as_ref()
            .expect("global tracked marked as initialized, but failed to unwrap");
        Some(tracker)
    }
}
