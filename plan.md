1. **Goal**: Remove the `StorageSnapshot` trait.
   - **Reason**: It's a "One-Time" trait. Only `CurrentStorageSnapshot` implements it. `HistoricalStorageSnapshot` does not implement it, but has similar methods. The `StorageSnapshot` trait isn't actually being used polymorphically anywhere; callers (like `extract_graph_data_from_snapshot` in `src/storage/checkpoint.rs`) just take a concrete `&CurrentStorageSnapshot` anyway.
2. **Steps**:
   - `src/storage/snapshot.rs`:
     - Delete `pub trait StorageSnapshot { ... }`.
     - Change `impl StorageSnapshot for CurrentStorageSnapshot` to just `impl CurrentStorageSnapshot`.
     - Remove `type NodeIter = CurrentNodeIterator;` and `type EdgeIter = CurrentEdgeIterator;` and return the concrete types instead of `Self::NodeIter`.
     - Wait, `iter_nodes` and `iter_edges` return `CurrentNodeIterator` and `CurrentEdgeIterator`. Make sure they return these explicitly.
   - `src/storage/mod.rs`:
     - Remove `StorageSnapshot` from the `pub use snapshot::{...};` exports.
   - `src/storage/checkpoint.rs`:
     - Remove `use crate::storage::snapshot::StorageSnapshot;`
3. **Write Tests Check**:
   - Verify with `cargo test`.
4. **Pre-commit checks**:
   - Run clippy, formatting, and `cargo doc --all-features`.
