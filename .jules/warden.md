# Warden's Journal

**2026-02-15 - Unbounded Memory Allocation in HNSW Mappings Loader**
**Threat:** A malicious actor could provide a sparse mappings file with a valid header claiming billions of entries but containing no data (sparse file). The `load_mappings_with_integrity` function would attempt to insert these entries into a `DashMap` based on the count, leading to Out-Of-Memory (OOM) Denial of Service (DoS) as it consumes gigabytes of RAM for the map structure.
**Defense:** Introduced `MAX_MAPPINGS_COUNT` constant (100,000,000) in `src/index/vector/hnsw.rs` and enforced this limit in `load_mappings_with_integrity` before allocation. Added regression test `tests/warden_hnsw_mappings_dos.rs` to verify rejection of malicious files.

**2026-02-15 - Unsafe Pointer Arithmetic in SIMD Functions**
**Threat:** Internal SIMD functions (`dot_and_magnitudes_avx2`, etc.) in `src/core/vector/simd.rs` used manual pointer arithmetic (`ptr.add(offset)`) assuming vectors had equal lengths. If called with mismatched lengths (violating the safety contract), this would lead to buffer over-reads and Undefined Behavior (UB/Segfault). Although wrapper functions enforce length checks, the internal unsafe API was brittle.
**Defense:** Refactored all SIMD implementations to use safe slice iterators (`chunks_exact` and `zip`) and explicitly sliced inputs to the common minimum length. This eliminates manual pointer arithmetic from the loop logic and guarantees panic-free (defined) behavior even if length invariants are violated. Added `test_simd_mismatched_lengths_safety` to verify robustness.

**2026-02-15 - Vector Deserialization Hardening Verification**
**Threat:** Malicious inputs causing Denial of Service (DoS) via excessive allocation or memory safety issues in `unsafe` vector deserialization logic.
**Defense:** Audited `src/core/property.rs` and `src/index/vector/hnsw.rs`. Verified presence of `MAX_VECTOR_DIMENSIONS`, `MAX_RECURSION_DEPTH`, and buffer bounds checks. Added `tests/warden_verification.rs` as a regression suite ("Test Exploits") to enforce these limits and prevent future regressions. Confirmed that `deserialize_vector` and `deserialize_sparse_vector` safely handle invalid inputs without panicking or accessing uninitialized memory.

**2026-02-15 - Redundant Alignment Checks in HNSW Metric Wrapper**
**Threat:** The `create_metric_wrapper` function in `src/index/vector/hnsw.rs` contained redundant alignment checks. While not a security vulnerability per se, it added complexity and potential for confusion. The bitwise check `(ptr as usize) & (align - 1)` is sufficient and more performant than `ptr.align_offset(align)`.
**Defense:** Removed the redundant `align_offset` check, relying on the bitwise check for safety. Also added `test_load_mappings_count_limit` to `src/index/vector/hnsw.rs` to verify OOM protection for Version 2 mapping files, complementing the existing Version 1 test.

**2026-02-15 - LSN Allocator Overflow**
**Threat:** The atomic LSN allocator used `fetch_add` without overflow checking. While requiring ~5000 years at 100M/sec to overflow `u64`, a large batch allocation (e.g. `u64::MAX`) or eventual wraparound would cause duplicate LSNs, breaking WAL ordering and data consistency.
**Defense:** Replaced `fetch_add` with `fetch_update` (CAS loop) in `src/storage/wal/lsn_allocator.rs` to atomically check for overflow *before* modifying the state. Added `tests/warden_security_tests.rs` to verify panic behavior on overflow attempts.

**2026-02-15 - FFI Boundary Panic Resilience**
**Threat:** A custom distance metric function provided by the user could panic (e.g., due to division by zero or explicit panic). Since this function is called from the C++ `usearch` library via FFI, allowing the panic to unwind across the FFI boundary causes Undefined Behavior (UB), typically aborting the process.
**Defense:** Wrapped the user-provided metric closure in `std::panic::catch_unwind` within `create_metric_wrapper` in `src/index/vector/hnsw.rs`. If a panic occurs, it is caught, logged to stderr, and `f32::MAX` is returned to indicate infinite distance (no match), preserving process stability.

**2026-02-16 - DoS via Massive WAL Entry**
**Threat:** A malicious user could submit a database operation (e.g. `CreateNode`) with a `PropertyMap` containing massive values (e.g. 10M element arrays or 100MB byte strings). `ConcurrentWal` would unknowingly serialize this into a huge buffer and attempt to append it to the `WalRingBuffer`. If multiple such requests occurred, the fixed-capacity (by slot) ring buffer would hold gigabytes of data, causing Out-Of-Memory (OOM) crashes.
**Defense:** Introduced `MAX_WAL_ENTRY_SIZE` (64MB) in `src/storage/wal/entry.rs`. Modified `ConcurrentWal::serialize_entry` to check the estimated size against this limit *before* allocation, returning `StorageError::CapacityExceeded` if violated. Added `tests/warden_wal_dos.rs` to verify rejection.

**2026-02-16 - Panic in PropertyMap::from_iter**
**Threat:** The `PropertyMap::from_iter` method used `expect()` when calculating serialized size. If a user provided a deeply nested `PropertyValue` (exceeding `MAX_RECURSION_DEPTH` of 100), `serialized_size()` would return an error, causing `from_iter` to panic and crash the process. This crash vector was reachable via standard iterator usage.
**Defense:** Replaced `expect()` with `unwrap_or(RECURSION_PENALTY_SIZE)` in `src/core/property.rs`. This allows map construction to proceed without crashing. Safety is maintained because the subsequent `serialize()` operation re-checks recursion depth and will fail gracefully (returning `Result::Err`) instead of panicking.

**2026-02-16 - SIMD Loop Safety Hardening**
**Threat:** `dot_and_magnitudes_avx2` and `dot_and_magnitudes_sse2` functions in `src/core/vector/simd.rs` used manual pointer arithmetic (`ptr.add(offset)`) inside the main processing loop. While logically correct given the `chunks` calculation, explicit pointer arithmetic in loops is brittle and prone to off-by-one errors or bounds check bypasses if logic changes.
**Defense:** Refactored both functions to use `chunks_exact` and `zip` iterator combinators. This eliminates manual pointer arithmetic from the loop body, relying instead on the standard library's verified slice iteration logic. The `unsafe` block scope is reduced to only the intrinsics themselves (which require unsafe). Verified with `test_simd_dot_and_magnitudes_large_vector` to ensure correctness on large inputs.
