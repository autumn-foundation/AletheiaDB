# Elenchus Journal - Verdicts & Patterns

**[Ghost Identity]**
**Module:** `src/core/graph.rs`
**Severity:** 🔴 Critical
**Finding:** `Node` and `Edge` structs derive `PartialEq`, but no tests verify this equality contract. A mutation replacing `PartialEq` with "always true" or "only check ID" would pass all existing tests.
**Evidence:** Code inspection reveals zero `assert_eq!(node1, node2)` assertions. Existing tests only check individual fields.
**Recommendation:** Add `test_node_equality` and `test_edge_equality` to `sentry_tests` verifying strict structural equality (including properties and metadata).

**[Vague Reflections]**
**Module:** `src/core/graph.rs`
**Severity:** 🟡 Suspect
**Finding:** Debug implementation tests use `contains()` assertions, verifying only that substrings exist.
**Evidence:** `test_node_debug` asserts `debug_str.contains("Person")` but would pass `format!("Person")` which loses all structural info.
**Recommendation:** Strengthen assertions to match the full expected `Debug` string format, checking struct fields.
