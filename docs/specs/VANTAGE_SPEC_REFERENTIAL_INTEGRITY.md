# 🔭 Vantage: Spec for Referential Integrity

👤 **User Story:**
As a Database Administrator, I want the system to enforce strict referential integrity or cascade deletes when a node is removed, so that my graph does not become polluted with orphaned edges pointing to deleted nodes.

**The "So What?" (Business Problem):**
Currently, AletheiaDB allows edges to become orphaned if their source or target node is deleted. This compromises data integrity, requires clients to handle unexpected orphaned edges during traversals, and forces developers to write complex cleanup scripts. Enforcing referential integrity guarantees a consistent graph state, improving developer trust and reducing application-side error handling complexity.

✅ **Acceptance Criteria:**
- The system must prevent deletion of a node if strict referential integrity is configured and the node has connected edges, returning a clear error.
- The system must automatically delete all incoming and outgoing edges connected to a node if cascade delete is configured.
- The chosen policy (cascade or strict) must be configurable at the database or label level.
- Read queries (graph traversals) must never encounter orphaned edges.

🚫 **Out of Scope:**
- Soft deletes or archiving of edges (they must be permanently deleted).
- Complex custom constraints beyond standard cascade/restrict behaviors.
