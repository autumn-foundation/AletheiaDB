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
