# Warden's Journal

**2026-02-15 - Unbounded Memory Allocation in HNSW Mappings Loader**
**Threat:** A malicious actor could provide a sparse mappings file with a valid header claiming billions of entries but containing no data (sparse file). The `load_mappings_with_integrity` function would attempt to insert these entries into a `DashMap` based on the count, leading to Out-Of-Memory (OOM) Denial of Service (DoS) as it consumes gigabytes of RAM for the map structure.
**Defense:** Introduced `MAX_MAPPINGS_COUNT` constant (100,000,000) in `src/index/vector/hnsw.rs` and enforced this limit in `load_mappings_with_integrity` before allocation. Added regression test `tests/warden_hnsw_mappings_dos.rs` to verify rejection of malicious files.

**2026-02-15 - FFI Unwind Safety in HNSW Metric Wrapper**
**Threat:** The `create_metric_wrapper` function in `src/index/vector/hnsw.rs` used `panic!` when encountering null or unaligned pointers from the C++ `usearch` library. Panicking across an FFI boundary is Undefined Behavior (UB) and can lead to memory corruption or exploitable crashes.
**Defense:** Replaced `panic!` with `std::process::abort()` and added explicit error logging. This ensures the process terminates safely and immediately if the integrity of the FFI boundary is violated, preventing UB.

**2026-02-15 - Path Traversal in Vector Index Creation**
**Threat:** The `enable_vector_index` API accepted arbitrary strings as property names. These names were used to construct filesystem paths for index persistence. A malicious actor could supply a property name containing `..` or path separators to traverse outside the intended directory and potentially overwrite critical files during persistence operations.
**Defense:** Implemented `validate_property_name` in `src/storage/current/mod.rs` to strictly validate property names. It rejects names containing `..`, `/`, or `\` and enforces a length limit. This validation is applied at the API entry point, preventing invalid paths from entering the system.

**2026-02-15 - Broken RAII Guard in HNSW Index**
**Threat:** The `FilterCallbackGuard` in `src/index/vector/hnsw.rs` was missing a `Drop` implementation, meaning the `IN_FILTER_CALLBACK` thread-local flag was never reset to `false` after a callback finished. This could leave a thread permanently unable to modify indexes if a callback was ever invoked, leading to a Denial of Service (DoS) for write operations on that thread.
**Defense:** Implemented `Drop` for `FilterCallbackGuard` to ensure the flag is always reset when the guard goes out of scope, guaranteeing correct state management even during panics.

**2026-02-15 - Buffer Over-read in WAL Segment Parsing**
**Threat:** The `UpdateEdge` operation parser in `src/storage/wal/segment_reader.rs` failed to check bounds before reading the 4-byte `label_id` field when processing legacy WAL versions. This allowed a malformed WAL entry (truncated payload) to trigger a panic (index out of bounds), leading to a Denial of Service (DoS) during recovery or replication.
**Defense:** Added an explicit `checked_add` bounds check before reading the `label_id` field, ensuring the buffer has sufficient capacity before access.

**2026-02-15 - Unchecked Buffer Access in UpdateNode Operation**
**Threat:** Similar to `UpdateEdge`, the `UpdateNode` operation parser in `src/storage/wal/segment_reader.rs` lacked a bounds check for the `label_id` field when processing `WAL_VERSION` entries. This exposed the parser to the same buffer over-read panic (DoS) if a truncated entry was processed.
**Defense:** Added an identical `checked_add` bounds check for `UpdateNode`, ensuring consistency and safety across all operations.
