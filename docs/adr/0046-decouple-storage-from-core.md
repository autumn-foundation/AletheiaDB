# 46. Decouple Storage from Core

Date: 2024-05-23

## Status

Proposed

## Context

Circular dependencies between the core domain logic and the storage implementation were causing build failures and preventing efficient refactoring. The monolithic structure meant that any change in persistence logic triggered a rebuild of the entire core system.

We needed a way to isolate the storage mechanism (how data is saved) from the business logic (what the data means).

## Decision

We will move all persistence logic to a dedicated `storage` module (intended to become a separate crate `aletheiadb-storage`).

The architectural boundary will be defined by a set of **Storage Traits** located in the `core` module:

```rust
// In Core
pub trait StorageEngine: Send + Sync {
    fn get_node(&self, id: NodeId) -> Result<Node>;
    fn save_node(&self, node: &Node) -> Result<()>;
}
```

The `storage` module will implement these traits. This inverts the dependency: `Core` defines the interface, and `Storage` depends on `Core` to implement it.

## Consequences

### Positive
*   **Build Times**: Improving modularity reduces incremental build times significantly.
*   **Clarity**: Strict separation of concerns makes the codebase easier to navigate.
*   **Pluggability**: Easier to swap out storage backends (e.g., in-memory for testing).

### Negative
*   **FFI Complexity**: Moving types across crate boundaries can complicate FFI if we expose a C API.
*   **Boilerplate**: Requires defining traits and potentially duplicating data structures for serialization.
