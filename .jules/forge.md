## src/db.rs is a God Object
**Learning:** `src/db.rs` contains over 1900 lines and mixes core database API logic with persistence management details (Tracker, Background Thread, Helper functions). This violates Single Responsibility Principle.
**Action:** Extract persistence logic into `src/storage/index_persistence/` modules (`tracker`, `worker`, `operations`).

## src/query/parser.rs God Function
**Learning:** `parse_primary_predicate` in `src/query/parser.rs` was a 100+ line function mixing multiple predicate parsing logic (EXISTS, string ops, IN, comparison), making it hard to read and maintain.
**Action:** Extracted specific predicate logic into helper functions (`parse_exists_predicate`, `parse_string_predicate`, etc.) to flatten the structure and improve readability.

## 2026-03-10 - Refactored parse_entry_at and load_indexes_startup
**Learning:** `src/storage/wal/segment_reader.rs` contained `parse_entry_at`, a 450+ line function with a giant match block for different WAL operations. `src/storage/index_persistence/operations.rs` contained `load_indexes_startup`, a nearly 300-line function with massive loops. This violated the Single Responsibility Principle and made the code hard to read.
**Action:** Extracted the logic for each WAL operation type into smaller, private helper functions (`parse_create_node_op`, `parse_create_edge_op`, etc.) and extracted the loops in `load_indexes_startup` into `restore_nodes_startup` and `restore_edges_startup`. Flattening the structure dramatically improved readability without changing behavior.
