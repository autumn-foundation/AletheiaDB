# ADR-0055: Unwinding API/Core Dependency

**Status:** Proposed
**Date:** 2026-05-24
**Deciders:** Atlas, Codex
**Categories:** architecture, layering, api, core

## Context

The `db` module imported `TxIdGenerator` indirectly through an `api::transaction` re-export. This created a structural anomaly and layering violation where `db` skipped importing a core domain primitive from `core`, choosing instead to rely on a convenience re-export provided by the intermediate `api` layer.

This coupling artificially entangled `api`'s transaction handling responsibilities with `core`'s foundational ID generation mechanics. Any change to `core::id::TxIdGenerator` would ripple unnecessarily through the `api` module, preventing clear architectural separation.

## Decision

We removed the `TxIdGenerator` re-export from `api::transaction::types` and `api::transaction::mod`. We updated the `db` module (`db::mod`, `db::config`) and all transaction tests to explicitly import `TxIdGenerator` directly from `core::id::TxIdGenerator`.

## Consequences

### Positive

-   **Layering Enforcement:** Ensures that higher-level modules (`db`) directly import foundational types from the appropriate layer (`core`), rather than depending on transitive re-exports.
-   **Reduced Coupling:** `api` is no longer unnecessarily bound to `core`'s ID generation responsibilities for other modules.

### Negative

-   **Explicit Imports:** Callers must now explicitly import `TxIdGenerator` from `core`, marginally increasing the verbosity of imports in files setting up transactions.

### Neutral

-   The internal mechanics of `TxIdGenerator` remain unchanged; only its accessibility across module boundaries was adjusted.
