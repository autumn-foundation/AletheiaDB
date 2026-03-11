# 🔭 Vantage Spec: Vector Search Incremental Updates (Phase 5)

| Metadata | Details |
| :--- | :--- |
| **ID** | SPEC-012 |
| **Status** | 🔍 Review |
| **Owner** | Vantage (Product) |
| **Implementer** | Nova (Engineering) |
| **Priority** | P1 (High) |
| **Related Code** | `src/index/vector/` |

## 1. 👤 User Stories (Jobs to be Done)

> **As an** AI Application Developer (RAG pipeline),
> **I want to** continuously add and update documents (nodes with vector embeddings) in real-time,
> **So that** my search index remains instantly up-to-date without needing to trigger expensive full-index rebuilds.

> **As a** Data Engineer managing a Knowledge Graph,
> **I want to** update the embeddings of existing nodes when my embedding model changes,
> **So that** the changes are reflected in similarity searches immediately while maintaining system performance.

## 2. 🧐 The "So What?" (Business Value)

**The Gap:**
Currently, AletheiaDB supports HNSW vector indexing. However, large-scale vector indices often require full rebuilds or suffer significant degradation when handling continuous, incremental updates (inserts, updates, deletes). This is a major bottleneck for dynamic RAG pipelines where knowledge is constantly evolving.

**ROI:**
- **Performance at Scale:** Avoids the catastrophic performance cliff of rebuilding large HNSW indices.
- **Real-time Utility:** Keeps the semantic search results perfectly synchronized with the transactional graph state in real-time.
- **Competitive Advantage:** Real-time incremental vector indexing is a highly sought-after feature in the vector database space, differentiating AletheiaDB from batch-oriented systems.

## 3. 🎯 Metric Definition

- **Success:**
    - Indexing overhead remains < 1ms per vector during continuous ingest.
    - Vector search latency (< 10ms for 1M vectors) does not degrade by more than 5% after 100,000 incremental updates.
    - Storage overhead for the incremental structures is < 20% of the total index size.

## 4. 📝 Gap Analysis (Current vs. Spec)

| Feature | Current State | Required State | Action |
| :--- | :--- | :--- | :--- |
| **Index Updates** | Handled but potentially expensive at scale | Incremental, fast, low-degradation | Implement incremental graph updates for HNSW |
| **Deletions** | Soft deletes or rebuilds | Efficient tombstoning and garbage collection | Add robust tombstone management |
| **Persistence** | In-memory / basic | Disk-persisted incrementally | Persist incremental changes to avoid long cold starts |

## 5. ✅ Acceptance Criteria

### Functional Requirements

1.  **Incremental Inserts**:
    - Adding a new node with a vector property must incrementally update the HNSW index without a full rebuild.
    - The new vector must be immediately searchable in the same transaction context.

2.  **Incremental Updates**:
    - Updating a node's vector property must correctly remove/tombstone the old vector and insert the new vector into the index.

3.  **Deletions and Tombstoning**:
    - Deleting a node must tombstone its vector in the index.
    - Searches must filter out tombstoned vectors.

4.  **Garbage Collection (Maintenance)**:
    - Must provide a background process or explicit command to optimize the index (compact tombstoned entries and re-balance the graph).

5.  **Persistence**:
    - The incremental changes must be persisted to disk efficiently to ensure fast recovery times (cold starts).

### Non-Functional Requirements
- **Concurrency**: Incremental updates must support high concurrency (e.g., lock-free or highly granular locking) to avoid blocking reads during writes.

## 6. 🚫 Out of Scope (Phase 1)

- **Distributed Incremental Indexing**: Synchronizing incremental vector index updates across multiple shards or replicas (deferred to replication/sharding phases).
- **Auto-tuning Hyperparameters**: Automatically adjusting HNSW parameters (ef_construction, M) on the fly based on workload (users still manually configure these).

## 7. 📅 Execution Plan

1. **Tombstone Strategy**: Implement a robust tombstoning mechanism for deleted/updated vectors within the existing HNSW structure.
2. **Incremental Insert Logic**: Optimize the insertion path to smoothly integrate new vectors into the HNSW graph layers.
3. **Background Compaction**: Develop a background task to prune tombstones and maintain graph quality.
4. **Persistence Layer Updates**: Extend the index persistence mechanism to flush incremental updates to disk without rewriting the entire index file.
