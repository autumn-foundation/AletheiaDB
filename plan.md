1. **Delete the `StorageSnapshot` trait in `src/storage/snapshot.rs`.**
   - It's an over-engineered abstraction for taking snapshots of the storage. There's only one implementation: `CurrentStorageSnapshot` (though `HistoricalStorageSnapshot` exists but doesn't implement the trait).

2. **Refactor `CurrentStorageSnapshot` to remove trait implementation.**
   - Change `type NodeIter` to actual types and directly implement `iter_nodes` and `iter_edges` on the struct.

3. **Update consumers to use `CurrentStorageSnapshot` and `HistoricalStorageSnapshot` concretely.**
   - Remove `use crate::storage::snapshot::StorageSnapshot;` imports in files like `src/storage/checkpoint.rs`.
   - Update usages to use concrete methods.

4. **Complete pre-commit steps.**
   - Complete pre-commit steps to ensure proper testing, verification, review, and reflection are done.

5. **Create a PR with the title 🪒 Razor: Delete StorageSnapshot trait.**
