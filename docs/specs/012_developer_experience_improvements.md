# 🔭 Vantage Spec: Developer Experience (DX) Improvements for First-Time Users

| Metadata | Details |
| :--- | :--- |
| **ID** | SPEC-012 |
| **Status** | 🔍 Review |
| **Owner** | Vantage (Product) |
| **Implementer** | Forge/Echo (Engineering) |
| **Priority** | P1 (High) |
| **Related Code** | `src/core/interning.rs`, `src/lib.rs`, `README.md` |

## 1. 👤 User Stories

> **As a** new developer exploring AletheiaDB,
> **I want to** print a node's label and see the actual string (e.g., "Person") instead of an internal memory optimization structure (`Interned(11)`),
> **So that** I can easily verify my data without learning the internal architecture on day one.

> **As a** beginner Rust developer following the documentation,
> **I want to** copy and paste examples without encountering cryptic trait import errors (`no method named 'create_node' found`) or missing feature flags,
> **So that** I experience a "magic" first 5 minutes that builds confidence in the library.

## 2. 🧐 The "So What?" (Business Value)

A recent DX audit (by "Echo") highlighted that AletheiaDB exposes its internal optimizations too aggressively to new users.

**The Gap:**
- **Leaky Abstractions**: Users printing `node.label` see `Interned(11)` instead of `"Person"`. Fixing this currently requires importing the global interner and manually resolving the string, which is highly confusing for a simple task.
- **Trait Import Tax**: The core transaction methods (`create_node`, etc.) require importing the `WriteOps` trait, which is often missed when copying code snippets.
- **Feature Flag Confusion**: Example code utilizing experimental features (like `nova`) fails to compile when copy-pasted without clear, inline instructions on required `Cargo.toml` updates.

**ROI:**
- **Faster Onboarding**: Reducing friction in the first 5 minutes of usage drastically increases the probability of adoption. "Simple" beats "Powerful" when evaluating a new tool.
- **Reduced Support Burden**: Fewer GitHub issues and questions regarding basic usage, missing traits, or "broken" examples.
- **Improved Perception**: A database that "just works" out of the box is perceived as higher quality and more reliable.

## 3. ✅ Acceptance Criteria

### Functional Requirements

1.  **Transparent Label Display**:
    - The `InternedString` struct MUST implement the `std::fmt::Display` trait.
    - When formatted using `{}`, an `InternedString` MUST attempt to resolve and print its underlying string value (e.g., "Person"). If resolution fails (which should be rare/impossible in normal operation), it should fallback gracefully (e.g., to `<unknown:11>`).
    - Alternatively, provide a highly visible, ergonomic `.to_string()` or `.as_str()` method directly on the `Node` and `Edge` structs that handles the resolution automatically.

2.  **Zero-Friction Imports (The Prelude)**:
    - The `aletheiadb::prelude::*` module MUST re-export the `WriteOps` and `ReadOps` traits.
    - Users MUST be able to call transaction methods (like `tx.create_node(...)`) immediately after importing the prelude, without explicitly importing the specific traits.

3.  **Self-Documenting Examples**:
    - All code snippets in `README.md` and public API documentation (`src/lib.rs` module docs) that rely on optional features (especially `nova`) MUST include an inline comment immediately preceding the code.
    - Example: `// Requires the "nova" feature in Cargo.toml`

### Non-Functional Requirements
-   **Performance**: Implementing `Display` for `InternedString` must not degrade the performance of the core `Debug` implementation or the performance of string interning itself. It is understood that `Display` incurs a resolution lookup, but this is acceptable for user-facing output.
-   **Metric Definition**: Success = A naive user can copy the `story_demo` snippet into a new project, run it, and print `node.label` to see "Person" with zero compilation errors or unexpected internal representations.

## 4. 🚫 Out of Scope (Phase 1)

-   **Redesigning the Interner**: We are not removing `InternedString` or changing how memory is optimized. We are only changing how it is *displayed* to the user.
-   **Automatic Feature Enablement**: We are not attempting to write build scripts or macros that automatically add features to the user's `Cargo.toml`. Documentation is sufficient for Phase 1.
-   **Complete API Rewrite**: We are not flattening the transaction API to remove the need for traits entirely; re-exporting them in the prelude is the designated solution.

## 5. 📝 Gap Analysis (Current vs. Spec)

| Feature | Current State | Required State | Action |
| :--- | :--- | :--- | :--- |
| **InternedString Display** | Implements `Debug` (`{:?}`) returning `Interned(X)`. No `Display`. | Implements `Display` returning resolved string. | Add `Display` impl for `InternedString` using `GLOBAL_INTERNER`. |
| **WriteOps Import** | Not in prelude. Requires explicit `use aletheiadb::WriteOps;`. | Exported in `aletheiadb::prelude`. | Update `src/lib.rs` prelude exports. |
| **Feature Docs** | Blockquotes above snippets in README. | Inline code comments within snippets. | Update `README.md` examples. |
