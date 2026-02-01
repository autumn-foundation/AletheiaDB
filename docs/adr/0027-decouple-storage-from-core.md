# Decouple Storage from Core

**Status:** Proposed
**Date:** 2026-01-27

## Context

Circular dependencies were causing build failures and making it difficult to refactor the codebase. The `core` module depended on `storage` for persistence, while `storage` depended on `core` for domain types.

## Decision

Move persistence logic to a dedicated crate (or module `storage` decoupled from `core`). The core will define traits that storage implements.

## Consequences

Build times improve, but FFI complexity increases due to the separation of concerns and the need for trait boundaries.
