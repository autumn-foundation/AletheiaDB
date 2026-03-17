# ADR-0055: Refactor Temporal Vector Index Module

**Status:** Accepted
**Date:** 2026-01-29
**Deciders:** AletheiaDB Core Team
**Categories:** architecture, modularity, index

## Context

The `src/index/vector/temporal.rs` file had grown into a "Blob" module of over 1400 lines. It mixed various concerns including configuration, snapshot logic, statistics, index observation, and the core temporal vector index implementation. This mixed-responsibility design made the code difficult to navigate, read, and maintain, violating the Single Responsibility Principle and reducing overall codebase transparency.

## Decision

We have decided to split the monolithic `src/index/vector/temporal.rs` file into a dedicated `src/index/vector/temporal/` module directory.

The module is decomposed into cohesive, single-responsibility files:
- `config.rs`: Extracts configuration structures.
- `snapshot.rs`: Extracts internal snapshot logic.
- `stats.rs`: Extracts statistics and metrics tracking.
- `observer.rs`: Extracts observer pattern implementations for the index.
- `mod.rs`: Retains the core logic and tests (for now), serving as the facade for the module and significantly reducing noise.

## Consequences

### Positive

- **Improved Readability:** Separating distinct concerns into dedicated files makes it easier for developers to find and comprehend specific logic.
- **Maintainability:** Smaller, focused files are easier to test and modify without causing unintended side effects in unrelated areas.
- **Clearer Boundaries:** Enforces structural boundaries between configuration, statistics, and core index operations.

### Negative

- **Initial Refactoring Overhead:** Moving code across files and updating imports requires a one-time effort.
- **File Proliferation:** Increases the total number of files in the repository, though they are logically grouped.

### Neutral

- Tests remain in `mod.rs` for now, meaning the core file is still relatively large, though the primary source of noise (config, stats, snapshot) has been eliminated.
