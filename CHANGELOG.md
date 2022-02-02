# Changelog
All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

<!-- next-header -->

## [Unreleased] - ReleaseDate

### Changed
- Updated to `0.3.x` for `tracing-subscriber`.
- Refactored the token registry to fix an issue with `arc-swap` needing to allocate on the read
  path, which caused reentrancy during allocation tracking.

## [0.1.2] - 2021-10-04

### Added
- Ability to specify a custom allocator to wrap around instead of always using the system allocator.

## [0.1.1] - 2021-10-04

### Added
- Support for entering/exiting allocation groups by attaching them to `tracing::Span`.

## [0.1.0] - 2021-10-03

### Added
- Initial commit.
