# Temporal Associative Retrieval (TAR)

**Spec ID**: FEATURE-001
**Status**: Draft
**Owner**: Vantage (Product Manager)
**Date**: 2024-05-22

## 1. Executive Summary

Temporal Associative Retrieval (TAR) enables users to find "relevant context" for an entity or concept by combining vector similarity search with temporal graph traversal. It answers questions like "What was related to Project X in 2023?" by first finding nodes semantically similar to "Project X" (Vector Phase) and then exploring their structural connections (Graph Phase) as they existed at that specific time (Temporal Phase).

This feature formalizes and productizes the experimental "Fishing" module into a core database capability, critical for Retrieval-Augmented Generation (RAG) applications where context is distributed across both semantic and structural relationships.

## 2. User Stories

### 2.1 The Intelligence Analyst
> "As an Intelligence Analyst investigating a financial crime, I want to find all entities related to 'Suspect A' (even if the name is spelled slightly differently), including those connected via shared identifiers like phone numbers or addresses, but only considering connections that were valid during the time of the alleged crime (last year), so that I can build a precise timeline of events without noise from irrelevant modern connections."

### 2.2 The LLM Agent
> "As an AI Agent answering user queries about historical events, I want to retrieve a subgraph of information centered around the user's topic, expanding from direct semantic matches to structurally related concepts (e.g., 'If the user asks about the 2008 crash, also give me the regulations that changed in 2009'), so that I have the complete context to generate an accurate and comprehensive answer."

### 2.3 The Fraud Detector
> "As a Fraud Detection System, I want to score transactions not just by their individual risk features, but by their proximity in the graph to known fraudulent nodes, respecting the time of the transaction, so that I can catch ring fraud where multiple accounts share a device or IP address within a short time window."

## 3. Problem Statement

Current "Hybrid Query" capabilities in AletheiaDB (and most vector/graph DBs) are optimized for **"Filter-then-Rank"**:
1.  Start at a specific node or match a pattern.
2.  Traverse relationships.
3.  Rank the resulting nodes by vector similarity.

This misses the **"Retrieve-Expand-Rank"** pattern needed for associative memory:
1.  **Retrieve**: Start with a vague concept (Vector) and find multiple potential entry points (Top-K similar nodes).
2.  **Expand**: From *all* these entry points, traverse the graph to find structurally related nodes (2-3 hops).
3.  **Rank**: Score *all* visited nodes based on a combination of their semantic similarity to the query, their graph distance from the entry points, and their temporal relevance.

The experimental `fishing` module attempts this but lacks:
-   **Temporal Integration**: It ignores `as_of` queries, returning current graph structure even if the user asks about the past.
-   **Multi-hop Expansion**: It is hardcoded to depth 1.
-   **Configurable Scoring**: Weights are manual and hard to tune.
-   **Performance**: It does a linear scan for "freshness" and lacks index support for the expansion phase.

## 4. Proposed Solution

We will introduce a new core API endpoint `associative_search` (and corresponding AQL operator) that executes a **Temporal Associative Retrieval** algorithm.

### 4.1 Algorithm Overview

1.  **Vector Phase (Seed Generation)**:
    -   Input: `query_vector` (or `seed_node_id`), `limit_k`.
    -   Action: Perform an HNSW vector search to find the top `k` nodes most similar to the query.
    -   *Crucial*: If `as_of` time is specified, this must use the vector index state at that time (or filter results by validity).

2.  **Graph Phase (Expansion)**:
    -   Input: `seed_nodes`, `depth`, `edge_filter` (optional).
    -   Action: Perform a Breadth-First Search (BFS) starting from *all* seed nodes simultaneously.
    -   Constraint: Only traverse edges that were **valid** at `as_of` time.
    -   Output: A set of visited nodes with their geodesic distance (hops) from the nearest seed.

