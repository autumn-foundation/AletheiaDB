👤 **User Story:**
- As a Database Administrator, I want to ensure that when a node is deleted, all edges connected to it are automatically deleted, So that my graph does not become polluted with orphaned edges that point to non-existent nodes, ensuring data integrity.
- As an Application Developer, I want to define strict referential integrity rules for specific relationship types, So that the database prevents me from accidentally deleting a node if it is still being referenced by critical relationships (e.g., stopping the deletion of a "Customer" if they still have active "Order" nodes).
- As a Data Engineer running complex ETL pipelines, I want the database to handle cascading deletes automatically within a single transaction, So that I do not have to write custom, error-prone application logic to find and delete every connected edge before deleting the target node.

✅ **Acceptance Criteria:**
- Default Cascade Delete: `delete_node` MUST automatically identify and delete all incoming/outgoing edges in the same transaction.
- Referential Integrity Constraints: The system SHOULD provide a mechanism to configure constraints (e.g., "RESTRICT") that prevents node deletion if certain types of edges are connected.
- Temporal Consistency: The cascading deletion MUST respect the bi-temporal model. The deleted edges should be marked as deleted (tombstoned) at the current transaction time.
- Metric Definition: Success = `delete_node` on a node with 1,000 edges completes in < 5ms, and 0 orphaned edges remain.

🚫 **Out of Scope:**
- Complex Cascade Rules (deep cascading node deletions). Phase 1 only cascades to the edges immediately connected to the deleted node.
- Cross-Shard Cascading: Handling distributed transactions for cascading deletes across different shards is deferred to Phase 2.
- Configurable Edge Policies per Label: Allowing users to define that `[:OWNS]` cascades but `[:KNOWS]` restricts. For Phase 1, all edges cascade by default.
