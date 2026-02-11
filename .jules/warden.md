# Warden's Journal

**2026-02-15 - Unbounded Memory Allocation in HNSW Mappings Loader**
**Threat:** A malicious actor could provide a sparse mappings file with a valid header claiming billions of entries but containing no data (sparse file). The `load_mappings_with_integrity` function would attempt to insert these entries into a `DashMap` based on the count, leading to Out-Of-Memory (OOM) Denial of Service (DoS) as it consumes gigabytes of RAM for the map structure.
**Defense:** Introduced `MAX_MAPPINGS_COUNT` constant (100,000,000) in `src/index/vector/hnsw.rs` and enforced this limit in `load_mappings_with_integrity` before allocation. Added regression test `tests/warden_hnsw_mappings_dos.rs` to verify rejection of malicious files.

**2026-02-11 - Unchecked Buffer Access in SIMD Operations**
**Threat:** Internal `unsafe` SIMD functions (e.g., `dot_and_magnitudes_avx2`) assumed input vectors had equal lengths but did not validate this invariant. A caller passing mismatched vectors (where the second is shorter) would cause a buffer over-read (Undefined Behavior) due to unchecked pointer arithmetic in the optimized loop.
**Defense:** Added `assert_eq!(a.len(), b.len())` to all `unsafe` SIMD primitives in `src/core/vector/simd.rs`. This enforces the invariant at the lowest level, ensuring that any misuse results in a safe panic rather than memory corruption. Added regression tests `test_unsafe_simd_mismatch_panics` to verify the fix.

**2026-02-11 - Lock Inversion Deadlock in HNSW Index**
**Threat:** A deadlock was possible when an  operation (specifically the  path) held the  (DashMap) shard lock while waiting for the  (usearch RwLock) write lock. This violated the global lock ordering invariant (inner -> id_mapping), causing a potential cycle if another thread (e.g.,  or ) held  and waited for .
**Defense:** Refactored  to use optimistic concurrency control. The  lock is now dropped before acquiring the  write lock. After acquiring , we re-verify the mapping to handle potential concurrent modifications, retrying the operation if necessary. This strictly enforces the correct lock ordering and eliminates the deadlock.

**2026-02-11 - Lock Inversion Deadlock in HNSW Index**
**Threat:** A deadlock was possible when an `add` operation (specifically the `Occupied` path) held the `id_mapping` (DashMap) shard lock while waiting for the `inner` (usearch RwLock) write lock. This violated the global lock ordering invariant (inner -> id_mapping), causing a potential cycle if another thread (e.g., `save` or `search`) held `inner` and waited for `id_mapping`.
**Defense:** Refactored `HnswIndex::add` to use optimistic concurrency control. The `id_mapping` lock is now dropped before acquiring the `inner` write lock. After acquiring `inner`, we re-verify the mapping to handle potential concurrent modifications, retrying the operation if necessary. This strictly enforces the correct lock ordering and eliminates the deadlock.
