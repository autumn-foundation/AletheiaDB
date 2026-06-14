# 🔭 Vantage Spec: Chimera String Synthesis

| Metadata | Details |
| :--- | :--- |
| **ID** | SPEC-022 |
| **Status** | 🔍 Review |
| **Owner** | Vantage (Product) |
| **Implementer** | Nova (R&D) |
| **Priority** | P2 (Medium) |
| **Related Code** | `src/experimental/reasoning/chimera.rs` |

## 1. 👤 User Stories

> **As a** Data Scientist simulating organizational changes,
> **I want to** gracefully handle string properties when synthesizing two nodes (e.g., merging "Sales" and "Marketing"),
> **So that** the resulting chimera node has meaningful string data rather than just silently picking one or dropping the property entirely.

> **As an** ML Engineer augmenting training data,
> **I want to** configure how string properties are combined (e.g., concatenation, picking the longest, or using an LLM to summarize),
> **So that** my synthetic nodes retain semantic context from both parent nodes.

## 2. 🧐 The "So What?" (Business Value)

Currently, the `ChimeraEngine`'s `merge_numeric` logic falls back to simply cloning the string from the first node (Node A) when it encounters string properties, leaving a `TODO: Implement string comparison logic if needed` in the codebase.

**The Gap:**
- **Data Loss:** When merging two nodes with distinct string properties (e.g., descriptions, names), the second node's string data is completely lost.
- **Incomplete Feature:** The synthesis engine is only truly effective for numeric and vector data, severely limiting its use cases for text-heavy graphs.

**ROI:**
- **Broader Applicability:** Makes Chimera useful for text-rich datasets (e.g., document synthesis, entity resolution).
- **Flexibility:** Empowers users to define exactly *how* they want text to merge, catering to diverse domains.

## 3. ✅ Acceptance Criteria

### Functional Requirements

1.  **String Merge Strategies:**
    - The `SynthesisConfig` MUST be updated to support specific strategies for merging string properties.
    - Supported strategies MUST include at least:
      - `Concatenate(separator)`: Joins the two strings with a given separator (e.g., "A, B").
      - `PreferLongest`: Keeps the string with the most characters.
      - `PreferA` / `PreferB`: Explicitly favors one parent (the current default behavior).
2.  **Integration with `merge_numeric` (or equivalent):**
    - The `TODO` in `src/experimental/reasoning/chimera.rs` MUST be addressed.
    - When two string properties are encountered, the engine MUST apply the configured string merge strategy instead of hardcoding a fallback to Node A.
3.  **Fallback Behavior:**
    - If a string merge strategy is not explicitly configured, it SHOULD default to a safe, documented behavior (e.g., `PreferA` or a simple concatenation).

### Non-Functional Requirements

-   **Performance:** Simple string operations (concatenation, length comparison) must not significantly degrade the synthesis performance compared to numeric merging.
-   **Metric Definition:** Success = String synthesis strategy is applied correctly without panics, and the resulting string matches the expected output of the chosen strategy.

## 4. 🚫 Out of Scope (Phase 1)

-   **LLM-based Semantic Merging:** Using an LLM to semantically blend or summarize two strings (e.g., merging "Develops software" and "Sells products" into "Develops and sells software products"). This requires the MCP server or external API integration and is deferred to Phase 2.
-   **Complex Data Types:** Deep merging of JSON objects or arrays of strings. Phase 1 focuses strictly on scalar `Property::String` values.

## 5. 📝 Gap Analysis (Current vs. Spec)

| Feature | Current State | Required State | Action |
| :--- | :--- | :--- | :--- |
| **String Merge** | Hardcoded to return Node A | Configurable via strategies | Implement string strategies |
| **Config** | `SynthesisConfig` lacks string options | Supports `StringMergeStrategy` | Update `SynthesisConfig` |
