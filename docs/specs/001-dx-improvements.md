# Spec 001: Developer Experience (DX) Improvements

**Status:** Draft
**Date:** 2026-02-02
**Author:** Vantage (Product Manager)
**Driver:** Jules (Engineer)

## 1. Problem Statement

New users encounter significant friction in the first 5 minutes of using AletheiaDB:

1.  ** opaque identifiers**: Printing a node label results in `Interned(11)` instead of the string value (e.g., "Person"). This forces users to learn internal implementation details (`GLOBAL_INTERNER`) just to debug their "Hello World" app.
2.  **Trait Import Tax**: Common operations like `db.create_node` require importing the `WriteOps` trait, which is not obvious to new Rust users who just want to use the `db` struct.
3.  **Feature Flag Confusion**: Copy-pasting examples from the README often fails because experimental features (like `nova`) are not enabled by default, leading to confusing compile errors.

## 2. User Stories

### Story 1: Human-Readable Debugging
*   **As a** Developer debugging my application,
*   **I want** `println!("{:?}", node.label)` to print `InternedString("Person")` instead of `InternedString(11)`,
*   **So that** I can verify my data without manually resolving interner IDs.

### Story 2: The "Prelude"
*   **As a** Developer starting a new project,
*   **I want** to import `use aletheiadb::prelude::*;`,
*   **So that** I have all necessary traits (`WriteOps`, `ReadOps`) and types (`PropertyMap`) available immediately without hunting for imports.

### Story 3: Clear Feature Requirements
*   **As a** Developer reading the documentation,
*   **I want** code examples to explicitly state required feature flags,
*   **So that** I don't waste time debugging "module not found" errors.

## 3. Acceptance Criteria

### AC1: Smart InternedString Debug
- [ ] `InternedString` implements `fmt::Debug` manually.
- [ ] `{:?}` prints `InternedString("Value")` if the ID exists in `GLOBAL_INTERNER`.
- [ ] `{:?}` falls back to `InternedString(ID)` if not found.
- [ ] `Display` remains unchanged (or is verified to work correctly).

### AC2: The Prelude Module
- [ ] `src/prelude.rs` exists.
- [ ] It re-exports:
    - `AletheiaDB`
    - `WriteOps`, `ReadOps` (Traits)
    - `NodeId`, `EdgeId`
    - `PropertyMap`, `PropertyMapBuilder`
    - `Result`, `Error`
- [ ] `src/lib.rs` exports `prelude`.

### AC3: Documentation Updates
- [ ] `README.md` examples for `nova` features include `// [dependencies] aletheiadb = { ..., features = ["nova"] }` or equivalent comments.
- [ ] `README.md` "Quick Start" uses the new `prelude`.

## 4. Out of Scope (Phase 2)
- Visualizing the graph structure (see `test_vis`).
- Interactive CLI.
