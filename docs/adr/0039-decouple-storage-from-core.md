# ADR-0039: Decouple Storage from Core

**Status:** Proposed
**Date:** 2026-05-25
**Deciders:** AletheiaDB Core Team
**Categories:** architecture, storage, core

## Context

Circular dependencies were causing build failures and hindering modular development. The core logic was tightly coupled with storage implementation details.

## Decision

Move persistence logic to a dedicated `storage` crate.

## Consequences

*   **Build times improve**: Changes to storage internals do not force recompilation of the entire core.
*   **FFI complexity increases**: The interface between core and storage must be strictly defined, potentially complicating cross-language bindings.
