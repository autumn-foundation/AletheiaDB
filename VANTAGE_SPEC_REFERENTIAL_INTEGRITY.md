# 🔭 Vantage: Spec for Referential Integrity (Cascade Delete)

## 👤 User Story
**As a** Database Administrator or Backend Developer,
**I want** to automatically clean up orphaned edges when a node is deleted,
**so that** I don't have dangling references, corrupt graph traversals, or wasted storage space that break my application logic.

## 🧐 The "So What?" Ask
**What business problem does this solve?**
Orphaned edges lead to data corruption and unexpected application behavior during graph traversal. Currently, deleting a node in AletheiaDB leaves connected edges in the database, breaking the fundamental contract of a graph database where an edge must connect two valid nodes. By implementing strict referential integrity (specifically Cascade Delete), we reduce application-level complexity (developers don't have to manually delete edges) and guarantee a consistent state, reducing debugging time and support tickets related to "ghost" data.

**Metric Definition:**
- **Success:** 0 orphaned edges remaining in the system immediately after a node deletion transaction commits.
- **Latency:** Node deletion latency impact should be <10% overhead compared to current non-cascading deletion for nodes with <100 edges.

**Gap Analysis:**
Modern graph databases (like Neo4j) enforce referential integrity by default—you cannot delete a node if it has connected edges unless you explicitly detach/cascade delete. Our current system allows silent creation of orphaned edges (documented as a known limitation in our write tests), which is an anti-pattern and a significant gap compared to market standards.

## ✅ Acceptance Criteria
- When a node is deleted, all incoming and outgoing edges connected to that node must be automatically deleted in the same transaction.
- Attempting to traverse from a remaining node across an edge that previously pointed to the deleted node must return no result (the edge must not exist).
- Must be an atomic operation: either the node and all its edges are deleted, or the transaction fails and rolls back.
- Must not panic if a node to be deleted has no edges.

## 🚫 Out of Scope
- Soft deletes (Phase 2).
- Configurable referential integrity modes (e.g., `RESTRICT` vs `CASCADE`) - MVP will enforce Cascade Delete by default to prevent orphans.
