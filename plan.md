1. **Analyze the Issue:**
   - The test function `test_calculate_semantic_cost_dimension_mismatch` actually creates nodes and paths. It is an integration-style test testing `find_path`'s behavior when dimension mismatch happens.
   - All tests inside `src/query/hybrid.rs` and `src/query/semantic_pathfinding.rs` import `crate::db::AletheiaDB` and test the public API of these query modules using a fully functional `AletheiaDB`. This directly creates the circular dependency `query` -> `db` -> `query`.

2. **Blueprint the Fix:**
   - We will move all tests from `src/query/hybrid.rs` to `tests/hybrid_query_integration.rs` (or append to existing `tests/hybrid_query.rs`). Let's check `tests/hybrid_query.rs` if it already tests `traverse_and_rank` or similar. We can append it or create a new file `tests/query_hybrid.rs`.
   - We will move all tests from `src/query/semantic_pathfinding.rs` to `tests/semantic_pathfinding.rs` (or append to `tests/sentinel_semantic_pathfinding.rs`). Let's just create `tests/query_semantic_pathfinding.rs`.
   - We'll then remove `use crate::db::AletheiaDB;` from both `src/query/hybrid.rs` and `src/query/semantic_pathfinding.rs` and delete their `mod tests { ... }`.
   - We will verify that `cargo check` and `cargo test` pass.
   - We will verify the circular dependency is broken by re-running our `find_cycles.py` script.

3. **Detailed Steps:**
   - Step 1: Copy the `mod tests` block from `src/query/hybrid.rs` into `tests/query_hybrid_tests.rs`, rewriting `use super::*;` to `use aletheiadb::query::hybrid::*;` and `use aletheiadb::db::AletheiaDB;` etc.
   - Step 2: Copy the `mod tests`, `mod havok_tests` and `mod sentry_robustness_tests` blocks from `src/query/semantic_pathfinding.rs` into `tests/query_semantic_pathfinding.rs`, similarly fixing imports.
   - Step 3: Remove the test blocks from the original `src/` files.
   - Step 4: Add `pub mod query_hybrid_tests;` to tests/mod.rs or just let cargo find them if they are in the root `tests/` directory (Cargo auto-discovers `tests/*.rs`). Wait, `tests/` files in root are auto-discovered as independent integration test binaries.
   - Step 5: Verify the cyclic dependency is gone.
   - Step 6: Complete pre commit steps.
