## src/db.rs is a God Object
**Learning:** `src/db.rs` contains over 1900 lines and mixes core database API logic with persistence management details (Tracker, Background Thread, Helper functions). This violates Single Responsibility Principle.
**Action:** Extract persistence logic into `src/storage/index_persistence/` modules (`tracker`, `worker`, `operations`).

## src/query/parser.rs God Function
**Learning:** `parse_primary_predicate` in `src/query/parser.rs` was a 100+ line function mixing multiple predicate parsing logic (EXISTS, string ops, IN, comparison), making it hard to read and maintain.
**Action:** Extracted specific predicate logic into helper functions (`parse_exists_predicate`, `parse_string_predicate`, etc.) to flatten the structure and improve readability.

## src/query/executor/mod.rs God Function
**Learning:** `execute_op` in `src/query/executor/mod.rs` was a 249-line "God Function" containing a massive match block for `PhysicalOp`. Large implementations for `HnswSearch` and `SimilarToNode` arms made it very hard to read.
**Action:** Extracted the complex match arms into descriptive private helper methods (`execute_hnsw_search`, `execute_similar_to_node`). This dramatically flattened the match block, improved readability, and enforced strict type-checking on the separated domains.
**Replace Duplicate Comparison Code**
**Learning:** In `src/query/converter.rs`, swapping operators in a massive match block was duplicated. Refactoring with a single tuple extraction drastically reduced code bloat.
**Action:** Use tuple extraction and conditional assignment `(key, value, flipped_op)` to reuse common building logic rather than duplicating the construction blocks.

**Remove Redundant PartialEq Implementation**
**Learning:** In `src/query/planner/rules/operation_reordering.rs`, there was an 80-line manual implementation of `predicates_equal`. The `Predicate` enum already derived `PartialEq`, making the manual code dead weight.
**Action:** Always check if a struct/enum derives standard traits like `PartialEq`, `Eq`, or `Default` before writing custom structural equality or initialization functions.

**Extract Closure Logic to Flatten Graph Traversal**
**Learning:** In `src/query/executor/iterators.rs`, `TraversalIterator::get_neighbors` had four massive, nested branches duplicating the same filtering closure. Extracting this to `process_outgoing` and `process_incoming` methods flattened the pyramid of doom.
**Action:** When closures are duplicated across match arms (especially when returning different iterator types), extract the closure's inner logic into a private `#[inline]` method on the struct.
