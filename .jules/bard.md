
## 2024-05-23 - Outdated Vector Index Docs
**Confusion:** The `src/index/vector/mod.rs` documentation contained outdated comments stating "no VectorIndex implementation exists yet" and referencing future phases, despite `HnswIndex` being fully implemented. The examples were also marked `no_run`.
**Clarification:** Updated the documentation to reflect that `HnswIndex` is the concrete implementation. Examples were updated to use `ignore` (as they require setup) and the text was updated to describe the current state of the implementation.

## 2024-05-24 - Transaction Consistency Guarantees
**Confusion:** The `ReadOps` trait methods in `src/api/transaction/mod.rs` did not specify consistency guarantees. Specifically, `node_count()` is *not* snapshot-isolated (returns current count), while `get_node()` *is* snapshot-isolated. This distinction is critical but was undocumented.
**Clarification:** Updated `ReadOps` documentation to explicitly state snapshot isolation behavior, ordering guarantees (none), and performance complexity for all methods. Added examples to `WriteOps` to demonstrate correct usage.

## 2024-05-24 - Historical Version Metadata
**Confusion:** Historical versions retrieved from storage (not currently in memory) may have `created_by_tx` set to `TxId(0)`. This is because the creating transaction ID is not currently preserved in the historical storage format to save space.
**Clarification:** Updated `VersionMetadata` documentation to explicitly state this behavior.
