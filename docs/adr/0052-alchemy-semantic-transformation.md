# 52. Alchemy: Semantic Graph Transformation

Date: 2024-05-24

## Status

Proposed

## Context

Knowledge graphs often suffer from two structural deficiencies:
1.  **Implicit Connectivity**: Nodes that are semantically related (high vector similarity) may not be explicitly connected by edges, causing graph traversals to miss relevant paths.
2.  **Entity Fragmentation**: The same real-world entity may be represented by multiple nodes (synonyms or duplicates) with slightly different properties or vector embeddings, fragmenting the graph's topology and diluting centrality metrics.

Current solutions rely on manual curation or complex ETL pipelines external to the database. We need an internal mechanism to evolve the graph's structure based on its semantic content.

## Decision

We will implement **Alchemy**, a Semantic Graph Transformation Engine, as an experimental module (`src/experimental/alchemy.rs`).

Alchemy provides two core capabilities:

1.  **Crystallize Wormholes**:
    -   Uses the `WormholeDetector` to find pairs of nodes that are semantically similar but structurally distant (or disconnected).
    -   Materializes these latent connections into explicit edges (e.g., `RELATED`) with a `similarity` property.
    -   This "reifies" the vector space into the topological space.

2.  **Fuse Synonyms**:
    -   Identifies nodes with extremely high vector similarity (above a strict threshold, e.g., 0.98).
    -   Merges them using a **Survivor/Victim** pattern:
        -   **Survivor**: The node with the lower ID (heuristic for "older/canonical").
        -   **Victim**: The duplicate node.
    -   Moves all edges (incoming and outgoing) from the Victim to the Survivor.
    -   Deletes the Victim node.

## Consequences

### Positive
-   **Improved Recall**: Traversals can now cross between semantically related subgraphs that were previously disconnected.
-   **Graph Hygiene**: Automatically reduces duplication, consolidating knowledge into single entities.
-   **Dynamic Evolution**: The graph structure evolves to reflect the semantic understanding of the data.

### Negative
-   **Risk of False Positives**: "Crystallizing" edges based purely on vector similarity might create nonsensical connections if the embedding model is misaligned.
-   **Destructive Merges**: Fusing synonyms is a destructive operation. If two distinct entities are merged (e.g., "Apple Inc." and "Apple Fruit") due to vector proximity, data is lost/corrupted. High thresholds are critical.
-   **Performance**: Both operations require significant scanning and transactional write volume.
