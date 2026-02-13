# Warden's Journal

**2026-02-15 - Unbounded Memory Allocation in HNSW Mappings Loader**
**Threat:** A malicious actor could provide a sparse mappings file with a valid header claiming billions of entries but containing no data (sparse file). The `load_mappings_with_integrity` function would attempt to insert these entries into a `DashMap` based on the count, leading to Out-Of-Memory (OOM) Denial of Service (DoS) as it consumes gigabytes of RAM for the map structure.
**Defense:** Introduced `MAX_MAPPINGS_COUNT` constant (100,000,000) in `src/index/vector/hnsw.rs` and enforced this limit in `load_mappings_with_integrity` before allocation. Added regression test `tests/warden_hnsw_mappings_dos.rs` to verify rejection of malicious files.

**2026-02-15 - Unsafe Pointer Arithmetic in SIMD Functions**
**Threat:** Internal SIMD functions (`dot_and_magnitudes_avx2`, etc.) in `src/core/vector/simd.rs` used manual pointer arithmetic (`ptr.add(offset)`) assuming vectors had equal lengths. If called with mismatched lengths (violating the safety contract), this would lead to buffer over-reads and Undefined Behavior (UB/Segfault). Although wrapper functions enforce length checks, the internal unsafe API was brittle.
**Defense:** Refactored all SIMD implementations to use safe slice iterators (`chunks_exact` and `zip`) and explicitly sliced inputs to the common minimum length. This eliminates manual pointer arithmetic from the loop logic and guarantees panic-free (defined) behavior even if length invariants are violated. Added `test_simd_mismatched_lengths_safety` to verify robustness.

**2026-02-15 - Redundant Alignment Checks in HNSW Metric Wrapper**
**Threat:** The `create_metric_wrapper` function in `src/index/vector/hnsw.rs` contained redundant alignment checks. While not a security vulnerability per se, it added complexity and potential for confusion. The bitwise check `(ptr as usize) & (align - 1)` is sufficient and more performant than `ptr.align_offset(align)`.
**Defense:** Removed the redundant `align_offset` check, relying on the bitwise check for safety. Also added `test_load_mappings_count_limit` to `src/index/vector/hnsw.rs` to verify OOM protection for Version 2 mapping files, complementing the existing Version 1 test.

**2026-02-15 - LSN Allocator Overflow**
**Threat:** The atomic LSN allocator used `fetch_add` without overflow checking. While requiring ~5000 years at 100M/sec to overflow `u64`, a large batch allocation (e.g. `u64::MAX`) or eventual wraparound would cause duplicate LSNs, breaking WAL ordering and data consistency.
**Defense:** Replaced `fetch_add` with `fetch_update` (CAS loop) in `src/storage/wal/lsn_allocator.rs` to atomically check for overflow *before* modifying the state. Added `tests/warden_security_tests.rs` to verify panic behavior on overflow attempts.

**2026-02-15 - HNSW Metric Panic Propagation (DoS)**
**Threat:** A panic within a user-provided custom metric function (in `HnswIndex`) would propagate unwinding across the FFI boundary into the C++ `usearch` library. Rust panics crossing FFI boundaries result in undefined behavior, typically manifesting as an immediate process abort (`SIGABRT`). This provided a Denial of Service vector where a malicious or buggy metric function could crash the entire database process.
**Defense:** Wrapped the execution of the user's distance function in `std::panic::catch_unwind` within `create_metric_wrapper` in `src/index/vector/hnsw.rs`. If a panic occurs, it is caught, an error is logged to stderr, and `f32::MAX` is returned to signal maximum dissimilarity. This allows the search to complete safely without crashing. Added `tests/warden_security_resilience.rs` to verify that searches survive panicking metrics.
