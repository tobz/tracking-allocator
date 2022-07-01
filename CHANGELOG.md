# Changelog
All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

<!-- next-header -->

## [Unreleased] - ReleaseDate

## [0.4.0] - 2022-07-01

## Changed

- `AllocationTracker::allocated` and `AllocationTracker::deallocated` now both get the size of the requested allocation,
  as well as the size of the wrapped allocation that `tracking-allocator` performs to store the group Id of the
  allocation owner. This allows users to understand the true size of allocations being performed while also
  understanding the allocations their applications are requesting.

## [0.3.0] - 2022-05-12

### Changed

- `AllocationGroupToken::enter` no longer consumes itself, and `AllocationGuard` is now bound by the lifetime of the
  token. This should make it a generally more flexible and useful primitive, as the group can be entered purely with
  mutable access to the token, such as if it is stored in something like a `Mutex<T>`.

### Fixed

- Reworked the logic around tracking the active allocation group as it had an edge case when used with `tracing::Span`
  that lead to a panic if a span was cloned/entered multiple times on the same thread. This is a common scenario for
  asynchronous applications that spawn multiple copies of a task to process work in parallel, which all share the same span.

## [0.2.0] - 2022-05-11

A big thanks to [@jswrenn](https://github.com/jswrenn) for their help on much of the newly-redesigned parts of the
crate, including the inline allocation metadata and reentrancy protection logic.

### Added

- New method `AllocationRegistry::untracked` that allows running a closure in a way where (de)allocations will not be
  tracked at all, which can be used by implementors in order to build or update data structures related to handling
  allocation events outside of the hot path.
- A new type, `AllocationGroupId`, that replaces the raw `usize` that was previously used for passing back the
  allocation group ID.

### Changed

- Updated to `0.3.x` for `tracing-subscriber`.
- Refactored the core concept of having a token registry at all, and switched entirely to monotonic token generation.
- Refactored the logic of entering/exiting the allocation group to entirely avoid reentrancy during calls to
  `AllocationTracker::allocated` and `AllocationTracker::deallocate`.
- Tags can no longer be registered with an allocation group, and thus `AllocationTracker::allocate` no longer has a
  `tags` parameter.
- The original allocation group is now tracked inline with the allocation, so `AllocationTracker::deallocate` now
  reports the group that originally acquire the allocation, the current group where the deallocation is occurring, and
  the size of the allocation.

## [0.1.2] - 2021-10-04

### Added

- Ability to specify a custom allocator to wrap around instead of always using the system allocator.

## [0.1.1] - 2021-10-04

### Added

- Support for entering/exiting allocation groups by attaching them to `tracing::Span`.

## [0.1.0] - 2021-10-03

### Added

- Initial commit.
