## src/db.rs is a God Object
**Learning:** `src/db.rs` contains over 1900 lines and mixes core database API logic with persistence management details (Tracker, Background Thread, Helper functions). This violates Single Responsibility Principle.
**Action:** Extract persistence logic into `src/storage/index_persistence/` modules (`tracker`, `worker`, `operations`).

## src/query/parser.rs God Function
**Learning:** `parse_primary_predicate` in `src/query/parser.rs` was a 100+ line function mixing multiple predicate parsing logic (EXISTS, string ops, IN, comparison), making it hard to read and maintain.
**Action:** Extracted specific predicate logic into helper functions (`parse_exists_predicate`, `parse_string_predicate`, etc.) to flatten the structure and improve readability.

## src/http/handlers.rs God Function
**Learning:** `handle_query` in `src/http/handlers.rs` was a single ~290-line function containing deeply nested logic for dispatching and handling five distinct `QueryRequest` operations (`CreateNode`, `GetNode`, `FindNode`, `FindNeighbors`, `ExecuteQuery`). This created a massive "Pyramid of Doom" that violated the single responsibility principle and made it hard to reason about each handler's resource limits and error responses.
**Action:** Flattened the logic by extracting each `QueryRequest` match arm into private asynchronous helper functions (`handle_create_node`, `handle_get_node`, etc.), reducing `handle_query` to a concise routing dispatch function.
