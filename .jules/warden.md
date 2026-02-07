# Warden's Security Journal

## 2026-01-28 - Supply Chain & DoS Hardening

**Threat:** Mutable Dependency in Supply Chain
The `usearch` dependency was pointing to a git branch (`fix/rust-move-semantics`). Git branches are mutable, meaning the underlying code could change without warning, potentially introducing malicious code or breaking changes (Supply Chain Attack).

**Defense:** Pinned Dependency
Pinned `usearch` to a specific commit hash (`ca8a0213cc56f43354b35ffc83e12f033993fd4a`) in `Cargo.toml`. This ensures immutable builds and prevents upstream changes from silently affecting the codebase.

**Threat:** Unbounded Loops / Resource Exhaustion in WAL
The `WalRingBuffer` uses a lock-free design with spinning/yielding backpressure. If not implemented correctly, high contention could lead to livelocks or thread starvation (DoS).

**Defense:** Verification & Stress Testing
Implemented `tests/warden_compliance.rs` to simulate a DoS scenario with high-contention writes against a slow consumer.
- Verified that `WalRingBuffer` correctly applies backpressure (sleeping/yielding) without deadlocking.
- Verified that data integrity is maintained even under stress.
- Validated `unsafe` blocks in `WalRingBuffer` and `PropertyMap` (little-endian optimization) as sound.

**Threat:** Missing Security Tooling
`cargo-audit` was not enforced or checked, leaving potential vulnerability scanning to manual processes.

**Defense:** Automated Check Script
Created `scripts/security_audit.sh` to automatically check for and run `cargo-audit` if available. This standardizes the security check process for agents and CI.

## 2026-02-01 - SIMD & Public API Hardening

**Threat:** Buffer Over-read in Unsafe SIMD
The `unsafe` SIMD functions in `src/core/vector.rs` rely on the caller to ensure input slices have equal lengths. If this contract is violated, pointer arithmetic could read past the end of the second buffer (Undefined Behavior).

**Defense:** Assertion Hardening
Added `debug_assert_eq!(a.len(), b.len())` to internal safe wrapper functions in `src/core/vector.rs` (`dot_and_magnitudes`, `squared_diff_sum`, `dot_product_sum`). This enforces the safety contract before entering `unsafe` SIMD blocks (`dot_and_magnitudes_avx2`, etc.), ensuring validation runs on all hardware (including CI runners without AVX) while protecting the unsafe inner functions.

**Threat:** Resource Exhaustion via Public API
Public HTTP and MCP endpoints could be vectors for DoS attacks via large inputs (e.g., massive vectors, deep traversals).

**Verification:** Audit & Stress Test
- **Verified:** `WalRingBuffer` backpressure mechanism passed high-contention stress test (`tests/warden_compliance.rs`).
- **Audited:** `src/mcp/server.rs` and `src/http/handlers.rs`. Confirmed that resource limits are strictly enforced:
  - `MAX_TRAVERSAL_DEPTH` (10)
  - `MAX_RESULT_LIMIT` (10,000)
  - `MAX_VECTOR_K` (1,000)
  - Vector dimensions validated against index configuration.
  - WAL segment reading limits file size to 1GB to prevent memory map exhaustion.

## 2026-02-05 - DoS & Integer Overflow Lockdown

**Threat:** Stack Overflow (DoS) in Query Parser
The recursive descent parser in `src/query/parser.rs` lacked recursion depth limits. A malicious query with deeply nested parentheses (e.g., `((((...))))`) could cause a stack overflow, crashing the server.

**Defense:** Recursion Limit
Enforced `MAX_RECURSION_DEPTH = 100` in `src/query/parser.rs`. Added a `depth` parameter to recursive parsing functions (`parse_predicate` chain) to track and limit nesting level. Verified with `tests/parser_dos.rs`.

**Threat:** Integer Overflow in WAL & Index Loading
Arithmetic operations on file offsets (`src/storage/wal/segment_reader.rs`) and allocation sizes (`src/index/vector/hnsw.rs`) were unchecked. Maliciously crafted files could cause integer overflows, leading to panics (DoS) or logic errors (loading incorrect data).

**Defense:** Checked Arithmetic
- Replaced arithmetic operators with `checked_add` and `checked_mul` in `src/index/vector/hnsw.rs` and `src/storage/wal/segment_reader.rs`.
- Implemented a `macro_rules! add_offset` helper in WAL reader to safely increment offsets and propagate errors on overflow.
- Added explicit error handling for size calculations to prevent panics.

## Open Risks
- `usearch` FFI boundaries rely on the C++ library behaving correctly regarding pointer validity. We added panic guards for null pointers, but full memory safety depends on `usearch` correctness.
- The `usearch` dependency points to a fork (`madmax983/USearch`). This fork contains Rust-specific fixes (move semantics) not yet in upstream. We have pinned the specific commit to ensure stability, but future upstream security patches will need manual cherry-picking.
- `mmap` usage in `src/storage/wal/segment_reader.rs` is inherently unsafe against external file truncation (SIGBUS risk), though file size is checked before mapping.

