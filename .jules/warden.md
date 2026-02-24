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

**2025-05-25 - DoS Prevention in AQL Execution via Deep Pagination**
**Threat:**
1. **Deep Pagination CPU Exhaustion:** The Query Planner implicitly trusted `SKIP` and `LIMIT` values in AQL queries. A malicious user could submit a query with a massive offset (e.g., `SKIP 100000000`), causing the `LimitIterator` to loop millions of times, consuming excessive CPU cycles on the worker thread. This bypasses the protections previously added to `FindNode` and `FindNeighbors` handlers.

**Defense:**
1. **Strict Limits in Planner:** Introduced `MAX_PAGINATION_LIMIT` (10,000) in `src/query/planner/mod.rs`.
2. **Validation Logic:** Modified `QueryPlanner::unary_to_physical` to validate `UnaryOp::Skip` and `UnaryOp::Limit` values against this limit. Queries exceeding the limit are rejected immediately with `QueryError::InvalidParameter`.
3. **HTTP Status Mapping:** Updated `src/http/handlers.rs` to catch "invalid query parameter" errors and return `400 Bad Request` instead of 500, providing correct feedback to the client.
4. **Verification:** Added `tests/warden_deep_pagination.rs` to confirm that deep pagination attempts are rejected with a client error.
