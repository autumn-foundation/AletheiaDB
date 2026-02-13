# Warden's Journal

**2026-02-15 - Unbounded Memory Allocation in HNSW Mappings Loader**
**Threat:** A malicious actor could provide a sparse mappings file with a valid header claiming billions of entries but containing no data (sparse file). The `load_mappings_with_integrity` function would attempt to insert these entries into a `DashMap` based on the count, leading to Out-Of-Memory (OOM) Denial of Service (DoS) as it consumes gigabytes of RAM for the map structure.
**Defense:** Introduced `MAX_MAPPINGS_COUNT` constant (100,000,000) in `src/index/vector/hnsw.rs` and enforced this limit in `load_mappings_with_integrity` before allocation. Added regression test `tests/warden_hnsw_mappings_dos.rs` to verify rejection of malicious files.

**2026-02-15 - LSN Allocator Overflow**
**Threat:** The atomic LSN allocator used `fetch_add` without overflow checking. While requiring ~5000 years at 100M/sec to overflow `u64`, a large batch allocation (e.g. `u64::MAX`) or eventual wraparound would cause duplicate LSNs, breaking WAL ordering and data consistency.
**Defense:** Replaced `fetch_add` with `fetch_update` (CAS loop) in `src/storage/wal/lsn_allocator.rs` to atomically check for overflow *before* modifying the state. Added `tests/warden_security_tests.rs` to verify panic behavior on overflow attempts.
