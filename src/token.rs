use std::{
    cell::RefCell,
    mem,
    num::NonZeroUsize,
    sync::atomic::{AtomicUsize, Ordering},
};

use crate::util::PhantomNotSend;

thread_local! {
    /// The currently executing allocation token.
    ///
    /// Any allocations which occur on this thread will be associated with whichever token is
    /// present at the time of the allocation.
    pub (crate) static CURRENT_ALLOCATION_TOKEN: RefCell<AllocationGroupId> =
        RefCell::new(AllocationGroupId::ROOT);
}

/// The identifier that uniquely identifiers an allocation group.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct AllocationGroupId(NonZeroUsize);

impl AllocationGroupId {
    /// Attempts to create an `AllocationGroupId` from a raw `usize`.
    ///
    /// If the raw value is zero, `None` is returned.
    pub(crate) fn from_raw(id: usize) -> Option<Self> {
        NonZeroUsize::new(id).map(Self)
    }
}

impl AllocationGroupId {
    /// The group ID used for allocations which are not made within a registered allocation group.
    pub const ROOT: Self = Self(unsafe { NonZeroUsize::new_unchecked(1) });

    /// Gets the integer representation of this group ID.
    pub const fn as_usize(&self) -> NonZeroUsize {
        self.0
    }

    fn register() -> Option<AllocationGroupId> {
        static GROUP_ID: AtomicUsize = AtomicUsize::new(AllocationGroupId::ROOT.0.get() + 1);
        static HIGHEST_GROUP_ID: AtomicUsize =
            AtomicUsize::new(AllocationGroupId::ROOT.0.get() + 1);

        let group_id = GROUP_ID.fetch_add(1, Ordering::Relaxed);
        let highest_group_id = HIGHEST_GROUP_ID.fetch_max(group_id, Ordering::AcqRel);

        if group_id >= highest_group_id {
            let group_id = NonZeroUsize::new(group_id).expect("bug: GROUP_ID overflowed");
            Some(AllocationGroupId(group_id))
        } else {
            None
        }
    }
}

/// A token that allows controlling when an allocation group is active or inactive.
///
/// Allocation groups represent the core mechanism for categorizing allocation activity, where the group must be active
/// for (de)allocation events to be attributed to it. Practically speaking, allocation groups are simply an internal
/// identifier that is used to identify the "owner" of an allocation.
///
/// ## Usage
///
/// In order for an allocation group to be attached to an allocation, it must be "entered." [`AllocationGroupToken`]
/// functions similarly to something like a mutex, where "entering" the token conumes the token and provides a guard:
/// [`AllocationGuard`].  This guard is tied to the allocation group being active: if the guard is dropped, or if it is
/// exited manually, the allocation group is no longer active.
///
/// [`AllocationGuard`] also tracks if another allocation group was active prior to entering, and ensures it is set back
/// as the active allocation group when the guard is dropped.  This allows allocation groups to be nested within each
/// other.
pub struct AllocationGroupToken(AllocationGroupId);

impl AllocationGroupToken {
    /// Registers an allocation group token.
    /// 
    /// Allocation groups use an internal identifier that is incremented atomically, and monotonically, when
    /// registration occurs.  This identifier, thus, has a limit based on the pointer size of the architecture. In other
    /// words, on 32-bit systems, a limit of 2^32 allocation groups can be registered before this identifier space is
    /// exhausted.  On 64-bit systems, this limit is 2^64.
    /// 
    /// If the number of registered allocation groups exceeds the limit, `None` is returned. This is a permanent state
    /// until the application exits. Otherwise, `Some` is returned.
    pub fn register() -> Option<AllocationGroupToken> {
        AllocationGroupId::register().map(AllocationGroupToken)
    }

    /// Gets the ID associated with this allocation group.
    pub fn id(&self) -> AllocationGroupId {
        self.0.clone()
    }

    #[cfg(feature = "tracing-compat")]
    pub(crate) fn into_unsafe(self) -> UnsafeAllocationGroupToken {
        UnsafeAllocationGroupToken::new(self.0)
    }

    /// Enters the allocation group, marking it as the active allocation group on this thread.
    ///
    /// If another allocation group is currently active, it is replaced, and restored either when this allocation guard
    /// is dropped, or when [`AllocationGuard::exit`] is called.
    pub fn enter(self) -> AllocationGuard {
        AllocationGuard::enter(self)
    }
}

