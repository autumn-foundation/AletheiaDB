# 🔭 Vantage Spec: Cascade Delete & Referential Integrity

| Metadata | Details |
| :--- | :--- |
| **ID** | SPEC-009 |
| **Status** | 🔍 Review |
| **Owner** | Vantage (Product) |
| **Implementer** | Core (Engineering) |
| **Priority** | P1 (High) |
| **Related Code** | `src/api/transaction/write/mod.rs` |

## 1. 👤 User Stories

> **As a** Database Administrator,
> **I want to** ensure that when a node is deleted, all edges connected to it are automatically deleted,
> **So that** my graph does not become polluted with orphaned edges that point to non-existent nodes, ensuring data integrity.

> **As an** Application Developer,
> **I want to** define strict referential integrity rules for specific relationship types,
> **So that** the database prevents me from accidentally deleting a node if it is still being referenced by critical relationships (e.g., stopping the deletion of a "Customer" if they still have active "Order" nodes).

> **As a** Data Engineer running complex ETL pipelines,
> **I want** the database to handle cascading deletes automatically within a single transaction,
> **So that** I do not have to write custom, error-prone application logic to find and delete every connected edge before deleting the target node.

## 2. 🧐 The "So What?" (Business Value)

Currently, AletheiaDB allows a node to be deleted while leaving its associated edges intact in the storage layer. This results in "orphaned edges."

**The Gap:**
- **Data Integrity:** Queries that traverse these orphaned edges may fail or return inconsistent results, breaking the fundamental promise of a graph database.
- **Developer Experience (DX):** Developers are forced to manually query and delete all incident edges before deleting a node. This is tedious, non-atomic (unless explicitly managed in a large transaction), and highly error-prone.
- **Performance/Storage:** Orphaned edges consume storage space and index capacity without providing any value.

**ROI:**
- **Trust & Reliability:** Guarantees that the graph structure is always mathematically valid.
- **Reduced Application Complexity:** Developers write less code to manage state deletions, accelerating feature delivery.
- **Storage Efficiency:** Automatically cleans up dead data, reducing database bloat.

## 3. ✅ Acceptance Criteria

### Functional Requirements

1.  **Default Cascade Delete:**
    - When a `delete_node` operation is executed, the system MUST automatically identify and delete all incoming and outgoing edges connected to that node.
    - This cascading deletion MUST occur within the same transaction as the node deletion, ensuring atomicity (all or nothing).

2.  **Referential Integrity Constraints (Optional/Future-proofing):**
    - The system SHOULD provide a mechanism to configure constraints (e.g., "RESTRICT") that prevents node deletion if certain types of edges are connected.
    - If a deletion violates a constraint, the transaction MUST abort and return a clear `ReferentialIntegrityError`.

3.  **Temporal Consistency:**
    - The cascading deletion MUST respect the bi-temporal model. The deleted edges should be marked as deleted (tombstoned) at the current transaction time, preserving their historical state for `as_of` queries.

### Non-Functional Requirements

-   **Performance:** The overhead of finding and deleting connected edges should be minimal. It must scale efficiently even for "super-nodes" (nodes with millions of edges), potentially requiring batching or asynchronous cleanup under the hood (though logically atomic to the user).
-   **Metric Definition:** Success = `delete_node` on a node with 1,000 edges completes in < 5ms, and 0 orphaned edges remain.

## 4. 🚫 Out of Scope (Phase 1)

-   **Complex Cascade Rules:** "Delete node A, which cascades to node B, which cascades to node C" (Deep cascading node deletions). Phase 1 only cascades to the *edges* immediately connected to the deleted node.
-   **Cross-Shard Cascading:** Handling distributed transactions for cascading deletes across different shards is deferred to Phase 2.
-   **Configurable Edge Policies per Label:** Allowing users to define that `[:OWNS]` cascades but `[:KNOWS]` restricts. For Phase 1, all edges cascade by default.

## 5. 📝 Gap Analysis (Current vs. Spec)

| Feature | Current State | Required State | Action |
| :--- | :--- | :--- | :--- |
| **Node Deletion** | Deletes node only | Deletes node + all incident edges | Update `tx.delete_node` logic |
| **Orphaned Edges** | Allowed (documented in tests) | Strictly prohibited | Implement edge lookup and deletion |
| **Atomicity** | Manual (user must group ops) | Automatic | Bundle edge deletions in the same write log entry |
