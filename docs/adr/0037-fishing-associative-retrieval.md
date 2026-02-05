# ADR-0037: Fishing Associative Retrieval

**Status:** Accepted
**Date:** 2026-05-20
**Deciders:** GallifreyDB Core Team
**Categories:** retrieval, rag, experimental

## Context

Standard Retrieval Augmented Generation (RAG) pipelines typically rely solely on **Vector Search** to find relevant context. While effective for finding semantically similar text, this approach often misses **structurally related** information.

For example, if a user asks about "Project X's budget", a vector search might find documents containing "budget" and "Project X". However, it might miss the *Team Lead* of Project X, or the *Client* who funded it, because those nodes don't semantically resemble the query "budget".

To provide holistic context, we need a mechanism that simulates **Associative Memory**: recalling one concept should "pull up" related concepts based on both meaning (vectors) and connection (edges).

## Decision

We will implement the **Fishing** module (Associative Retrieval) within the `experimental` feature set.

The algorithm uses a fishing metaphor:

1.  **The Bait:** The input query (a raw vector or an existing node ID).
2.  **Casting (Vector Search):** We first perform a standard vector search to find the "School" (the set of nodes most semantically similar to the bait).
3.  **Spreading the Net (Graph Expansion):** From the "School", we traverse outgoing edges (optionally filtered by label) to catch neighboring nodes.
4.  **The Catch (Scoring):** Results are ranked by a composite score:
    -   `Score = (VectorSimilarity * W_vec) + (IsNeighbor * W_graph) + (Freshness * W_fresh)`

### Freshness Decay

To support temporal relevance, we include a freshness decay function:
`Freshness = 1.0 / (1.0 + AgeInHours)`
This ensures that more recent information is prioritized, which is critical for RAG applications dealing with evolving data.

## Consequences

### Positive

-   **Richer Context:** Retrieves both semantically and structurally relevant nodes, reducing "hallucinations" caused by missing context.
-   **Configurable:** Weights (`W_vec`, `W_graph`, `W_fresh`) can be tuned to favor specific retrieval strategies.
-   **One-Shot API:** simplifies the complex "Search then Traverse" pattern into a single `cast()` call.

### Negative

-   **Performance:** Requires fetching neighbors for every top-k vector result, which increases I/O and latency compared to pure vector search.
-   **Tuning Complexity:** Finding the right balance of weights can be tricky and domain-dependent.

## Compliance

This ADR addresses the "Associative Retrieval" needs of advanced RAG agents.
