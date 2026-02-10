# Warden's Journal

**2026-02-15 - Unbounded Memory Allocation in HNSW Mappings Loader**
**Threat:** A malicious actor could provide a sparse mappings file with a valid header claiming billions of entries but containing no data (sparse file). The `load_mappings_with_integrity` function would attempt to insert these entries into a `DashMap` based on the count, leading to Out-Of-Memory (OOM) Denial of Service (DoS) as it consumes gigabytes of RAM for the map structure.
**Defense:** Introduced `MAX_MAPPINGS_COUNT` constant (100,000,000) in `src/index/vector/hnsw.rs` and enforced this limit in `load_mappings_with_integrity` before allocation. Added regression test `tests/warden_hnsw_mappings_dos.rs` to verify rejection of malicious files.

**2026-02-15 - FFI Boundary Panic Hardening**
**Threat:** Panicking across FFI boundaries (e.g., in a callback passed to a C/C++ library) is Undefined Behavior (UB) in Rust. The `create_metric_wrapper` function in `src/index/vector/hnsw.rs` used `panic!` when detecting null or unaligned pointers from `usearch`. If `usearch` (C++) triggered this callback and the panic unwound into C++ frames, it could cause memory corruption or arbitrary code execution.
**Defense:** Replaced `panic!` with `std::process::abort()` in the FFI callback. `abort()` immediately terminates the process, which is safe (though drastic) as it prevents unwinding into C++ code. Logged a critical error to stderr before aborting. Removed unit tests that expected panics as `abort()` is not testable via `#[should_panic]`.

**2026-02-15 - Serialization DoS Hardening**
**Threat:** `serialize_vector_into` in `src/core/property.rs` panicked when vector dimensions exceeded `MAX_VECTOR_DIMENSIONS`. If triggered by user input during serialization, this could cause a Denial of Service (DoS) by crashing the application thread (or process if panic=abort).
**Defense:** Deprecated `serialize_vector_into` and updated internal usage to use the fallible `try_serialize_vector_into`. Updated tests to verify that `try_serialize_vector_into` returns an error instead of panicking on oversized vectors.