## 2026-02-12 - SIMD Hardening
**Threat:** Buffer Over-read in Release Builds
Internal SIMD helper functions in `src/core/vector.rs` used `debug_assert_eq!` to check dimension equality. In release builds, this check is stripped, allowing `unsafe` SIMD intrinsics to potentially read past the end of a buffer if dimensions mismatch (Undefined Behavior/Crash).

**Defense:** Runtime Assertion
Replaced `debug_assert_eq!` with `assert_eq!` in `dot_and_magnitudes`, `squared_diff_sum`, and `dot_product_sum`. This ensures that even in optimized release builds, dimension mismatches trigger a safe panic rather than UB. Verified with a regression test `test_internal_safety_in_release`.

## 2026-02-01 - JSON Recursion DoS Hardening
**Threat:** Stack Overflow (DoS) in JSON Parsing and Serialization
The JSON converter `json_to_property_value` and its inverse `property_value_to_json` in `src/http/converters.rs` processed nested arrays recursively without a depth limit. A malicious payload with deeply nested arrays (e.g., `[[[[...]]]]`) could cause a stack overflow during deserialization or serialization, crashing the server.

**Defense:** Recursion Depth Limit
Refactored both functions to use recursive helpers that track depth. Enforced `MAX_JSON_RECURSION_DEPTH = 100` (strictly `>=`). Changed `property_value_to_json` to return `Result` to propagate errors. Verified with unit tests covering deserialization, serialization, and boundary conditions (depth 99 vs 100).

## 2026-02-15 - Vector Index Memory Safety Lockdown

**Threat:** Buffer Over-read in Custom Metrics
The `usearch` library supports quantized vector storage (I8, F16), which reduces memory usage. However, user-defined custom metrics are defined to operate on `f32` slices. When using a custom metric with quantized storage, `usearch` passes pointers to the quantized data (e.g., `i8*`), but the Rust wrapper blindly cast these to `f32*`. This resulted in reading 4x (for I8) or 2x (for F16) more memory than allocated, leading to potential crashes (DoS) or information leakage (reading uninitialized/unrelated memory).

**Defense:** Configuration Validation
Enforced a strict validation rule in `HnswIndexBuilder::build` and `HnswIndex::load`: Custom metrics are now **only** allowed when using `Quantization::F32`. Attempting to combine `custom_metric` with `I8` or `F16` quantization now returns a specific `InvalidVector` error instead of proceeding with unsafe memory access.

**Verification:** Regression Test
Added `tests/security_custom_metric.rs` which attempts to build and load an index with this dangerous combination. Verified that the operation fails safely with the expected error message, whereas previously it would read out-of-bounds memory.

## 2026-02-15 - Race Condition & Unsafe Audit

**Threat:** Race Condition / UB in Environment Variable Tests
Tests in `src/embeddings/providers` were using `unsafe { std::env::set_var(...) }` to mock configuration. Since `set_var` is not thread-safe and tests run in parallel by default, this could lead to data races (Undefined Behavior) and flaky tests across the entire suite.

**Defense:** Dependency Injection for Config
Refactored `OpenAIConfig::from_env` and `HuggingFaceConfig::from_env` to use an internal helper `from_env_with_provider` that accepts a closure for environment lookup.
- Removed all `unsafe` blocks and `std::env::set_var` calls from tests.
- Removed the `Mutex` serialization hack.
- Tests now use a mock closure to simulate environment variables safely.

**Threat:** Potential Buffer Over-read in Vector Deserialization
`src/core/property.rs` contains `unsafe` blocks for optimizing vector serialization/deserialization by casting byte slices to f32 slices. While theoretically sound on little-endian systems, incorrect length checks could lead to buffer over-reads.

**Verification:** Audit & Fuzz Test
- **Audited:** Verified that `serialize_vector_into` and `deserialize_vector` perform strict bounds checking (`data_slice.len() == dimension * 4`) and handle alignment correctly via `Vec::with_capacity`.
- **Documented:** Added "Verified by Warden" comments explaining the safety proofs (validity of f32 bit patterns, alignment guarantees).
- **Verified:** Added `tests/warden_property_safety.rs` to fuzz truncated and malformed inputs, confirming that safe guards trigger before `unsafe` blocks are reached.

## 2026-02-16 - Allocation Bomb DoS Hardening

**Threat:** Allocation Bomb in Property Deserialization
`PropertyMap::deserialize` and `deserialize_sparse_vector` blindly trusted the declared count/size fields from the input buffer.
- `PropertyMap::deserialize` would call `HashMap::with_capacity(count)`. A malicious payload declaring `count = 4_000_000_000` would cause the server to attempt a massive allocation (32GB+), leading to an immediate OOM crash (DoS).
- `deserialize_sparse_vector` had a similar issue with `nnz` (number of non-zero elements).

**Defense:** Capacity Limits
Enforced strict capacity limits during deserialization:
- `PropertyMap`: `count` must be <= `MAX_PROPERTY_MAP_CAPACITY` (10,000).
- `SparseVector`: `nnz` must be <= `MAX_VECTOR_DIMENSIONS` (100,000).

