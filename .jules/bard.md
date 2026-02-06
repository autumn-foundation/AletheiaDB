# Bard's Journal 🎻

## 2024-05-23 - Outdated Vector Index Docs
**Confusion:** The `src/index/vector/mod.rs` documentation contained outdated comments stating "no VectorIndex implementation exists yet" and referencing future phases, despite `HnswIndex` being fully implemented. The examples were also marked `no_run`.
**Clarification:** Updated the documentation to reflect that `HnswIndex` is the concrete implementation. Examples were updated to use `ignore` (as they require setup) and the text was updated to describe the current state of the implementation.

## 2024-05-24 - Transaction Consistency Guarantees
**Confusion:** The `ReadOps` trait methods in `src/api/transaction/mod.rs` did not specify consistency guarantees. Specifically, `node_count()` is *not* snapshot-isolated (returns current count), while `get_node()` *is* snapshot-isolated. This distinction is critical but was undocumented.
**Clarification:** Updated `ReadOps` documentation to explicitly state snapshot isolation behavior, ordering guarantees (none), and performance complexity for all methods. Added examples to `WriteOps` to demonstrate correct usage.

## 2024-05-24 - Historical Version Metadata
**Confusion:** Historical versions retrieved from storage (not currently in memory) may have `created_by_tx` set to `TxId(0)`. This is because the creating transaction ID is not currently preserved in the historical storage format to save space.
**Clarification:** Updated `VersionMetadata` documentation to explicitly state this behavior.

## 2024-05-25 - AletheiaDB Default Configuration
**Confusion:** `AletheiaDB::new()` documentation did not specify whether it creates an in-memory or disk-based database. It defaults to disk-based storage at `./aletheiadb/wal`, which could surprise users expecting an ephemeral in-memory instance.
**Clarification:** Updated `AletheiaDB::new()` documentation to explicitly state the default disk-based configuration and point to `with_unified_config` for customization.

## 2024-05-25 - WAL Entry Binary Format
**Confusion:** The on-disk binary format of `WalEntry` was only documented in code comments within the serialization logic, making it hard to understand the storage format without deep diving into implementation details.
**Clarification:** Added detailed binary layout documentation to the `WalEntry` struct in `src/storage/wal/entry.rs`, including field sizes and ordering.

## 2024-05-26 - HTTP JSON Conversion Limits
**Confusion:** The HTTP API's JSON conversion logic enforces a recursion depth limit (100) to prevent stack overflow attacks, but this behavior was undocumented and could surprise developers working with deeply nested data.
**Clarification:** Added documentation to `src/http/converters.rs` explicitly stating the recursion limit and detailing the type mappings between AletheiaDB types and JSON types.
