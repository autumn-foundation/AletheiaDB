# VANTAGE SPEC: Referential Integrity

## 👤 User Story
As a Database Administrator, I want the database to enforce referential integrity so that I do not have orphaned edges referencing deleted nodes.

## ❓ So What?
What business problem does this solve?
Currently, when a node is deleted, any edges that referenced it become "orphaned" but still exist in storage (as documented in `src/api/transaction/write/tests.rs`). This leads to data inconsistency, potential application crashes when traversing edges to nowhere, and storage bloat from zombie data. Enforcing referential integrity ensures data consistency and prevents application logic errors.

## 🎯 Metric Definition
- Success = 0 orphaned edges allowed in the system after a transaction completes.
- Query performance penalty for write transactions involving node deletion should be < 5%.

## 🔍 Gap Analysis
Standard graph databases (like Neo4j) and relational databases (via FOREIGN KEY constraints) inherently support cascade deletes or strict referential integrity checks to prevent orphaned relationships. Our current implementation lacks this core guarantee, making it less reliable for strict data modeling compared to standard alternatives.

## ✅ Acceptance Criteria
- If a transaction attempts to delete a node that is referenced by existing edges, it must either:
  - Fail the transaction (Strict mode).
  - Automatically delete all referencing edges (Cascade mode).
- The behavior (Strict vs Cascade) must be configurable per transaction or system-wide.
- Attempting to create an edge referencing a non-existent node must fail the transaction.
- Existing tests (e.g. `tx2 commit should succeed - edge addition doesn't create version conflict on node2`) must be updated to reflect the new strict referential integrity behavior.

## 🚫 Out of Scope
- Automatic bidirectional cascade deletes (e.g., deleting an edge deletes the nodes).
- Background garbage collection of existing historical orphaned edges (only focusing on active transactional integrity).
