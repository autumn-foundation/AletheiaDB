## src/db.rs is a God Object
**Learning:** `src/db.rs` contains over 1900 lines and mixes core database API logic with persistence management details (Tracker, Background Thread, Helper functions). This violates Single Responsibility Principle.
**Action:** Extract persistence logic into `src/storage/index_persistence/` modules (`tracker`, `worker`, `operations`).

## src/query/parser.rs God Function
**Learning:** `parse_primary_predicate` in `src/query/parser.rs` was a 100+ line function mixing multiple predicate parsing logic (EXISTS, string ops, IN, comparison), making it hard to read and maintain.
**Action:** Extracted specific predicate logic into helper functions (`parse_exists_predicate`, `parse_string_predicate`, etc.) to flatten the structure and improve readability.

## src/query/parser.rs Recurring Anti-Pattern
**Learning:** Duplicate parsing logic for literals in `parse_property_value` and `parse_literal_expression` leads to potential inconsistency and maintenance burden. Also manual loops are used instead of `while let` for simple iterator-like parsing.
**Action:** Extracted `parse_value` and `parse_float_value` helpers. Replaced manual loop with `while let`.
