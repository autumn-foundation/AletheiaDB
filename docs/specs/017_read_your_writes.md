# 🔭 Vantage Spec: Read-Your-Writes for Edge Traversal

| Metadata | Details |
| :--- | :--- |
| **ID** | SPEC-017 |
| **Status** | 📝 Draft |
| **Owner** | Vantage (Product) |
| **Implementer** | Core (Engineering) |
| **Priority** | P1 (High) |
| **Related Code** | `src/api/transaction/write/mod.rs` |

## 1. 👤 User Stories

> **As an** Application Developer,
> **I want to** immediately read the edges I just created within the same transaction before committing,
> **So that** I can write complex business logic (like traversing a newly built sub-graph to compute a metric) without having to prematurely commit and expose partial state.

> **As a** Data Engineer,
> **I want to** ensure that cascading deletes remove *all* connected edges, including those I created earlier in the exact same transaction,
> **So that** my graph does not become polluted with orphaned edges due to transaction buffering limitations.

## 2. 🧐 The "So What?" (Business Value)

Currently, AletheiaDB provides "read-your-writes" for Nodes (finding buffered nodes), but **not** for Edge traversals (`get_outgoing_edges`, `get_incoming_edges`). This is explicitly documented as a LIMITATION in `src/api/transaction/write/mod.rs`.

**The Gap:**
- **Data Integrity:** Because edge traversals only query the committed state and ignore the active write buffer, operations like cascading deletes will fail to delete edges created within the *same* transaction. This results in orphaned edges violating referential integrity.
- **Developer Experience (DX):** Developers expect standard ACID transactions. Not being able to "see" your own recent edge inserts breaks the principle of least astonishment.

**ROI:**
- **Trust & Reliability:** Guarantees absolute referential integrity, even within complex, multi-step transaction workflows.
- **Predictability:** Makes the transaction model fully ACID compliant from the user's perspective.

## 3. ✅ Acceptance Criteria

### Functional Requirements

1.  **Buffer Traversal:**
    - The edge traversal methods (`get_outgoing_edges`, `get_incoming_edges`, `get_outgoing_edges_with_label`) MUST combine results from both the committed state AND the transaction's active write buffer.
2.  **State Consolidation:**
    - If an edge exists in storage but was marked as deleted in the buffer, it MUST NOT be returned in the traversal.
    - If a new edge was added in the buffer, it MUST be included in the traversal.
    - If an edge is created and then deleted within the same transaction, it MUST NOT be returned in the traversal.
3.  **Cascade Delete Fix:**
    - The cascading delete operation MUST correctly identify and delete all edges connected to the target node, including edges created within the same active transaction.

### Non-Functional Requirements

-   **Performance:** Merging results from the buffer should not significantly degrade traversal latency.
-   **Metric Definition:** Success = A transaction that creates 100 edges to a node and immediately calls `delete_node_cascade` on it leaves exactly 0 orphaned edges.

## 4. 🚫 Out of Scope (Phase 1)

-   **Historical Edge Traversal (`as_of`):** Read-your-writes only applies to the current active transaction state, not simulating historical branch logic within the buffer.
-   **Cross-Transaction Visibility:** Transactions remain isolated until committed. This spec only addresses intra-transaction visibility.

## 5. 📝 Gap Analysis (Current vs. Spec)

| Feature | Current State | Required State | Action |
| :--- | :--- | :--- | :--- |
| **Read-Your-Writes (Nodes)** | Supported | Supported | No change |
| **Read-Your-Writes (Edges)** | Not supported (Limitation) | Fully supported | Update traversal logic to include the active write buffer |
| **Cascade Delete Integrity** | Misses same-tx edges | Captures all edges | Automatically fixed by traversal update |
