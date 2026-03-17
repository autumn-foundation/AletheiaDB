# ADR-0064: Breaking VersionMetadata Dependency Cycle

**Status:** Accepted
**Date:** 2025-03-10
**Deciders:** AletheiaDB Core Team
**Categories:** architecture, naming, domain

## Context

Both `src/index/temporal.rs` and `src/core/version.rs` contained structures named `VersionMetadata`.

This dual naming caused a subtle but pervasive naming collision and semantic ambiguity across the codebase, a symptom of the "Leak" architectural smell. The `index` module's struct was specific to tracking temporal index timelines, whereas the `core` module's struct defined the global domain representation of a version's metadata (e.g., timestamps, authorship).

This ambiguity led to confusing imports, frequent need for aliasing (`use index::temporal::VersionMetadata as IndexVersionMetadata`), and made it difficult for new developers to distinguish between a core domain primitive and a specific index implementation detail.

## Decision

We have decided to aggressively break the semantic overlap and enforce clear domain boundaries by renaming the index-specific struct.

### Key Changes

1.  **Rename `VersionMetadata` in `index`:** Renamed `src/index/temporal.rs::VersionMetadata` to `TimelineVersionMetadata`.
2.  **Update Aliases:** Updated the associated index aliases and usages throughout the codebase to reflect this new, context-specific name.

## Consequences

### Positive

-   **Semantic Clarity:** The name `TimelineVersionMetadata` explicitly communicates its purpose: it serves temporal timelines within the index layer, differentiating it clearly from the global domain `VersionMetadata` in `core`.
-   **Enforced Boundaries:** Eliminates the naming collision, making it impossible to accidentally import the index implementation detail when intending to use the core domain primitive.
-   **Improved Readability:** Removes the necessity for confusing import aliases throughout the code.

### Negative

-   **Refactoring Effort:** Required a targeted find-and-replace across multiple files and test suites.

### Neutral

-   **No Functional Change:** The behavior and memory layout of the structs remain identical; the change is strictly nomenclature to improve architectural transparency.
