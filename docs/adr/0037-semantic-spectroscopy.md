# ADR-0037: Semantic Spectroscopy (Prism)

**Status:** Accepted
**Date:** 2026-05-25
**Deciders:** AletheiaDB Core Team
**Categories:** experimental, vector-search, explainability

## Context

Vector databases suffer from the "Black Box" problem. When a query returns a result with a similarity score (e.g., 0.89), the user knows *that* the items are similar, but not *why* or *in what way*.

In high-stakes domains (finance, legal, intelligence), "It's close in the 1536-dimensional space" is not an acceptable explanation. Users need to understand the semantic components that contribute to that similarity.

For example, a document might be similar to "Machine Learning" because it discusses:
1.  Technical algorithms?
2.  Business applications?
3.  Ethical concerns?

We need a way to decompose a dense vector into human-interpretable "spectral lines" or facets.

## Decision

We will implement **Semantic Spectroscopy** (codenamed "Prism") in `src/experimental/prism.rs`.

Prism allows users to project arbitrary vectors onto a set of named **Axes** (Concepts).
The result is a **Spectrum**: a mapping of `Concept -> Intensity`.

Key capabilities:
1.  **Define Axes**: Users can define axes by providing a reference vector (e.g., the embedding for the word "Risk") or a reference Node.
2.  **Orthogonalization**: Prism can automatically orthogonalize these axes (using Gram-Schmidt) to ensure they represent distinct, non-overlapping concepts.
3.  **Spectral Analysis**: Decomposes any node's vector into these components.
    $$ Score_{axis} = v_{node} \cdot v_{axis} $$
4.  **Evolution**: Can track how a node's spectrum changes over time (e.g., "became more 'Technical' in 2024").

## Consequences

### Positive

*   **Explainable AI (XAI)**: Transforms opaque vectors into transparent, faceted profiles.
*   **Semantic Debugging**: Allows detecting if a model is biased or drifting into unwanted territory (e.g., "Why is this neutral query projecting strongly onto the 'Hate Speech' axis?").
*   **Faceted Search**: Enables filtering search results by semantic dimensions (e.g., "Show me results similar to X, but only on the 'Financial' axis").

### Negative

*   **Subjectivity**: The "meaning" of an axis is entirely defined by the reference vector provided by the user. "Garbage In, Garbage Out".
*   **Dimensionality Loss**: Projecting a high-dimensional vector (1536d) onto a few axes (e.g., 5d) necessarily discards information (the "Residual"). The spectrum is a simplification.
*   **Complexity**: Users must carefully select and potentially orthogonalize axes to get meaningful results. Non-orthogonal axes can lead to confusing "double counting" of semantic energy.

## Implementation Details

*   **Location**: `src/experimental/prism.rs`
*   **Input**: `NodeId` or raw vector.
*   **Configuration**: Users build a `Prism` instance with a set of `Axis` definitions.
*   **Output**: `HashMap<String, f32>` (The Spectrum).
