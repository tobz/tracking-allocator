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
    alloc::{GlobalAlloc, Layout, System},
    borrow::Cow,
    cell::RefCell,
    error, fmt,
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc,
    },
};

use arc_swap::ArcSwapOption;
use im::Vector;

mod util;
use crate::util::PhantomNotSend;

/// Whether or not allocations should be tracked.
static TRACKING_ENABLED: AtomicBool = AtomicBool::new(false);

thread_local! {
    /// The currently executing allocation token.
    ///
    /// Any allocations which occur on this thread will be associated with whichever token is
    /// present at the time of the allocation.
    pub(crate) static CURRENT_ALLOCATION_TOKEN: RefCell<Option<usize>> = RefCell::new(None);
}

type TokenRegistry = Vector<Option<&'static [(&'static str, &'static str)]>>;

// Holds the token registry, which maps allocation tokens to a set of static tags that describe who
// or what the allocations tied to that token belong to.
static TOKEN_REGISTRY: ArcSwapOption<TokenRegistry> = ArcSwapOption::const_empty();

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

    /// Registers an allocation group.
    pub fn register() -> AllocationToken {
        let mut id = 0;
        TOKEN_REGISTRY.rcu(|registry| {
            let mut registry = registry
                .as_ref()
                .map(|inner| inner.as_ref().clone())
                .unwrap_or_default();

            id = registry.len();
            registry.push_back(None);
            Some(Arc::new(registry))
        });

        AllocationToken(id)
    }

    /// Registers an allocation group, with tags.
    ///
    /// While allocation groups are primarily represented by their [`AllocationToken`], users can
    /// use this method to attach specific tags -- or key/value string pairs -- to the group.  These
    /// tags will be provided to the global tracker whenever an allocation event is associated with
    /// the allocation group.
    ///
    /// ## Memory usage
    ///
    /// In order to minimize any allocations while in the process of tracking normal allocations, we
    /// rely on utilizing only static references to data.  If the tags given are not already
    /// `'static` references, they will be leaked in order to make them `'static`.  Thus, callers
    /// should take care to avoid registering tokens on an ongoing basis when utilizing owned tags as
    /// this could create a persistent and ever-growing memory leak over the life of the process.
    pub fn register_with_tags<I, K, V>(tags: I) -> AllocationToken
    where
        I: IntoIterator,
        I::Item: AsRef<(K, V)>,
        K: Into<Cow<'static, str>> + Clone,
        V: Into<Cow<'static, str>> + Clone,
    {
        let tags = tags
            .into_iter()
            .map(|tags| {
                let (k, v) = tags.as_ref();
                let sk = match k.clone().into() {
                    Cow::Borrowed(rs) => rs,
                    Cow::Owned(os) => Box::leak(os.into_boxed_str()),
                };

                let sv = match v.clone().into() {
                    Cow::Borrowed(rs) => rs,
                    Cow::Owned(os) => Box::leak(os.into_boxed_str()),
                };

                (sk, sv)
            })
            .collect::<Vec<_>>();
        let tags = &*Box::leak(tags.into_boxed_slice());

        let mut id = 0;
        TOKEN_REGISTRY.rcu(|registry| {
            let mut registry = registry
                .as_ref()
                .map(|inner| inner.as_ref().clone())
                .unwrap_or_default();

            id = registry.len();
            registry.push_back(Some(tags));
            Some(Arc::new(registry))
        });

        AllocationToken(id)
    }
}

/// A token that uniquely identifies an allocation group.
pub struct AllocationToken(usize);

impl AllocationToken {
    /// Marks the associated allocation group as the active allocation group on this thread.
    ///
    /// If another allocation group is currently active, it is replaced, and restored either when
    /// this allocation guard is dropped, or when [`AllocationGuard::exit`] is called.
    pub fn enter(self) -> AllocationGuard {
        AllocationGuard::enter(self)
    }
}

/// Guard that updates the current thread to track allocations for the associated allocation group.
///
/// ## Drop behavior
///
/// This guard has a [`Drop`] implementation that resets the active allocation group back to the
/// previous allocation group.
///
/// ## Moving across threads
///
/// [`AllocationGuard`] is specifically marked as `!Send` as the active allocation group is tracked
/// at a per-thread level.  If you acquire an `AllocationGuard` and need to resume computation on
/// another thread, such as across an await point or when simply sending objects to another thread,
/// you must first [`exit`][exit] the guard and move the resulting [`AllocationToken`].  Once on the
/// new thread, you can then reacquire the guard.
///
/// [exit]: AllocationGuard::exit
pub struct AllocationGuard {
    previous: Option<usize>,
    current: usize,

    /// ```compile_fail
    /// use tracking_allocator::AllocationGuard;
    /// trait AssertSend: Send {}
    ///
    /// impl AssertSend for AllocationGuard {}
    /// ```
    _ns: PhantomNotSend,
}

impl AllocationGuard {
    pub(crate) fn enter(token: AllocationToken) -> AllocationGuard {
        let id = token.0;

        // Set the current allocation token to the new token, keeping the previous.
        let previous = CURRENT_ALLOCATION_TOKEN.with(|current| current.replace(Some(id)));

        AllocationGuard {
            previous,
            current: id,
            _ns: PhantomNotSend::default(),
        }
    }

    /// Unmarks this allocation group as the active allocation group on this thread, resetting the
    /// active allocation group to the previous value.
    pub fn exit(mut self) -> AllocationToken {
        self.exit_inner()
    }

    fn exit_inner(&mut self) -> AllocationToken {
        // Reset the current allocation token to the previous one.
        CURRENT_ALLOCATION_TOKEN.with(|current| {
            *current.borrow_mut() = self.previous;
        });

        AllocationToken(self.current)
    }
}

impl Drop for AllocationGuard {
    fn drop(&mut self) {
        let _ = self.exit_inner();
    }
}

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

struct AllocationGroup {
    id: usize,
    tags: Option<&'static [(&'static str, &'static str)]>,
}

#[inline(always)]
fn get_active_allocation_group() -> Option<AllocationGroup> {
    // See if there's an active allocation token on this thread.
    CURRENT_ALLOCATION_TOKEN
        .with(|current| *current.borrow())
        .and_then(|id| {
            // Try and grab the tags from the registry.  This shouldn't ever failed since we wrap
            // registry IDs in AllocationToken which only we can create.
            let registry_guard = TOKEN_REGISTRY.load();
            let registry = registry_guard
                .as_ref()
                .expect("allocation token cannot be set unless registry has been created");
            registry
                .get(id)
                .copied()
                .map(|tags| AllocationGroup { id, tags })
        })
}

fn track_allocation(addr: usize, size: usize) {
    if let Some(tracker) = get_global_tracker() {
        if let Some(group) = get_active_allocation_group() {
            tracker.allocated(addr, size, group.id, group.tags)
        }
    }
}

fn track_deallocation(addr: usize) {
    if let Some(tracker) = get_global_tracker() {
        tracker.deallocated(addr)
    }
}
