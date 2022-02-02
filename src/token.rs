use std::{
    borrow::Cow,
    cell::RefCell,
    mem,
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc,
    },
};

use crate::util::PhantomNotSend;

type GroupTags = &'static [(&'static str, &'static str)];
type TokenRegistry = Vec<Option<GroupTags>>;

// Holds the token registry, which maps allocation tokens to a set of static tags that describe who
// or what the allocations tied to that token belong to.
static TOKEN_REGISTRY: AtomicUsize = AtomicUsize::new(0);
static TOKEN_REGISTRY_STATE: AtomicUsize = AtomicUsize::new(0);

const REGISTRY_STABLE: usize = 0;
const REGISTRY_MODIFYING: usize = 1;

thread_local! {
    /// The currently executing allocation token.
    ///
    /// Any allocations which occur on this thread will be associated with whichever token is
    /// present at the time of the allocation.
    static CURRENT_ALLOCATION_TOKEN: RefCell<Option<AllocationGroupId>> = RefCell::new(None);

    // Whether or not a token registry update is in progress.
    //
    // We track this to avoid reentrancy problems with allocations during the update.
    static TOKEN_REGISTRY_RCU_IN_FLIGHT: AtomicBool = AtomicBool::new(false);
}

fn get_token_registry() -> Option<Arc<TokenRegistry>> {
    unsafe {
        let addr = TOKEN_REGISTRY.load(Ordering::Acquire);
        if addr == 0 {
            None
        } else {
            // Reconstruct the `Arc<TokenRegistry>` behind this address, and clone it.  Crucially,
            // we have to forget about the reconstructed `Arc<TokenRegistry>` because we don't want
            // the drop destructor to run prematurely.
            let inner = Arc::from_raw(addr as *const TokenRegistry);
            let new = Arc::clone(&inner);
            mem::forget(inner);
            Some(new)
        }
    }
}

