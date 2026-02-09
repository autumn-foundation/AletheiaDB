# Warden's Journal

**2026-02-15 - Unbounded Memory Allocation in HNSW Mappings Loader**
**Threat:** A malicious actor could provide a sparse mappings file with a valid header claiming billions of entries but containing no data (sparse file). The `load_mappings_with_integrity` function would attempt to insert these entries into a `DashMap` based on the count, leading to Out-Of-Memory (OOM) Denial of Service (DoS) as it consumes gigabytes of RAM for the map structure.
**Defense:** Introduced `MAX_MAPPINGS_COUNT` constant (100,000,000) in `src/index/vector/hnsw.rs` and enforced this limit in `load_mappings_with_integrity` before allocation. Added regression test `tests/warden_hnsw_mappings_dos.rs` to verify rejection of malicious files.

**2026-02-15 - Unbounded Dimension Allocation in HNSW Index**
**Threat:** An attacker could configure an HNSW index with excessively large dimensions (e.g., `usize::MAX`), leading to:
1.  **Denial of Service (DoS):** `usearch` or internal vectors attempting to allocate massive amounts of memory.
2.  **Potential UB:** In `create_metric_wrapper`, `slice::from_raw_parts` is called with `dims`. If `dims * size_of<f32>` exceeds `isize::MAX`, this is Undefined Behavior.
**Defense:** Enforced `MAX_VECTOR_DIMENSIONS` (100,000) in `HnswIndexBuilder::build`, `HnswIndex::load`, and `HnswIndex::open_mmap`. Added regression test `tests/warden_hnsw_dimensions.rs`.
