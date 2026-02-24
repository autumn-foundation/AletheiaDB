# ADR-0052: Decouple Storage from Core

**Status:** Proposed
**Date:** 2026-05-25
**Deciders:** Atlas, Codex
**Categories:** architecture, storage, core

## Context

Circular dependencies between `core` and `storage` modules were causing build failures and hindering development velocity. Any change in the storage layer triggered a rebuild of the entire core, slowing down iteration cycles. Additionally, testing core logic required mocking complex storage interactions, leading to fragile tests.

## Decision

We will **move all persistence logic to a dedicated `storage` module**, separate from the `core` domain logic.

The architectural boundary will be enforced by:
1.  Defining clear interfaces (traits) in `core` that `storage` implements (or vice-versa depending on direction, but typically Core defines domain and Storage implements persistence).
2.  Ensuring `core` does not depend on concrete `storage` implementation details.
3.  Removing any circular references between `Node`/`Edge` definitions and their serialization formats.

## Consequences

### Positive

-   **Build Times:** Significant improvement as storage changes no longer force core recompilation.
-   **Testability:** Core logic can be tested in isolation using lightweight mocks.
-   **Modularity:** Clear separation of concerns facilitates easier maintenance and future refactoring.

### Negative

-   **FFI Complexity:** Separating storage might increase complexity if we expose FFI bindings later.
-   **Indirection:** Requires careful management of trait boundaries or dependency injection.

## References

-   `docs/ARCHITECTURE.md` (Updated diagrams)
