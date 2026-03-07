# 🔭 Vantage Spec: Vector Search Phase 5: Streaming and Incremental Updates (The "Agility" Dimension)

| Metadata | Details |
| :--- | :--- |
| **ID** | SPEC-012 |
| **Status** | 📝 Draft |
| **Owner** | Vantage (Product) |
| **Implementer** | Nova (Engineering) |
| **Priority** | P1 (High) |
| **Related Code** | `src/experimental/` |

## 1. 👤 User Stories

> **As a** Machine Learning Engineer,
> **I want to** incrementally update the vector index with new embeddings as data flows in,
> **So that** my semantic search results reflect the absolute latest real-world events without requiring a full re-index.

> **As a** Fraud Analyst monitoring a transaction stream,
> **I want to** immediately search against new node embeddings the moment they are ingested,
> **So that** I can detect emerging attack vectors in near real-time.

> **As a** System Operator,
> **I want to** stream updates into the index without blocking read queries,
> **So that** the database remains highly available and responsive during continuous data ingestion.

## 2. 🧐 The "So What?" (Business Value)

Currently, AletheiaDB's vector search index is robust for static or slowly-changing datasets. However, real-world data pipelines (like news feeds, social media, or financial transactions) are continuous streams.

**The Gap:**
- **Staleness:** If we only batch-update the index periodically, our search results are out-of-date by the interval duration.
- **Downtime/Latency:** Full re-indexing is computationally expensive and can block or degrade concurrent read performance.
- **Developer Friction:** Users have to manually manage batching and indexing schedules outside the database.

**ROI:**
- **Real-Time AI:** Unlocks use cases that require instant semantic relevance (e.g., breaking news clustering, real-time recommendation engines).
- **Reduced Operational Overhead:** Eliminates the need for complex external orchestration to manage index rebuilds.
- **Competitive Advantage:** Matches or exceeds the capabilities of dedicated vector databases while maintaining our unique bi-temporal graph advantages.

## 3. ✅ Acceptance Criteria

### Functional Requirements

1.  **Incremental Ingestion**:
    -   Must support adding, updating, and deleting individual vector embeddings in the index without requiring a full rebuild.
2.  **Read-While-Write**:
    -   Incoming vector updates must not block concurrent semantic search queries (`SIMILAR TO`).
3.  **Consistency**:
    -   Once a transaction committing a new vector is acknowledged, that vector must be immediately discoverable by subsequent search queries (Read-Your-Writes).
4.  **Temporal Awareness**:
    -   Incremental updates must respect the bi-temporal model (valid time and transaction time) ensuring historical queries remain accurate.

### Non-Functional Requirements
-   **Ingestion Throughput**: Must sustain a continuous streaming ingestion rate of at least 1,000 vectors per second per node.
-   **Search Latency Impact**: Concurrent streaming updates must degrade search latency by no more than 10% (p99).

## 4. 🚫 Out of Scope (Phase 1)

-   **Distributed Streaming Coordination**: Complex multi-node ingestion routing and consensus are deferred. Phase 1 focuses on single-node streaming mechanics.
-   **Automated Index Compaction**: While incremental updates are supported, sophisticated background garbage collection or index re-balancing is out of scope for the initial MVP.
-   **Streaming Search Subscriptions**: Pushing search result changes to clients (e.g., via WebSockets) is not included; clients still pull data via queries.

## 5. 📝 Gap Analysis (Current vs. Spec)

| Feature | Current State | Required State | Action |
| :--- | :--- | :--- | :--- |
| **Index Updates** | Batch rebuilds / static | Incremental, mutable index | Implement mutable HNSW or similar dynamic index structure |
| **Concurrency** | Read/Write contention | Non-blocking read-while-write | Adopt concurrent data structures for the index |
| **WAL Integration** | Basic logging | Streaming index recovery | Ensure incremental index ops are durably logged and replayable |
