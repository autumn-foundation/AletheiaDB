# 🔭 Vantage Spec: Alchemy (Semantic Graph Transformation)

| Metadata | Details |
| :--- | :--- |
| **ID** | SPEC-018 |
| **Status** | 📝 Draft |
| **Owner** | Vantage (Product) |
| **Implementer** | Nova (Engineering) |
| **Priority** | P2 (Medium) |
| **Related Code** | `src/experimental/alchemy.rs` |

## 1. 👤 User Stories

> **As a** Knowledge Graph Administrator,
> **I want to** automatically link disconnected entities that share highly similar semantic properties,
> **So that** my graph becomes denser and recommendations improve without requiring manual data entry or complex external pipelines.

> **As a** Data Quality Engineer,
> **I want to** automatically merge duplicate nodes that are semantically identical,
> **So that** I can deduplicate data ingested from multiple messy sources and maintain a clean, canonical dataset.

## 2. 🧐 The "So What?" (Business Value)

Graph databases excel at querying relationships, but maintaining the quality and completeness of those relationships often requires manual rules, domain expertise, or external batch processing jobs. This is computationally expensive, brittle, and slow.

**The Gap:**
- **Manual Maintenance:** Users currently have to pull data out, run clustering or similarity scripts in Python, and push new edges or merge commands back in.
- **Lost Insights:** Disconnected nodes that *should* be related remain isolated, degrading the quality of traversal-based analytics and Agent reasoning.

**ROI:**
- **Self-Healing Data:** Alchemy allows the database to enrich its own topology natively. By crystallizing "missing links" (Wormholes) and fusing duplicates (Synonyms), the graph becomes smarter over time.
- **Operational Efficiency:** Eliminates the need for external data-cleansing pipelines for semantic deduplication.

**Success Metric Definition:**
- **Performance (Fusion):** Merging 1,000 highly similar nodes (with their respective edges) completes in < 5 seconds.
- **Value (Crystallization):** Automatically generating "RELATED" edges between disconnected but semantically similar nodes improves 2-hop traversal recall by at least 15% on benchmark datasets.

## 3. ✅ Acceptance Criteria

### Functional Requirements

1.  **Crystallize Wormholes:**
    - The API MUST accept a set of candidate nodes, a similarity threshold, a structural distance limit, and a target edge label.
    - It MUST automatically create new directed edges between pairs of nodes that meet the semantic similarity threshold but lack a structural path within the distance limit.
2.  **Fuse Synonyms:**
    - The API MUST provide a mechanism to identify nodes exceeding a high semantic similarity threshold.
    - It MUST merge these nodes into a single canonical entity, migrating all incoming and outgoing edges to the surviving node.
3.  **Transactional Integrity:**
    - All transformations MUST be executed within a transaction to guarantee ACID properties. If a fusion fails midway, no partial edges should be left dangling.

### Non-Functional Requirements

-   **Graceful Degradation:** Must handle missing vector data without panicking, safely skipping nodes that cannot be semantically compared.

## 4. 🚫 Out of Scope (Phase 1)

-   **Continuous Real-Time Evolution:** Alchemy operations are triggered manually or via scheduled batches in the MVP. Automatic, trigger-based reactions on every write are deferred to Phase 2.
-   **Un-fusing Nodes:** Once nodes are merged via synonym fusion, the operation cannot be automatically reversed. Audit logs of the merge will exist, but an "undo" API is out of scope.
