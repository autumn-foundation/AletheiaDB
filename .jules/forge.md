## src/db.rs is a God Object
**Learning:** `src/db.rs` contains over 1900 lines and mixes core database API logic with persistence management details (Tracker, Background Thread, Helper functions). This violates Single Responsibility Principle.
**Action:** Extract persistence logic into `src/storage/index_persistence/` modules (`tracker`, `worker`, `operations`).

## src/query/parser.rs God Function
**Learning:** `parse_primary_predicate` in `src/query/parser.rs` was a 100+ line function mixing multiple predicate parsing logic (EXISTS, string ops, IN, comparison), making it hard to read and maintain.
**Action:** Extracted specific predicate logic into helper functions (`parse_exists_predicate`, `parse_string_predicate`, etc.) to flatten the structure and improve readability.

**MCP Server Argument Deserialization**
**Learning:** `src/mcp/server.rs` contained 29 `handle_*` methods for tools, each duplicating the exact same boilerplate code for `serde_json::from_value(args)` match blocks and `self.error_json` formatting.
**Action:** Created a `parse_args<T: DeserializeOwned>` helper method to centralize deserialization and error generation, reducing boilerplate and enforcing consistent error handling.
