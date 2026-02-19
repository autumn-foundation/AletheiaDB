## 2024-05-24 - Recursion Depth Limits in Deserialization
**Learning:** Custom recursive deserialization logic (like `TAG_ARRAY`) is vulnerable to Stack Overflow DoS attacks if depth is not limited. Rust's stack overflow protection aborts the process, making it a severe availability risk.
**Action:** Always enforce a `MAX_RECURSION_DEPTH` (e.g., 100) in recursive functions processing untrusted input. Use a helper function with a `depth` parameter.

## 2024-05-24 - False Fallibility in PropertyMapBuilder
**Learning:** API methods like `try_insert_vector` imply fallibility via `Result` but call underlying methods that panic on validation errors (e.g., `PropertyValue::vector`).
**Action:** Document these panic points with `#[should_panic]` tests immediately. In the future, refactor to proper error propagation to match the function signature's promise.

## 2026-02-15 - False Fallibility in JSON Conversion
**Learning:** `serde_json` array conversion to `PropertyValue::Vector` was bypassing `MAX_VECTOR_DIMENSIONS` validation because it constructed the enum variant directly instead of using the validating constructor. Also, `PropertyMapBuilder::insert` panics on error, which is unsafe for public APIs.
**Action:** Always validate dimensions when constructing `Vector` variants manually. Use `try_insert` in API layers and propagate errors.

## 2026-02-15 - Unaligned SIMD Loads
**Learning:** Rust's `Vec<f32>` only guarantees 4-byte alignment, but AVX2 `vmovaps` requires 32-byte alignment. Using aligned load intrinsics on standard Vecs is a segfault ticking time bomb.
**Action:** Always use `loadu` (unaligned load) intrinsics unless alignment is manually enforced and verified. Added rigorous unaligned access tests in `src/core/vector/sentry_tests.rs` to prevent regression to aligned loads.

## [Pre-existing Failure in Parser Recursion Test]
**Learning:** Found a failing test `query::parser::sentry_tests::test_parser_recursion_limit_boundary` while verifying HNSW changes. It seems to fail consistently on the boundary condition (100 nested parens). This indicates an off-by-one error in recursion depth check or test expectation.
**Action:** Logged for future investigation; proceeded with HNSW coverage improvements as they are isolated.

## [Catastrophic Cancellation in Euclidean Distance]
**Learning:** The formula `||a||^2 + ||b||^2 - 2<a,b>` for squared Euclidean distance is numerically unstable for vectors that are very close to each other, leading to negative results due to floating-point errors. This causes `sqrt()` to return `NaN`.
**Action:** Replaced with the stable single-pass algorithm `sum((a_i - b_i)^2)` in `sparse_squared_euclidean_distance`. This is both numerically robust and faster (1 pass vs 3). Added deterministic regression test with seed 34.

## 2026-03-01 - Thread-Local Stripe Affinity Bug
**Learning:** `ConcurrentWal` cached stripe indices in a global `thread_local!` variable. This index was tied to the `num_stripes` of the *first* WAL instance accessed by the thread. When the same thread accessed a second WAL instance with fewer stripes (common in test suites or multi-tenant setups), the cached index could exceed the bounds of the new `stripes` vector, causing a panic.
**Action:** Changed `THREAD_STRIPE_ID` to cache the `thread_id.hash()` (u64) instead of the calculated stripe index. The stripe index is now re-calculated on every access (`hash & stripe_mask`), which is cheap and always correct for the current WAL instance's configuration.

## 2026-03-02 - Deadlock in Synchronous WAL Append
**Learning:** `PendingEntry` was intended to implement `Drop` to notify waiters of errors if the entry was discarded (e.g., buffer full or panic), but the implementation was missing. This caused `CompletionHandle::wait()` to hang indefinitely if the entry was dropped before completion.
**Action:** Implemented `Drop` for `PendingEntry` to check `!notifier.is_complete()` and notify an error. Also updated tests to access `PendingEntry` fields by reference since `Drop` prevents moving fields out.
