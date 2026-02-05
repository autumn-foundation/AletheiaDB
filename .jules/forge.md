## src/db.rs is a God Object
**Learning:** `src/db.rs` contains over 1900 lines and mixes core database API logic with persistence management details (Tracker, Background Thread, Helper functions). This violates Single Responsibility Principle.
**Action:** Extract persistence logic into `src/storage/index_persistence/` modules (`tracker`, `worker`, `operations`).

## src/query/parser.rs God Function
**Learning:** `parse_primary_predicate` in `src/query/parser.rs` was a 100+ line function mixing multiple predicate parsing logic (EXISTS, string ops, IN, comparison), making it hard to read and maintain.
**Action:** Extracted specific predicate logic into helper functions (`parse_exists_predicate`, `parse_string_predicate`, etc.) to flatten the structure and improve readability.

## src/core/property.rs is a God Object
**Learning:** `src/core/property.rs` contains over 4000 lines and mixes PropertyValue enum definition, complex recursive serialization/deserialization logic, vector math utilities, and the PropertyMap implementation. This makes it difficult to navigate and test.
**Action:** Extracted deserialization logic into private helper functions (`deserialize_bool`, `deserialize_array`, etc.) to flatten the `deserialize_recursive` function. Future work should consider splitting the file into `value.rs`, `map.rs`, and `serialization.rs`.