3.  **Scoring Phase (Ranking)**:
    -   Input: All visited nodes.
    -   Action: Calculate a relevance score for each node `n`:
        $$ Score(n) = W_v \cdot Sim(n, q) + W_g \cdot \frac{1}{1 + Dist(n, seeds)} + W_t \cdot Recency(n, t) $$
        -   `Sim(n, q)`: Vector similarity to query (if `n` has a vector, otherwise 0 or inherited from seed).
        -   `Dist(n, seeds)`: Minimum hops from a seed node (0 for seeds themselves).
        -   `Recency(n, t)`: Decay function based on how close the node's last update was to `as_of` time (optional).

4.  **Return Phase**:
    -   Return top `N` nodes by score.
    -   Include **Provenance**: "Why is this here?" (e.g., "Linked to Node X (Sim 0.9) via Edge Y").

### 4.2 Interface Design (Draft)

#### Logical Inputs
- **Seed**: A vector embedding (e.g., from an LLM query) or a specific Node ID.
- **As Of Time**: The historical timestamp to query against (optional, defaults to now).
- **Initial Candidates (k)**: Number of similar nodes to retrieve from the vector index (e.g., 10).
- **Expansion Depth**: How many hops to traverse from the seed nodes (e.g., 2).
- **Weights**: Relative importance of Vector Similarity vs. Graph Distance vs. Temporal Recency.

#### Logical Outputs
- List of **Scored Nodes**, where each result contains:
    - The Node data.
    - A Relevance Score (0.0 to 1.0).
    - **Provenance Metadata**: An explanation of *why* this node was retrieved (e.g., "Linked to Seed X via Edge Y").

#### AQL Syntax (Proposed)
```cypher
-- Find context for "Project X" as of 2023
AS OF '2023-01-01T00:00:00Z'
ASSOCIATIVE SEARCH FROM $embedding
  SEEDS 10
  DEPTH 2
  WEIGHTS { vector: 1.0, graph: 0.5, recency: 0.1 }
LIMIT 50
```

## 5. Functional Requirements

1.  **Bi-Temporal Support**: The expansion MUST respect the `as_of` timestamp. Edges created after this time MUST NOT be traversed. Edges deleted before this time MUST NOT be traversed.
2.  **Vector Index Integration**: The seed generation MUST use the HNSW index efficiently.
3.  **Configurable Depth**: Users MUST be able to specify depth (default 1, max e.g. 5).
4.  **Edge Filtering**: Users MUST be able to restrict expansion to specific edge labels (e.g., `["KNOWS", "WORKS_WITH"]` only).
5.  **Scoring Customization**: Weights for vector, graph, and temporal factors MUST be adjustable.

## 6. Non-Functional Requirements

1.  **Latency**:
    -   Seed Phase: < 10ms (standard HNSW).
    -   Expansion Phase (Depth 2, avg degree 10): < 50ms.
    -   Total P99 Latency: < 100ms for typical queries.
2.  **Scalability**: Must handle expansion starting from 100+ seeds without blowing up memory (visited set management).
3.  **Safety**: Max depth and max visited nodes limits to prevent DoS.

## 7. Roadmap & Phasing

-   **Phase 1 (MVP)**:
    -   Implement `AssociativeQuery` struct and handler.
    -   Linear scan for "Recency" score (no index support yet).
    -   Single-threaded BFS expansion.
    -   Expose via Rust API only.

-   **Phase 2 (Optimization)**:
    -   Parallel BFS expansion.
    -   AQL syntax support.
    -   MCP Tool integration (`associative_search`).

-   **Phase 3 (Advanced)**:
    -   "Concept Drift" scoring (using Temporal Vectors to see how meaning changed).
    -   Bi-directional expansion (meet-in-the-middle).

## 8. Success Metrics

-   **Utility**: Can retrieve a "Context Subgraph" that contains >80% of relevant nodes for a standard benchmark dataset (e.g., HotPotQA mapped to graph).
-   **Performance**: Sub-100ms response time for 2-hop queries on 1M node graph.
