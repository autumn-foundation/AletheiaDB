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

## Open Risks
- `usearch` FFI boundaries rely on the C++ library behaving correctly regarding pointer validity. We added panic guards for null pointers, but full memory safety depends on `usearch` correctness.
- The `usearch` dependency points to a fork (`madmax983/USearch`). This fork contains Rust-specific fixes (move semantics) not yet in upstream. We have pinned the specific commit to ensure stability, but future upstream security patches will need manual cherry-picking.
- `mmap` usage in `src/storage/wal/segment_reader.rs` is inherently unsafe against external file truncation (SIGBUS risk), though file size is checked before mapping.
