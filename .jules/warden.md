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
