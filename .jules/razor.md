## [Reduction]
**Bloat:** The `StorageSnapshot` trait in `src/storage/snapshot.rs`. It was implemented by exactly one struct (`CurrentStorageSnapshot`). `HistoricalStorageSnapshot` did not implement it. All callers explicitly took `&CurrentStorageSnapshot`.
**Cut:** Deleted the `StorageSnapshot` trait, merged its methods directly into `CurrentStorageSnapshot`, and updated `src/storage/checkpoint.rs` to stop importing and using it.
**Saved:** Unnecessary indirection and a useless trait definition. Code is more explicit and easier to read.