#[cfg(feature = "tracing-compat")]
#[cfg_attr(docsrs, doc(cfg(feature = "tracing-compat")))]
impl AllocationGroupToken {
    /// Attaches this allocation group to a tracing [`Span`][tracing::Span].
    ///
    /// When the span is entered or exited, the allocation group will also transition from inactive to active, and vise
    /// versa.  In effect, all allocations that occur while the span is entered will be associated with the allocation
    /// group.
    pub fn attach_to_span(self, span: &tracing::Span) {
        use crate::tracing::WithAllocationGroup;

        let mut unsafe_token = Some(self.into_unsafe());

        tracing::dispatcher::get_default(move |dispatch| {
            if let Some(id) = span.id() {
                if let Some(ctx) = dispatch.downcast_ref::<WithAllocationGroup>() {
                    let unsafe_token = unsafe_token.take().expect("token already consumed");
                    ctx.with_allocation_group(dispatch, &id, unsafe_token);
                }
            }
        });
    }
}

enum GuardState {
    // Guard is idle.  We aren't the active allocation group.
    Idle(AllocationGroupId),

    // Guard is active.  We're the active allocation group, so we hold on to the previous
    // allocation group ID, if there was one, so we can switch back to it when we transition to
    // being idle.
    Active(AllocationGroupId),
}

impl GuardState {
    fn transition_to_active(&mut self) {
        let new_state = match self {
            Self::Idle(id) => {
                // Set the current allocation token to the new token, keeping the previous.
                let previous = CURRENT_ALLOCATION_TOKEN.with(|current| current.replace(id.clone()));
                Self::Active(previous)
            }
            Self::Active(ref previous) => {
                let current = CURRENT_ALLOCATION_TOKEN.with(|current| current.borrow().clone());
                panic!(
                    "tid {:?}: transitioning active->active is invalid; current={:?} previous={:?}",
                    std::thread::current().id(),
                    current,
                    previous
                );
            }
        };
        *self = new_state;
    }

    fn transition_to_idle(&mut self) -> AllocationGroupId {
        match self.try_transition_to_idle() {
            None => panic!(
                "tid {:?}: transitioning idle->idle is invalid",
                std::thread::current().id()
            ),
            Some(id) => id,
        }
    }

    fn try_transition_to_idle(&mut self) -> Option<AllocationGroupId> {
        let (id, new_state) = match self {
            Self::Idle(_) => return None,
            Self::Active(previous) => {
                // Reset the current allocation token to the previous one:
                let current = CURRENT_ALLOCATION_TOKEN
                    .with(|current| mem::replace(&mut *current.borrow_mut(), previous.clone()));
                (Some(current.clone()), Self::Idle(current))
            }
        };
        *self = new_state;
        id
    }
}

/// Guard that updates the current thread to track allocations for the associated allocation group.
///
/// ## Drop behavior
///
/// This guard has a [`Drop`] implementation that resets the active allocation group back to the previous allocation
/// group.  Calling [`exit`][exit] is generally preferred for being explicit about when the allocation group begins and
/// ends, though.
///
/// ## Moving across threads
///
/// [`AllocationGuard`] is specifically marked as `!Send` as the active allocation group is tracked at a per-thread
/// level.  If you acquire an `AllocationGuard` and need to resume computation on another thread, such as across an
/// await point or when simply sending objects to another thread, you must first [`exit`][exit] the guard and move the
/// resulting [`AllocationGroupToken`].  Once on the new thread, you can then reacquire the guard.
///
/// [exit]: AllocationGuard::exit
pub struct AllocationGuard {
    state: GuardState,

    /// ```compile_fail
    /// use tracking_allocator::AllocationGuard;
    /// trait AssertSend: Send {}
    ///
    /// impl AssertSend for AllocationGuard {}
    /// ```
    _ns: PhantomNotSend,
}

impl AllocationGuard {
    pub(crate) fn enter(token: AllocationGroupToken) -> AllocationGuard {
        let mut state = GuardState::Idle(token.0);
        state.transition_to_active();

        AllocationGuard {
            state,
            _ns: PhantomNotSend::default(),
        }
    }

