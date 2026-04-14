# 🔭 Vantage: Spec for Vector Search Phase 5 (Streaming & Incremental Updates)

## 1. 👤 User Story

> **As an** AI Application Developer,
> **I want to** incrementally update my vector indexes as new data arrives and stream semantic query results,
> **So that** my application can maintain real-time accuracy without suffering from massive latency spikes due to full index rebuilds, and can return first-tokens to users immediately.

## 2. 🧐 The "So What?" (Business Value)

Currently, AletheiaDB supports hybrid vector + graph queries, but updating the vector index relies heavily on full snapshots and bulk operations.
When dealing with large-scale knowledge bases (e.g., millions of documents), rebuilding the HNSW index or loading the entire result set into memory is a bottleneck.

**The Gap:**
- **Writes:** Changing a single embedding shouldn't require rebuilding massive portions of the index. Real-time RAG applications require low-latency "upserts".
- **Reads:** Waiting for a full k-NN + graph traversal to complete before returning the *first* result ruins the Time-To-First-Token (TTFT) for generative UI.

**ROI:**
- **Performance:** Reduces write latency for vector updates, directly improving write throughput (Operations/Sec).
- **User Experience (DX & End-User):** Enables real-time streaming interfaces for LLMs (crucial for modern chat applications).
- **Operational Cost:** Lowers memory overhead by processing result sets in a streaming fashion.

## 3. ✅ Acceptance Criteria

### Functional Requirements
1.  **Incremental Index Updates**:
    - The vector index must support atomic, low-latency "upserts" and "deletes" for individual embeddings without triggering a full index rebuild.
    - Soft deletes must be efficiently garbage-collected or physically removed from the index structure over time.
2.  **Streaming Query API**:
    - Queries (both purely vector and hybrid graph+vector) must support returning a stream/iterator of results rather than a materialized `Vec`.
    - Must support pipelined execution where vector results are yielded as they are found/ranked.
3.  **Persistence Guarantees**:
    - Incremental updates must be durably persisted (e.g., appended to a WAL or differential file) so that crashes do not lose recent vector updates.

### Non-Functional Requirements (Constraints)
-   **Performance Metrics (Success Definition):**
    - Single vector update (insert/update/delete) latency: `< 1ms` (for 384-dim vectors on a 1M node graph).
    - Time-To-First-Result (Streaming latency): `< 5ms` for the first yielded result in a hybrid query.
-   **Storage Overhead:**
    - Incremental index structures must consume `< 20%` additional memory/disk overhead compared to the base HNSW index.

## 4. 🚫 Out of Scope (Phase 5)
-   **Distributed Sharding of Vector Indexes**: Partitioning the HNSW index across multiple physical machines is reserved for a future phase (Phase 6 or Sharding Phase 2).
-   **Hardware Acceleration (GPU/TPU)**: Moving distance calculations or indexing to specialized hardware.
-   **Alternative Index Types**: Support for inverted file indexes (IVF) or Product Quantization (PQ) is out of scope (we stick to HNSW for now).
