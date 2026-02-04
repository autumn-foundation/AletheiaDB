# ADR-0032: Concept Algebra (Semantic Vector Arithmetic)

**Status:** Accepted
**Date:** 2026-05-24
**Deciders:** GallifreyDB Core Team
**Categories:** architecture, experimental, vector-search, semantic-analysis

## Context

Standard vector databases support "Similarity Search" (Find nearest neighbors to vector V). However, vector embeddings often capture semantic meaning in their spatial relationships.
Researchers and users increasingly want to perform **Semantic Arithmetic** on these embeddings to derive new insights or perform "Reasoning by Analogy".

Common examples include:
*   **Analogy:** "King" - "Man" + "Woman" = "Queen"
*   **Composition:** "Paris" + "Capital" = "France" (roughly)
*   **Negation:** "Apple" - "Fruit" = "Technology Company" (context dependent)

Currently, users must extract vectors, perform this math in Python/NumPy, and then re-query the database. This round-trip is inefficient and breaks the "Database as a Reasoning Engine" paradigm.

## Decision

We will implement **Concept Algebra** as a first-class experimental feature in `src/experimental/concept_algebra.rs`.

This module exposes a `ConceptAlgebra` tool that allows:
1.  **Vector Arithmetic on Nodes**: Treating a Node as a handle for its underlying vector.
2.  **Operations**: `Add`, `Subtract`, `Analogy` (A - B + C), and `Mean` (Centroid).
3.  **In-Database Execution**: The math happens within the Rust process, and the resulting vector is immediately used to query the HNSW index for nearest neighbors.

The API treats Nodes as "Concepts". For example:
```rust
let algebra = ConceptAlgebra::new(&db);
let queen = algebra.analogy(king_id, man_id, woman_id, 1)?;
```

## Consequences

### Positive

*   **Performance**: Eliminates network round-trips for fetching vectors and sending back query vectors.
*   **Expressivity**: Enables a new class of "Semantic Queries" that can be composed.
*   **Abstraction**: Hides the complexity of vector retrieval, normalization, and dimension matching from the user.

### Negative

*   **Vector Quality Dependency**: Garbage In, Garbage Out. Arithmetic only works well with high-quality, normalized embeddings (like OpenAI text-embedding-3 or similar).
*   **Ambiguity**: The result of vector arithmetic is a point in space, not necessarily a valid concept. The "Nearest Neighbor" might be semantically distant if the result lands in a sparse region of the vector space.
*   **Complexity**: Requires handling vector dimension mismatches and missing properties gracefully.

## Implementation Details

*   **Location**: `src/experimental/concept_algebra.rs`
*   **Dependencies**: Uses the internal `HnswIndex` for neighbor search.
*   **Normalization**: Currently assumes vectors are pre-normalized or that the distance metric (Cosine) handles it appropriately. Future versions might enforce L2 normalization on load.
