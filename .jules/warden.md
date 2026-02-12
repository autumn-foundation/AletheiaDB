# Warden's Journal

**2026-02-15 - Unbounded Memory Allocation in HNSW Mappings Loader**
**Threat:** A malicious actor could provide a sparse mappings file with a valid header claiming billions of entries but containing no data (sparse file). The `load_mappings_with_integrity` function would attempt to insert these entries into a `DashMap` based on the count, leading to Out-Of-Memory (OOM) Denial of Service (DoS) as it consumes gigabytes of RAM for the map structure.
**Defense:** Introduced `MAX_MAPPINGS_COUNT` constant (100,000,000) in `src/index/vector/hnsw.rs` and enforced this limit in `load_mappings_with_integrity` before allocation. Added regression test `tests/warden_hnsw_mappings_dos.rs` to verify rejection of malicious files.

**2026-02-15 - Unsafe Pointer Arithmetic in SIMD Functions**
**Threat:** Internal SIMD functions (`dot_and_magnitudes_avx2`, etc.) in `src/core/vector/simd.rs` used manual pointer arithmetic (`ptr.add(offset)`) assuming vectors had equal lengths. If called with mismatched lengths (violating the safety contract), this would lead to buffer over-reads and Undefined Behavior (UB/Segfault). Although wrapper functions enforce length checks, the internal unsafe API was brittle.
**Defense:** Refactored all SIMD implementations to use safe slice iterators (`chunks_exact` and `zip`) and explicitly sliced inputs to the common minimum length. This eliminates manual pointer arithmetic from the loop logic and guarantees panic-free (defined) behavior even if length invariants are violated. Added `test_simd_mismatched_lengths_safety` to verify robustness.

**2026-02-15 - Zip Bomb Vulnerability in Cold Storage Decompression**
**Threat:** The `decompress` function in `src/storage/compression.rs` used `zstd::decode_all`, which attempts to decompress the entire input into memory without a size limit. A malicious actor could provide a small compressed payload (e.g., 42KB) that expands to gigabytes of data (a "Zip Bomb"), causing the server to crash due to Out-Of-Memory (OOM) Denial of Service (DoS).
**Defense:** Replaced `zstd::decode_all` with a hardened `decompress_with_limit` function that enforces a strict `max_decompressed_size` (default 64MB). Modified the loop to check the limit *before* extending the buffer to prevent transient memory spikes. Added configuration options to `ColdStorageConfig` and regression test `test_decompress_zip_bomb_via_config`.
