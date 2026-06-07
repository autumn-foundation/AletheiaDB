# 🔭 Vantage Spec: Chimera Lexicographical String Comparison

| Metadata | Details |
| :--- | :--- |
| **ID** | SPEC-021 |
| **Status** | 🔍 Review |
| **Owner** | Vantage (Product) |
| **Implementer** | Core (Engineering) |
| **Priority** | P2 (Medium) |
| **Related Code** | `src/experimental/reasoning/chimera.rs` |

## 1. 👤 User Stories

> **As a** Data Scientist,
> **I want to** merge string properties using deterministic lexicographical comparisons (Min/Max) during entity synthesis,
> **So that** I can automatically resolve conflicting categorical data or labels (e.g., keeping the earliest alphabetical rank) when combining multiple similar nodes into a single chimera node.

## 2. 🧐 The "So What?" (Business Value)

Currently, the Chimera Engine falls back to keeping the first value (`KeepA` behavior) when it encounters string properties under Min or Max merge strategies. This is explicitly marked as a `TODO: Implement string comparison logic if needed.` in the codebase.

**The Gap:**
- **Data Inconsistency:** When merging properties, users expect Min/Max to evaluate string values alphanumerically (e.g., "Active" < "Inactive"). Silently falling back to `KeepA` produces unpredictable outcomes that violate the expected behavior of these operators.
- **Developer Friction:** Users cannot rely on native strategies to handle string-based categorical variables properly and must build external data-cleaning steps before using the Chimera Engine.

**ROI:**
- **Predictability:** Ensures the synthesis engine honors user-defined strategies consistently across data types.
- **Completeness:** Plugs a known gap in the experimental reasoning capabilities, reducing friction for early adopters of the Chimera feature.

## 3. ✅ Acceptance Criteria

### Functional Requirements

1. **Lexicographical Comparison:**
   - When the `PropertyMergeStrategy` is set to `Min` and both properties are strings, the engine MUST return the string that is lexicographically first (e.g., "Apple" over "Banana").
   - When the strategy is `Max`, the engine MUST return the string that is lexicographically last.
2. **Fallback Behavior:**
   - If the strings are identical, either value can be returned.
   - If only one value is a string and the other is numeric or missing, it MUST gracefully fallback to the existing default behavior without panicking.

### Non-Functional Requirements

- **Metric Definition:** Success = String comparisons during a 10,000-node synthesis operate with < 5% overhead compared to numeric comparisons.

## 4. 🚫 Out of Scope (Phase 1)

- **Locale-Specific Collation:** Implementing complex, language-specific sorting rules (e.g., dictionary order ignoring accents).
- **Semantic String Merging:** Using LLMs or embeddings to find the semantic "middle" or "max" of two sentences.
