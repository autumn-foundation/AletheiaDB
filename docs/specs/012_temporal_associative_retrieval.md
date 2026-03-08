# 🔭 Vantage Spec: Temporal Associative Retrieval

| Metadata | Details |
| :--- | :--- |
| **ID** | SPEC-012 |
| **Status** | 🔍 Review |
| **Owner** | Vantage (Product) |
| **Implementer** | Nova (Engineering) |
| **Priority** | P1 (High) |
| **Related Code** | `src/experimental/fishing.rs` |

## 1. 👤 User Stories

> **As an** Intelligence Analyst,
> **I want to** find all entities related to a specific suspect based on their semantic similarity and structural graph connections,
> **So that** I can build a precise timeline of events without being overwhelmed by irrelevant modern connections.

> **As an** LLM Agent answering user queries,
> **I want to** retrieve a subgraph of information centered around a topic, expanding from direct semantic matches to structurally related concepts as they existed at a specific time,
> **So that** I have complete and temporally accurate context to generate a comprehensive answer.

> **As a** Fraud Detection System,
> **I want to** score transactions by their graph distance to known fraudulent nodes and semantic similarity, while respecting the time of the transaction,
> **So that** I can catch ring fraud operating within specific historical time windows.

## 2. 🧐 The "So What?" (Business Value)

Current capabilities in AletheiaDB rely on a **"Filter-then-Rank"** pattern for hybrid queries, missing the critical **"Retrieve-Expand-Rank"** pattern needed for true associative memory. The experimental "Fishing" module attempts to solve this but is fundamentally limited:

**The Gap:**
- **No Bi-Temporal Support:** The current `fishing` module completely ignores time travel (`as_of` queries), returning the current graph structure even when querying historical states.
- **Limited Expansion:** Hardcoded to a single hop (Depth 1), preventing multi-hop associative retrieval.
- **Performance Bottlenecks:** Freshness scoring relies on a slow linear scan rather than utilizing indexes.

**ROI:**
- **RAG Dominance:** This feature productizes the experimental "Fishing" module into a core database capability, making AletheiaDB the premier choice for complex Retrieval-Augmented Generation (RAG) applications.
- **Differentiated Value:** Solves the difficult problem of temporally-accurate associative context retrieval, setting us apart from standard vector or graph databases.

## 3. ✅ Acceptance Criteria

### Functional Requirements

1.  **Vector Phase (Seed Generation)**:
    -   Must find the top `k` similar nodes based on a query vector or seed node ID.
    -   Must respect the optional `as_of` timestamp if provided, using the vector index state at that exact time.

2.  **Graph Phase (Expansion)**:
    -   Must perform a Breadth-First Search (BFS) expansion starting from all seed nodes.
    -   Must allow configurable `depth` traversal (e.g., up to 5 hops).
    -   Must enforce bi-temporal correctness: only traverse edges that were valid at the `as_of` time.
    -   Must allow optional filtering by specific edge labels.

3.  **Scoring Phase (Ranking)**:
    -   Must calculate a relevance score combining three weights:
        -   **Vector Similarity** (`vector_weight`)
        -   **Graph Distance** (`graph_weight`): Decreases as hop distance increases.
        -   **Temporal Recency** (`freshness_weight`): Decreases as the time between the node's last update and the `as_of` time increases.
    -   Weights must be fully configurable via the API.

4.  **Return Phase**:
    -   Must return a ranked list of the top `N` nodes based on the combined score.
    -   Must include provenance metadata explaining *why* a node was retrieved (e.g., "Linked to Node X via KNOWS edge").

### Non-Functional Requirements
-   **Performance**: The P99 latency for a 2-hop expansion from 10 seed nodes on a 1M node graph should be < 100ms.
-   **Safety**: Must implement safeguards against combinatorial explosion during expansion (e.g., max visited nodes limit).

## 4. 🚫 Out of Scope (Phase 1)

-   **"Concept Drift" Scoring**: Using Temporal Vectors to score how a node's meaning changed over time.
-   **Bi-directional Expansion**: Meet-in-the-middle associative retrieval.
-   **GraphQL/gRPC Integration**: This will only be exposed via the Rust API, HTTP API (`/query`), and AQL in Phase 1.