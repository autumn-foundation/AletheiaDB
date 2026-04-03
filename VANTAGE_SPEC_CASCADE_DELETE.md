# 🔭 Vantage: Spec for Cascade Delete and Referential Integrity

## 👤 User Story
**As a** Database Administrator or Application Developer,
**I want** the system to either automatically delete associated edges when a node is deleted (cascade delete) or prevent the deletion of a node if it still has connected edges (strict referential integrity),
**so that** I do not end up with orphaned edges pointing to non-existent nodes, which causes data corruption and application errors during graph traversals.

## 🧐 The "So What?" Ask
**What business problem does this solve?**
Currently, when a node is deleted in AletheiaDB, any edges connected to that node remain in the database as "orphans" (as noted in `src/api/transaction/write/tests.rs`). This is a documented limitation but poses a severe risk for data integrity. Applications performing graph traversals will encounter edges pointing to missing nodes, leading to broken assumptions, potential application panics, or silently incorrect query results. Implementing cascade deletes or strict referential integrity ensures that the graph remains in a mathematically consistent state at all times, drastically reducing developer debugging time and preventing critical data anomalies in production applications.

**Success Metric Definition:**
- **Data Integrity:** 100% of deleted nodes result in either the successful deletion of all connected edges (Cascade) or the transaction safely aborting with an error (Restrict).
- **Performance:** Deleting a node with up to 1,000 connected edges completes in <50ms.
- **Developer Experience:** Users can explicitly configure the desired deletion behavior (cascade vs. strict) per transaction or system-wide.

## ✅ Acceptance Criteria
- Must introduce an API or configuration option to define node deletion behavior: `Cascade` (automatically delete incoming and outgoing edges) or `Restrict` (fail the transaction if edges exist).
- Must ensure that in `Cascade` mode, deleting a node also automatically removes all associated current state edges from the adjacency lists and edge storage.
- Must ensure that in `Restrict` mode, an attempt to delete a node with connected edges returns a clearly defined `ReferentialIntegrityError`.
- Must properly handle temporal history; the cascade delete must record the temporal deletion of the affected edges at the exact same transaction time as the node deletion.
- Must execute the cascading deletes (or referential checks) within the same atomic write transaction as the primary node deletion.

## 🚫 Out of Scope
- Cross-shard distributed cascade deletes (Phase 2). MVP focuses on single-shard consistency.
- Deep or chained cascade deletes (e.g., deleting Node A cascades to Edge E1, which cascades to Node B). MVP only cascades from the target Node to its immediate incident Edges.