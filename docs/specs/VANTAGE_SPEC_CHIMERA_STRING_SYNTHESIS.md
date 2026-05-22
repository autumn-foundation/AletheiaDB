# 🔭 Vantage Spec: Chimera String Synthesis

| Metadata | Details |
| :--- | :--- |
| **ID** | SPEC-021 |
| **Status** | 🔍 Review |
| **Owner** | Vantage (Product) |
| **Implementer** | Nova (Engineering) |
| **Priority** | P2 (Medium) |
| **Related Code** | `src/experimental/reasoning/chimera.rs` |

## 1. 👤 User Stories

> **As a** Data Scientist simulating mergers,
> **I want to** synthesize two nodes with string properties intelligently (e.g., concatenating names or picking the most frequent categorical value),
> **So that** my generated Chimera nodes have meaningful textual data instead of silently falling back to the left node's value.

> **As an** AI Agent building hypothetical scenarios,
> **I want to** configure string merging strategies (Concat, Longest, KeepLeft, KeepRight),
> **So that** I have granular control over how text fields are blended during the hypothetical reasoning process.

## 2. 🧐 The "So What?" (Business Value)

Currently, the `ChimeraEngine` completely ignores string properties during node synthesis. As noted in the codebase (`// TODO: Implement string comparison logic if needed`), it just silently clones the value from node A.

**The Gap:**
- **Data Loss**: Valuable textual information from node B is completely discarded during synthesis.
- **Inaccurate Scenarios**: When generating a Chimera node to represent a "merger" or "blend" of two entities, failing to merge their text fields (like "Description", "Category", or "Tags") leads to poor quality synthetic data.

**ROI:**
- **Higher Quality Synthetic Data**: Makes the Chimera engine vastly more useful for text-heavy knowledge graphs.
- **Feature Completeness**: Closes a known gap/TODO in the experimental reasoning engine, making it a more mature feature.

## 3. ✅ Acceptance Criteria

### Functional Requirements

1.  **String Merge Strategies**:
    -   The `PropertyMergeStrategy` enum (or a new `StringMergeStrategy`) MUST support at least the following string-specific operations:
        -   `Concat(delimiter)`: Joins the two strings with a provided delimiter (e.g., ", " or " & ").
        -   `Longest`: Picks the string with the greater length.
        -   `KeepLeft`: (Current behavior) Always picks the value from node A.
        -   `KeepRight`: Always picks the value from node B.
2.  **Integration with Synthesis**:
    -   When `ChimeraEngine::synthesize` encounters two `PropertyValue::String` (or interned strings), it MUST apply the configured string merge strategy instead of hardcoding `Some(a.clone())`.
3.  **Type Safety & Fallbacks**:
    -   If an unsupported strategy (like `Mean` or `Sum`) is applied to string properties, it MUST gracefully fallback to a safe default (like `KeepLeft`) or return a clear error, rather than panicking.

### Non-Functional Requirements
-   **Metric Definition**: Success = A synthetic node created from "Alpha" and "Beta" with a `Concat(" & ")` strategy correctly has the property value "Alpha & Beta".

## 4. 🚫 Out of Scope (Phase 1)

-   **LLM-based Semantic Blending**: Asking an LLM to read both strings and write a summary (e.g., "Company A makes shoes, Company B makes hats" -> "A combined company making apparel"). That is Phase 2. Phase 1 is purely deterministic text manipulation.
-   **Complex Regex Merging**: Extracting specific patterns to merge.

## 5. 📝 Gap Analysis (Current vs. Spec)

| Feature | Current State | Required State | Action |
| :--- | :--- | :--- | :--- |
| **String Synthesis** | Hardcoded to return node A | Configurable strategy-based merge | Update `ChimeraEngine::merge_properties` |
| **Merge Strategies** | Numeric only (`Mean`, `Max`, etc.) | Numeric + String strategies | Extend `PropertyMergeStrategy` |