fn register_token<K, V>(tags: Option<&[(K, V)]>) -> usize
where
    K: Into<Cow<'static, str>> + Clone,
    V: Into<Cow<'static, str>> + Clone,
{
    TOKEN_REGISTRY_RCU_IN_FLIGHT.with(|flag| {
        flag.store(true, Ordering::Release);

        let tags = tags.map(|tags| {
            let tags = tags
                .iter()
                .map(|tag| {
                    let (k, v) = tag;
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
            &*Box::leak(tags.into_boxed_slice())
        });

        // First, take ownership of modifying the token registry global pointer:
        while TOKEN_REGISTRY_STATE
            .compare_exchange_weak(
                REGISTRY_STABLE,
                REGISTRY_MODIFYING,
                Ordering::SeqCst,
                Ordering::SeqCst,
            )
            .is_err()
        {}

        // Now we can clone the existing registry and build the new token:
        let mut new_registry = get_token_registry()
            .map(|r| r.as_ref().clone())
            .unwrap_or_default();

        let id = new_registry.len();
        new_registry.push(tags);

        // Now we need to wrap the new registry up and store it:
        let new_ptr = Arc::into_raw(Arc::new(new_registry));
        let old_addr = TOKEN_REGISTRY.swap(new_ptr as usize, Ordering::SeqCst);

        // We also reconstruct the old registry (via `Arc<TokenRegistry>`) so that we can drop it,
        // ensuring that the memory is cleaned up whenever the last reference to the old registry
        // goes out of scope, which may literally be this one.
        let old_arc_ptr = old_addr as *const TokenRegistry;
        if !old_arc_ptr.is_null() {
            let old_arc = unsafe { Arc::from_raw(old_arc_ptr) };
            drop(old_arc);
        }

        // And release our ownership for modifying the token registry global pointer:
        TOKEN_REGISTRY_STATE.store(REGISTRY_STABLE, Ordering::SeqCst);

        flag.store(false, Ordering::Release);

        id
    })
}

#[inline]
pub(crate) fn within_token_registry_rcu() -> bool {
    TOKEN_REGISTRY_RCU_IN_FLIGHT.with(|flag| flag.load(Ordering::Acquire))
}

/// The identifier that uniquely identifiers an allocation group.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct AllocationGroupId(usize);

/// A token that uniquely identifies an allocation group.
///
/// Allocation groups are the core grouping mechanism of `tracking-allocator` and drive much of its
/// behavior.  While the allocator must be overridden, and a global track provided, no allocations
/// are tracked unless a group is associated with the current thread making the allocation.
///
/// Practically speaking, allocation groups are simply an internal identifier that is used to
/// identify the "owner" of an allocation.  Additional tags can be provided when acquire an
/// allocation group token, which is provided to [`AllocationTracker`][crate::AllocationTracker]
/// whenever an allocation occurs.
///
/// ## Usage
///
/// In order for an allocation group to be attached to an allocation, its must be "entered."
/// [`AllocationGroupToken`] functions similarly to something like a mutex, where "entering" the
/// token conumes the token and provides a guard: [`AllocationGuard`].  This guard is tied to the
/// allocation group being active: if the guard is dropped, or if it is exited manually, the
/// allocation group is no longer active.
///
/// [`AllocationGuard`] also tracks if another allocation group was active prior to entering, and
/// ensures it is set back as the active allocation group when the guard is dropped.  This allows
/// allocation groups to be nested within each other.
pub struct AllocationGroupToken(AllocationGroupId);

impl AllocationGroupToken {
    /// Acquires an allocation group token.
    pub fn acquire() -> AllocationGroupToken {
        let id = register_token::<String, String>(None);
        AllocationGroupToken(AllocationGroupId(id))
    }

    /// The ID associated with this allocation group.
    pub fn id(&self) -> AllocationGroupId {
        self.0.clone()
    }

    /// Acquires an allocation group token, with tags.
    ///
    /// While allocation groups are primarily represented by their [`AllocationGroupToken`], users can
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
    pub fn acquire_with_tags<K, V>(tags: &[(K, V)]) -> AllocationGroupToken
    where
        K: Into<Cow<'static, str>> + Clone,
        V: Into<Cow<'static, str>> + Clone,
    {
        let id = register_token(Some(tags));
        AllocationGroupToken(AllocationGroupId(id))
    }

    #[cfg(feature = "tracing-compat")]
    pub(crate) fn into_unsafe(self) -> UnsafeAllocationGroupToken {
        UnsafeAllocationGroupToken::new(self.0)
    }

    /// Marks the associated allocation group as the active allocation group on this thread.
    ///
    /// If another allocation group is currently active, it is replaced, and restored either when
    /// this allocation guard is dropped, or when [`AllocationGuard::exit`] is called.
    pub fn enter(self) -> AllocationGuard {
        AllocationGuard::enter(self)
    }
}

#[cfg(feature = "tracing-compat")]
#[cfg_attr(docsrs, doc(cfg(feature = "tracing-compat")))]
impl AllocationGroupToken {
    /// Attaches this allocation group to a tracing [`Span`][tracing::Span].
    ///
    /// When the span is entered or exited, the allocation group will also transition from idle to
    /// active, or active to idle.  In effect, all allocations that occur while the span is entered
    /// will be associated with the allocation group.
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
    Active(Option<AllocationGroupId>),
}

impl GuardState {
    fn transition_to_active(&mut self) {
        let new_state = match self {
            Self::Idle(id) => {
                // Set the current allocation token to the new token, keeping the previous.
                let previous =
                    CURRENT_ALLOCATION_TOKEN.with(|current| current.replace(Some(id.clone())));
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
                let current = CURRENT_ALLOCATION_TOKEN.with(|current| {
                    let old = mem::replace(&mut *current.borrow_mut(), previous.take());
                    old.expect("transitioned to idle state with empty CURRENT_ALLOCATION_TOKEN")
                });
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
/// This guard has a [`Drop`] implementation that resets the active allocation group back to the
/// previous allocation group.  Calling `exit` is generally preferred for being explicit about when
/// the allocation group begins and ends, though.
///
/// ## Moving across threads
///
/// [`AllocationGuard`] is specifically marked as `!Send` as the active allocation group is tracked
/// at a per-thread level.  If you acquire an `AllocationGuard` and need to resume computation on
/// another thread, such as across an await point or when simply sending objects to another thread,
/// you must first [`exit`][exit] the guard and move the resulting [`AllocationGroupToken`].  Once
/// on the new thread, you can then reacquire the guard.
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

    /// Unmarks this allocation group as the active allocation group on this thread, resetting the
    /// active allocation group to the previous value.
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
/// While normally users would work directly with [`AllocationGroupToken`] and [`AllocationGuard`],
/// we cannot store [`AllocationGuard`] in span data as it is `!Send`, and tracing spans can be sent
/// across threads.
///
/// However, `tracing` itself employs a guard for entering spans.  The guard is `!Send`, which
/// ensures that the guard cannot be sent across threads.  Since the same guard is used to know when
/// a span has been exited, `tracing` ensures that between a span being entered and exited, it
/// cannot move threads.
///
/// Thus, we build off of that invariant, and use this stripped down token to manually enter and
/// exit the allocation group in a specialized `tracing_subscriber` layer that we control.
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

    /// Marks the associated allocation group as the active allocation group on this thread.
    ///
    /// If another allocation group is currently active, it is replaced, and restored either when
    /// this allocation guard is dropped, or when [`AllocationGuard::exit`] is called.
    ///
    /// Functionally equivalent to [`AllocationGroupToken::enter`].
    pub fn enter(&mut self) {
        self.state.transition_to_active();
    }

    /// Unmarks this allocation group as the active allocation group on this thread, resetting the
    /// active allocation group to the previous value.
    ///
    /// Functionally equivalent to [`AllocationGuard::exit`].
    pub fn exit(&mut self) {
        let _ = self.state.transition_to_idle();
    }
}

pub(crate) struct AllocationGroupMetadata {
    id: AllocationGroupId,
    tags: Option<GroupTags>,
}

impl AllocationGroupMetadata {
    pub fn id(&self) -> AllocationGroupId {
        self.id.clone()
    }

    pub fn tags(&self) -> Option<GroupTags> {
        self.tags
    }
}

/// Gets the current allocation group, if one is active, and any metadata associated with it.
#[inline(always)]
pub(crate) fn get_active_allocation_group() -> Option<AllocationGroupMetadata> {
    // See if there's an active allocation token on this thread.
    CURRENT_ALLOCATION_TOKEN
        .with(|current| current.borrow().clone())
        .and_then(|id| {
            // Try and grab the tags from the registry.  This shouldn't ever failed since we wrap
            // registry IDs in AllocationToken which only we can create.
            let registry = get_token_registry();
            registry.map(|registry| {
                let tags = registry.get(id.0).copied().flatten();
                AllocationGroupMetadata { id, tags }
            })
        })
}
