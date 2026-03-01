# Warden's Journal

**2025-05-23 - Hardened HTTP Rate Limiting and Input Validation**
**Threat:**
1.  **Panic on Invalid Config:** `RateLimitConfig` allowed zero values for `requests_per_second` and `burst_size`, which caused `actix-governor` to panic at runtime (or startup) when building the limiter. This is a DoS vector if configuration is reloadable or controlled by external inputs.
2.  **Panic on Malformed JSON:** Initially suspected that `unwrap()` calls in `src/http/handlers.rs` would panic on invalid user input. Investigation revealed these were mostly in tests or safely handled by `actix-web` extractors, but explicit validation was reinforced.

**Defense:**
1.  **Strict Validation:** Added `RateLimitConfig::validate()` to enforce strict positivity (> 0) for rate limit parameters.
2.  **Error Propagation:** Refactored `build_rate_limit` in `src/http/server.rs` to return `Result` instead of panicking with `expect`. Errors are now propagated up to `create_server` and `run_server`, allowing graceful failure.
3.  **Verification:** Added `tests/warden_http_panic.rs` to verify that invalid JSON payloads (syntax errors, missing fields, wrong types) return `400 Bad Request` instead of crashing the server.

**2025-05-24 - Async Runtime Starvation Prevention**
**Threat:**
1.  **Blocking Operations in Async Handlers:** HTTP handlers (`FindNode`, `FindNeighbors`, `ExecuteQuery`, `CreateNode`) were executing synchronous, CPU-intensive database operations directly on Actix async worker threads. This allows a DoS attack where a few heavy requests (e.g. large scans or vector builds) starve the event loop, making the server unresponsive to other requests.
2.  **Sleep in Async Context:** `HnswIndex::retry_usearch` used `std::thread::sleep` for backoff. When called from an async task (e.g. via `handle_query`), this blocks the underlying OS thread, reducing the worker pool capacity and potentially deadlocking the runtime if all workers sleep.

**Defense:**
1.  **Offload Blocking Tasks:** Wrapped potentially slow operations in `src/http/handlers.rs` using `actix_web::web::block`, offloading them to a dedicated thread pool.
2.  **Async-Aware Indexing:** Implemented `maybe_block_in_place` helper in `src/index/vector/hnsw.rs`. This automatically detects if it's running in a multi-threaded Tokio runtime and uses `tokio::task::block_in_place` to prevent reactor starvation during vector operations and retries.
3.  **Pagination Limits:** Verified existing `saturating_add` checks for deep pagination in `FindNode` and `FindNeighbors` to prevent memory exhaustion DoS.

**2026-01-03 - Supply Chain Security Update (tokenizers)**
**Threat:**
1.  **Unmaintained Dependency:** `tokenizers` (0.15.2) depended on `number_prefix` (0.4.0) which is unmaintained (RUSTSEC-2025-0119).
2.  **Yanked Dependency:** `wasm-bindgen` (via `reqwest`) depended on `bumpalo` (3.20.0) which was yanked.
3.  **Vulnerability:** CVE-2024-3205 affects `tokenizers` < 0.19.1 (Out-of-bounds Read).

**Defense:**
1.  **Update `tokenizers`:** Updated `tokenizers` to version `0.22` in `Cargo.toml`. This version drops the dependency on `number_prefix` and includes fixes for known vulnerabilities.
2.  **Update Dependencies:** Ran `cargo update` to pull in the latest compatible versions of all dependencies, resolving the yanked `bumpalo` issue in `wasm-bindgen`.
3.  **Verification:** Ran `cargo audit` to confirm the vulnerabilities are resolved. Ran tests (`cargo test --features embedding-onnx`) to ensure no regressions.

**2025-05-25 - Removed unsafe transmute in AdjacencyIndex**
**Threat:** `AdjacencyIndex::import_csr` and `convert_offsets` used `unsafe { Vec::from_raw_parts }` to zero-copy cast `Vec<u64>` to `Vec<NodeId>` and `Vec<usize>`. This bypassed `NodeId::MAX_VALID_ID` invariants and relied on potentially fragile memory layout assumptions, presenting a risk of Undefined Behavior (UB) if a corrupted index was loaded.
**Defense:** Replaced `unsafe` transmute with safe iterator mapping (`into_iter().map().collect()`).
