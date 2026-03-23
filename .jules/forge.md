## src/db.rs is a God Object
**Learning:** `src/db.rs` contains over 1900 lines and mixes core database API logic with persistence management details (Tracker, Background Thread, Helper functions). This violates Single Responsibility Principle.
**Action:** Extract persistence logic into `src/storage/index_persistence/` modules (`tracker`, `worker`, `operations`).

## src/query/parser.rs God Function
**Learning:** `parse_primary_predicate` in `src/query/parser.rs` was a 100+ line function mixing multiple predicate parsing logic (EXISTS, string ops, IN, comparison), making it hard to read and maintain.
**Action:** Extracted specific predicate logic into helper functions (`parse_exists_predicate`, `parse_string_predicate`, etc.) to flatten the structure and improve readability.

## src/query/executor/mod.rs God Function
**Learning:** `execute_op` in `src/query/executor/mod.rs` was a 249-line "God Function" containing a massive match block for `PhysicalOp`. Large implementations for `HnswSearch` and `SimilarToNode` arms made it very hard to read.
**Action:** Extracted the complex match arms into descriptive private helper methods (`execute_hnsw_search`, `execute_similar_to_node`). This dramatically flattened the match block, improved readability, and enforced strict type-checking on the separated domains.

## src/storage/wal/segment_reader.rs God Function
**Learning:** `parse_entry_at` in `src/storage/wal/segment_reader.rs` was a 450-line "God Function" containing a massive match block for WAL operations (`CreateNode`, `CreateEdge`, etc.). Large implementations made it very hard to read and mentally trace the offset bounds checking.
**Action:** Extracted the complex match arms into descriptive private helper methods (`parse_create_node`, `parse_create_edge`, etc.). This dramatically flattened the match block, improved readability, and enforced strict bounds checking in isolated functions.
