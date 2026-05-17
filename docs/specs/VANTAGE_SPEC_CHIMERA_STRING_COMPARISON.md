# 🔭 Vantage: Spec for Chimera String Comparison

## 👤 User Story
**As a** Data Scientist using Chimera for reasoning and synthesis,
**I want** Chimera to be able to logically compare string properties (like identifying the lexicographically "minimum" or "maximum" string),
**so that** I can synthesize nodes correctly even when their distinguishing features are textual rather than purely numeric.

## 🧐 The "So What?" Ask
**What business problem does this solve?**
Chimera currently handles numeric properties perfectly when merging or synthesizing data. However, when comparing string values (e.g., finding the "latest" status alphabetically, or resolving conflicts between textual categories), it simply falls back to returning the first value. This results in unpredictable synthesized output for text fields, forcing users to write custom cleanup code down the pipeline. By implementing deterministic string comparison, we improve the reliability and accuracy of the reasoning engine, making it useful for a wider range of enterprise data types.

**Metric Definition:**
- **Accuracy:** 100% of string comparison operations in Chimera produce the lexicographically correct result (Min or Max).
- **Correctness:** 0% fallback to arbitrary "first value" logic when standard comparison operators are requested on strings.

**Gap Analysis:**
- *Current State:* `src/experimental/reasoning/chimera.rs` contains a `TODO: Implement string comparison logic if needed.` when both properties are strings. It just returns `a`.
- *Required State:* Implement logic to compare two string properties lexicographically and return the appropriate one based on whether the operation is conceptually a "Min" or "Max".

## ✅ Acceptance Criteria
- Must implement string comparison in `Chimera::merge_comparative`.
- Must properly identify if the `op` represents a "Min" or "Max" operation and return the lexicographically smaller or larger string accordingly.
- Must add a unit test in `src/experimental/reasoning/chimera.rs` verifying string comparison behavior for Min/Max operations.
- Must preserve existing numeric merging logic.

## 🚫 Out of Scope (Phase 1)
- Complex semantic string comparison (e.g., using LLMs or embeddings to find the "best" string). Phase 1 is strictly lexicographical.
- Case-insensitive or locale-aware comparisons (Phase 2). Standard Rust string comparison is sufficient for Phase 1.
