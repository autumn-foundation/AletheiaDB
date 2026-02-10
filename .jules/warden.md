# Warden's Journal

**2026-02-15 - Unbounded Memory Allocation in HNSW Mappings Loader**
**Threat:** A malicious actor could provide a sparse mappings file with a valid header claiming billions of entries but containing no data (sparse file). The `load_mappings_with_integrity` function would attempt to insert these entries into a `DashMap` based on the count, leading to Out-Of-Memory (OOM) Denial of Service (DoS) as it consumes gigabytes of RAM for the map structure.
**Defense:** Introduced `MAX_MAPPINGS_COUNT` constant (100,000,000) in `src/index/vector/hnsw.rs` and enforced this limit in `load_mappings_with_integrity` before allocation. Added regression test `tests/warden_hnsw_mappings_dos.rs` to verify rejection of malicious files.

**2026-02-15 - FFI Unwind Safety in HNSW Metric Wrapper**
**Threat:** The `create_metric_wrapper` function in `src/index/vector/hnsw.rs` used `panic!` when encountering null or unaligned pointers from the C++ `usearch` library. Panicking across an FFI boundary is Undefined Behavior (UB) and can lead to memory corruption or exploitable crashes.
**Defense:** Replaced `panic!` with `std::process::abort()` and added explicit error logging. This ensures the process terminates safely and immediately if the integrity of the FFI boundary is violated, preventing UB.
