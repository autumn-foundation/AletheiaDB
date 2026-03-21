## src/db.rs is a God Object
**Learning:** `src/db.rs` contains over 1900 lines and mixes core database API logic with persistence management details (Tracker, Background Thread, Helper functions). This violates Single Responsibility Principle.
**Action:** Extract persistence logic into `src/storage/index_persistence/` modules (`tracker`, `worker`, `operations`).

## src/query/parser.rs God Function
**Learning:** `parse_primary_predicate` in `src/query/parser.rs` was a 100+ line function mixing multiple predicate parsing logic (EXISTS, string ops, IN, comparison), making it hard to read and maintain.
**Action:** Extracted specific predicate logic into helper functions (`parse_exists_predicate`, `parse_string_predicate`, etc.) to flatten the structure and improve readability.

## src/query/executor/mod.rs God Function
**Learning:** `execute_op` in `src/query/executor/mod.rs` was a 249-line "God Function" containing a massive match block for `PhysicalOp`. Large implementations for `HnswSearch` and `SimilarToNode` arms made it very hard to read.
**Action:** Extracted the complex match arms into descriptive private helper methods (`execute_hnsw_search`, `execute_similar_to_node`). This dramatically flattened the match block, improved readability, and enforced strict type-checking on the separated domains.

## src/storage/wal/flush_coordinator.rs God Function
**Learning:** `flush` in `src/storage/wal/flush_coordinator.rs` was a 160-line God Function with deep nesting for error handling and phantom commit rollback logic.
**Action:** Extracted core write logic into `write_entries_to_buffer` and critical sync/rollback logic into `sync_to_disk_with_rollback`. This drastically simplified the main logic flow.
