# Warden's Journal

**2026-02-15 - Unbounded Memory Allocation in HNSW Mappings Loader**
**Threat:** A malicious actor could provide a sparse mappings file with a valid header claiming billions of entries but containing no data (sparse file). The `load_mappings_with_integrity` function would attempt to insert these entries into a `DashMap` based on the count, leading to Out-Of-Memory (OOM) Denial of Service (DoS) as it consumes gigabytes of RAM for the map structure.
**Defense:** Introduced `MAX_MAPPINGS_COUNT` constant (100,000,000) in `src/index/vector/hnsw.rs` and enforced this limit in `load_mappings_with_integrity` before allocation. Added regression test `tests/warden_hnsw_mappings_dos.rs` to verify rejection of malicious files.

**2026-02-11 - Unchecked Buffer Access in SIMD Operations**
**Threat:** Internal `unsafe` SIMD functions (e.g., `dot_and_magnitudes_avx2`) assumed input vectors had equal lengths but did not validate this invariant. A caller passing mismatched vectors (where the second is shorter) would cause a buffer over-read (Undefined Behavior) due to unchecked pointer arithmetic in the optimized loop.
**Defense:** Added `assert_eq!(a.len(), b.len())` to all `unsafe` SIMD primitives in `src/core/vector/simd.rs`. This enforces the invariant at the lowest level, ensuring that any misuse results in a safe panic rather than memory corruption. Added regression tests `test_unsafe_simd_mismatch_panics` to verify the fix.
