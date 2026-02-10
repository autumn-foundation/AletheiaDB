# Warden's Journal

**2026-02-15 - Unbounded Memory Allocation in HNSW Mappings Loader**
**Threat:** A malicious actor could provide a sparse mappings file with a valid header claiming billions of entries but containing no data (sparse file). The `load_mappings_with_integrity` function would attempt to insert these entries into a `DashMap` based on the count, leading to Out-Of-Memory (OOM) Denial of Service (DoS) as it consumes gigabytes of RAM for the map structure.
**Defense:** Introduced `MAX_MAPPINGS_COUNT` constant (100,000,000) in `src/index/vector/hnsw.rs` and enforced this limit in `load_mappings_with_integrity` before allocation. Added regression test `tests/warden_hnsw_mappings_dos.rs` to verify rejection of malicious files.

**2026-02-15 - FFI Unwind Safety in HNSW Metric Wrapper**
**Threat:** The `create_metric_wrapper` function in `src/index/vector/hnsw.rs` used `panic!` when encountering null or unaligned pointers from the C++ `usearch` library. Panicking across an FFI boundary is Undefined Behavior (UB) and can lead to memory corruption or exploitable crashes.
**Defense:** Replaced `panic!` with `std::process::abort()` and added explicit error logging. This ensures the process terminates safely and immediately if the integrity of the FFI boundary is violated, preventing UB.

**2026-02-16 - Sparse Vector Dimension Mismatch Logic Bug**
**Threat:** `sparse_dot_product` and `sparse_cosine_similarity` in `src/core/vector/sparse.rs` did not verify that input vectors had matching dimensions. This allowed operations between vectors from different vector spaces (e.g., comparing a 10-dim vector with a 100-dim vector), leading to mathematically invalid results and potential logic errors in downstream applications (e.g., semantic search returning high similarity for incompatible vectors).
**Defense:** Added explicit `a.dimension() == b.dimension()` checks to `sparse_dot_product` (which propagates to cosine similarity), returning `VectorError::DimensionMismatch` on failure. Verified with `tests/warden_sparse_dimensions.rs`.

**2026-02-16 - Thread-Local State Leak in Filter Callback Guard**
**Threat:** The `FilterCallbackGuard` struct in `src/index/vector/hnsw.rs` set a thread-local flag `IN_FILTER_CALLBACK` to `true` on creation but failed to implement `Drop` to reset it to `false`. This meant that once a thread performed a filtered search, it would remain permanently in a "callback" state, potentially blocking legitimate index modifications (adds/removes) on that thread due to deadlock prevention checks.
**Defense:** Implemented `Drop` for `FilterCallbackGuard` to reset the flag, ensuring correct state cleanup even during panics (RAII).
