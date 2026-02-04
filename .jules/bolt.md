**[HNSW Optimization: IdentityHasher]**
**Learning:** `DashMap` (and `HashMap`) use cryptographic hashing (SipHash) by default, which is overkill for keys that are already unique integers (like `NodeId` or `u64` IDs). This adds measurable overhead on hot paths like search filtering and result sorting where we perform thousands of lookups.
**Action:** Created `src/utils/hashing.rs` with `IdentityHasher` and applied it to `HnswIndex` mappings and `StringInterner`. This is a classic "zero-cost abstraction" optimization—using the type system to swap implementation details for performance.