**Verification:** Reproduction Test
Added `tests/warden_dos_repro.rs` which simulates malicious payloads with excessive counts.
- Before fix: Code attempted allocation (and failed with "Buffer too short" or potentially OOM).
- After fix: Code immediately returns `StorageError::CorruptedData` citing the capacity limit, protecting memory resources.

## 2026-02-17 - API DoS Hardening

**Threat:** Unbounded Allocation in FindNeighbors
The `FindNeighbors` endpoint allocated a `Vec` containing all neighbor nodes before serializing them. For highly connected nodes (supernodes), this could cause OOM crashes or massive latency (DoS). It also lacked pagination support.

**Defense:** Pagination & Zero-Allocation Iterators
- Enforced strict pagination in `QueryRequest::FindNeighbors` with `limit` (default 100, max 1000) and `offset`.
- Exposed zero-allocation iterators (`get_outgoing_edges_iter`) in `AletheiaDB` to traverse edges without intermediate allocations.
- Implemented streaming deduplication and pagination pipeline.
- Added safety check for deep pagination (`offset + limit <= 10,000`) to prevent CPU DoS.

## 2026-02-18 - Allocation Amplification DoS Hardening

**Threat:** Allocation Amplification (DoS)
`deserialize_recursive` (for Arrays) and `PropertyMap::deserialize` in `src/core/property.rs` were vulnerable to allocation amplification. They allocated memory based on a `count` field read from the input before verifying that sufficient data existed in the buffer. An attacker could send a tiny payload (e.g., 5 bytes) claiming to have 1,000,000 elements, causing the server to allocate ~16MB. With concurrent requests, this leads to rapid memory exhaustion.

**Defense:** Pre-allocation Validation
Implemented strict validation logic that checks the remaining buffer size against the minimum possible size for the requested number of elements:
- Arrays: `remaining_bytes >= count` (min 1 byte/element).
- Maps: `remaining_bytes >= count * 5` (min 5 bytes/entry).
This ensures memory is only allocated if the client has actually sent a proportional amount of data.

**Verification:** Reproduction Test
Added `tests/repro_allocation_dos.rs` simulating malicious payloads. Confirmed that the new logic returns `StorageError::CorruptedData` ("Insufficient buffer size") *before* attempting allocation.

## 2026-02-18 - Vector Panic Hardening

**Threat:** Denial of Service (Panic) via Large Vectors
`PropertyValue::vector` and `serialize_vector_into` panicked when vector dimensions exceeded `MAX_VECTOR_DIMENSIONS`. `PropertyMapBuilder::try_insert_vector` (which implies fallibility) internally called the panicking `vector` constructor. An attacker could potentially trigger a server crash by supplying an oversized vector through an API endpoint that uses these builders.

**Defense:** Fallible Constructors
- Introduced `PropertyValue::try_vector` and `try_serialize_vector_into` which return `Result` instead of panicking.
- Refactored `try_insert_vector` to use `try_vector` and propagate the error.
- Refactored `serialize_recursive` to use `try_serialize_vector_into`.
- Extracted dimension validation into a DRY helper `validate_vector_dimensions`.
- Existing `vector` and `serialize_vector_into` methods were preserved as panicking wrappers for backward compatibility, but their internal logic now delegates to the safe implementations.

**Verification:** Regression Test
Added `tests/warden_vector_safety.rs` which attempts to insert and serialize oversized vectors using the `try_` methods. Verified that they now return `VectorError::DimensionTooLarge` instead of crashing the test runner.

## 2026-02-18 - Unbounded Allocation DoS Hardening

**Threat:** Unbounded Allocation in JSON-to-Vector Conversion
The `json_to_property_value` function in `src/http/converters.rs` processed JSON arrays of numbers by collecting them into a `Vec<f32>` *before* checking the `MAX_VECTOR_DIMENSIONS` limit. An attacker could send a JSON array with millions of numbers (e.g., 100M elements), causing the server to attempt a large allocation (400MB+) before rejecting it. This could lead to memory exhaustion and denial of service.

**Defense:** Pre-allocation Limit Check
Modified `json_to_property_value_recursive` to check `arr.len()` against `MAX_VECTOR_DIMENSIONS` immediately after verifying that the array contains only numbers and before allocating the vector. If the length exceeds the limit, it returns an error immediately, preventing the allocation.

**Verification:** Reproduction Test
Added `test_json_vector_dimension_allocation_check` in `src/http/converters.rs`. Verified that the function correctly rejects oversized vectors with the expected error message. While the allocation avoidance is structural (verified by code audit), the behavior remains correct (rejection).

**Audit:** Unsafe FFI Boundaries
Audited `src/index/vector/hnsw.rs`, specifically the `unsafe` block in `create_metric_wrapper` which converts raw pointers from `usearch` into Rust slices.
- Confirmed that `dims` is fixed at index creation time.
- Confirmed that `usearch` contract implies valid pointers for the configured dimension.
- Added explicit safety comments documenting these invariants and the reliance on `Quantization::F32` enforcement for custom metrics.