    /// Exits the allocation group, restoring the previously active allocation group on this thread.
    pub fn exit(mut self) -> AllocationGroupToken {
        // Reset the current allocation token to the previous one.
        let current = self.state.transition_to_idle();

        AllocationGroupToken(current)
    }
}

impl Drop for AllocationGuard {
    fn drop(&mut self) {
        let _ = self.state.try_transition_to_idle();
    }
}

/// Unmanaged allocation group token used specifically with `tracing`.
///
/// ## Safety
///
/// While users would normally work directly with [`AllocationGroupToken`] and [`AllocationGuard`], we cannot store
/// [`AllocationGuard`] in span data as it is `!Send`, and tracing spans can be sent across threads.
///
/// However, `tracing` itself employs a guard for entering spans.  The guard is `!Send`, which ensures that the guard
/// cannot be sent across threads.  Since the same guard is used to know when a span has been exited, `tracing` ensures
/// that between a span being entered and exited, it cannot move threads.
///
/// Thus, we build off of that invariant, and use this stripped down token to manually enter and exit the allocation
/// group in a specialized `tracing_subscriber` layer that we control.
#[cfg(feature = "tracing-compat")]
pub(crate) struct UnsafeAllocationGroupToken {
    state: GuardState,
}

#[cfg(feature = "tracing-compat")]
impl UnsafeAllocationGroupToken {
    /// Creates a new `UnsafeAllocationGroupToken`.
    pub fn new(id: AllocationGroupId) -> Self {
        Self {
            state: GuardState::Idle(id),
        }
    }

    /// Enters the allocation group, marking it as the active allocation group on this thread.
    ///
    /// If another allocation group is currently active, it is replaced, and restored either when this allocation guard
    /// is dropped, or when [`AllocationGuard::exit`] is called.
    ///
    /// Functionally equivalent to [`AllocationGroupToken::enter`].
    pub fn enter(&mut self) {
        self.state.transition_to_active();
    }

    /// Exits the allocation group, restoring the previously active allocation group on this thread.
    ///
    /// Functionally equivalent to [`AllocationGuard::exit`].
    pub fn exit(&mut self) {
        let _ = self.state.transition_to_idle();
    }
}

/// Calls `f` after suspending the allocation group, if it was not already suspended.
///
/// If the allocation group is not currently suspended, then `f` is called, after suspending it, with a reference to the
/// suspended allocation group. If any other call to `try_with_suspended_allocation_group` happens while this method
/// call is on the stack, `f` in those calls with itself not be called.
#[inline(always)]
pub(crate) fn try_with_suspended_allocation_group<F>(f: F)
where
    F: FnOnce(AllocationGroupId),
{
    CURRENT_ALLOCATION_TOKEN.with(
        #[inline(always)]
        |current| {
            // The crux of avoiding reentrancy is `RefCell:try_borrow_mut`, which allows callers to skip trying to run
            // `f` if they cannot mutably borrow the current allocation group ID. As `try_borrow_mut` will only let one
            // mutable borrow happen at a time, the tracker logic is never reentrant.
            if let Ok(group_id) = current.try_borrow_mut() {
                f(AllocationGroupId(group_id.0));
            }
        },
    );
}

/// Calls `f` after suspending the allocation group.
///
/// In constrast to `try_with_suspended_allocation_group`, this method will always call `f` after attempting to suspend
/// the current allocation group, even if it was already suspended.
///
/// In practice, this method is primaryl useful for "run this function and don't track any allocations at all" while
/// `try_with_suspended_allocation_group` is primary useful for "run this function if nobody else is tracking
/// allocations right now".
#[inline(always)]
pub(crate) fn with_suspended_allocation_group<F, R>(f: F) -> R
where
    F: FnOnce() -> R,
{
    CURRENT_ALLOCATION_TOKEN.with(
        #[inline(always)]
        |current| {
            // The crux of avoiding reentrancy is `RefCell:try_borrow_mut`, as `try_borrow_mut` will only let one
            // mutable borrow happen at a time. As we simply want to ensure that the allocation group is suspended, we
            // don't care what the return value is: calling `try_borrow_mut` and holding on to the result until the end
            // of the scope is sufficient to either suspend the allocation group or know that it's already suspended and
            // will stay that way until we're done in this method.
            let _result = current.try_borrow_mut();
            f()
        },
    )
}
