use std::{borrow::Cow, cell::RefCell, sync::Arc};

use arc_swap::ArcSwapOption;
use im::Vector;

use crate::util::PhantomNotSend;

type GroupTags = &'static [(&'static str, &'static str)];
type TokenRegistry = Vector<Option<GroupTags>>;

// Holds the token registry, which maps allocation tokens to a set of static tags that describe who
// or what the allocations tied to that token belong to.
static TOKEN_REGISTRY: ArcSwapOption<TokenRegistry> = ArcSwapOption::const_empty();

thread_local! {
    /// The currently executing allocation token.
    ///
    /// Any allocations which occur on this thread will be associated with whichever token is
    /// present at the time of the allocation.
    static CURRENT_ALLOCATION_TOKEN: RefCell<Option<usize>> = RefCell::new(None);
}

/// A token that uniquely identifies an allocation group.
pub struct AllocationGroupToken(usize);

impl AllocationGroupToken {
    /// Acquires an allocation group token.
    pub fn acquire() -> AllocationGroupToken {
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

        AllocationGroupToken(id)
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
    pub fn acquire_with_tags<I, K, V>(tags: I) -> AllocationGroupToken
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

        AllocationGroupToken(id)
    }

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
    pub(crate) fn enter(token: AllocationGroupToken) -> AllocationGuard {
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
    pub fn exit(mut self) -> AllocationGroupToken {
        self.exit_inner()
    }

    fn exit_inner(&mut self) -> AllocationGroupToken {
        // Reset the current allocation token to the previous one.
        CURRENT_ALLOCATION_TOKEN.with(|current| {
            *current.borrow_mut() = self.previous;
        });

        AllocationGroupToken(self.current)
    }
}

impl Drop for AllocationGuard {
    fn drop(&mut self) {
        let _ = self.exit_inner();
    }
}

pub(crate) struct AllocationGroupMetadata {
    id: usize,
    tags: Option<GroupTags>,
}

impl AllocationGroupMetadata {
    pub fn id(&self) -> usize {
        self.id
    }

    pub fn tags(&self) -> Option<GroupTags> {
        self.tags
    }
}

#[inline(always)]
pub(crate) fn get_active_allocation_group() -> Option<AllocationGroupMetadata> {
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
                .map(|tags| AllocationGroupMetadata { id, tags })
        })
}