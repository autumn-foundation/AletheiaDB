# Warden's Journal

**2026-02-15 - Unbounded Memory Allocation in HNSW Mappings Loader**
**Threat:** A malicious actor could provide a sparse mappings file with a valid header claiming billions of entries but containing no data (sparse file). The `load_mappings_with_integrity` function would attempt to insert these entries into a `DashMap` based on the count, leading to Out-Of-Memory (OOM) Denial of Service (DoS) as it consumes gigabytes of RAM for the map structure.
**Defense:** Introduced `MAX_MAPPINGS_COUNT` constant (100,000,000) in `src/index/vector/hnsw.rs` and enforced this limit in `load_mappings_with_integrity` before allocation. Added regression test `tests/warden_hnsw_mappings_dos.rs` to verify rejection of malicious files.

**2026-02-16 - FFI Panic on Unaligned Pointers in HNSW**
**Threat:** The `create_metric_wrapper` function in `src/index/vector/hnsw.rs` panicked when receiving unaligned pointers from the `usearch` FFI boundary. While this prevented Undefined Behavior (UB), it introduced a Denial of Service (DoS) vector where a misbehaving library or specific usage pattern could crash the application.
**Defense:** Replaced the panic with a safe fallback that copies unaligned data to a temporary aligned `Vec<f32>` (using `Cow` to minimize overhead in the happy path). This ensures availability even if the underlying library passes unaligned data, adhering to the principle of "Defense in Depth". Verified with `test_metric_wrapper_handles_unaligned`.
