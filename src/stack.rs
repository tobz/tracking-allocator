use crate::{AllocationGroupId, AllocationRegistry};

/// An allocation group stack.
///
/// As allocation groups are entered and exited, they naturally end up looking a lot like a stack itself: the active
/// allocation group gets added to the stack when entered, and if another allocation group is entered before the
/// previous is exited, the newer group is added to the stack above the previous one, and so on and so forth.
///
/// This implementation is an incredibly thin wrapper around `Vec<T>` which already provides the necessary "push" and
/// "pop" methods required for a stack. Our logic is slightly tweaked to account for the expectation that a there should
/// never be a pop without a corresponding push, and so on.
pub struct GroupStack {
    slots: Vec<AllocationGroupId>,
}

impl GroupStack {
    /// Creates an empty [`GroupStack`].
    pub const fn new() -> Self {
        Self { slots: Vec::new() }
    }

    /// Gets the currently active allocation group.
    ///
    /// If the stack is empty, then the root allocation group is the defacto active allocation group, and is returned as such.
    pub fn current(&self) -> AllocationGroupId {
        if self.slots.is_empty() {
            AllocationGroupId::ROOT
        } else {
            self.slots
                .last()
                .cloned()
                .expect("self.slots cannot be empty")
        }
    }

    /// Pushes an allocation group on to the stack, marking it as the active allocation group.
    pub fn push(&mut self, group: AllocationGroupId) {
        if self.slots.len() == self.slots.capacity() {
            // Make sure we don't track this allocation, which reentrancy protection would do correctly, but we're just
            // optimizing a little bit here for maximum speeeeeeeed.
            AllocationRegistry::untracked(|| {
                // Why 64, you ask? 64 should be more than enough for literally any normal operational profile, which
                // means we'll allocate _once_ per thread.. and then hopefully never again. If we have to allocate
                // again, well, that stinks... but the depth of nested allocation groups should be fairly low in nearly
                // all cases, low enough to fit into the first 64-element resize that we do.
                self.slots.reserve(64);
            });
        }

        self.slots.push(group);
    }

    /// Pops the currently active allocation group off the stack.
    pub fn pop(&mut self) -> AllocationGroupId {
        self.slots
            .pop()
            .expect("pop should not be callable when group stack is empty")
    }
}
