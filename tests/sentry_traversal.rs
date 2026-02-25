//! Sentry Tests for Traversal Logic
//!
//! These tests verify critical edge cases in graph traversal, specifically:
//! 1. Node Isomorphism suppression (cycle detection)
//! 2. Input isolation (history clearing between inputs)
//!
//! These tests were added by Elenchus to address zero unit test coverage for `TraversalIterator`.

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use parking_lot::RwLock;
    use aletheiadb::core::id::NodeId;
    use aletheiadb::core::property::PropertyMapBuilder;
    use aletheiadb::storage::current::CurrentStorage;
    use aletheiadb::storage::historical::HistoricalStorage;
    use aletheiadb::query::planner::physical::PhysicalOp;
    use aletheiadb::query::QueryExecutor;
    use aletheiadb::query::planner::physical::PhysicalPlan;

    fn create_cycle_graph() -> (Arc<CurrentStorage>, Arc<RwLock<HistoricalStorage>>, NodeId, NodeId) {
        let current = Arc::new(CurrentStorage::new());
        let historical = Arc::new(RwLock::new(HistoricalStorage::new()));

        // Create A and B
        let a = current.create_node("Person", PropertyMapBuilder::new().insert("name", "A").build()).unwrap();
        let b = current.create_node("Person", PropertyMapBuilder::new().insert("name", "B").build()).unwrap();

        // A -> B
        current.create_edge(a, b, "KNOWS", PropertyMapBuilder::new().build()).unwrap();
        // B -> A (Cycle)
        current.create_edge(b, a, "KNOWS", PropertyMapBuilder::new().build()).unwrap();

        (current, historical, a, b)
    }

    #[test]
    fn test_traversal_cycle_node_isomorphism() {
        // 🎯 Target: TraversalIterator cycle suppression logic
        // 💣 Risk: Infinite loops if cycles aren't detected, or confusing results if user expects Relationship Isomorphism.
        // AletheiaDB implements Node Isomorphism (nodes are visited at most once per traversal).

        let (current, historical, a, _b) = create_cycle_graph();
        let executor = QueryExecutor::new(current, historical);

        // Plan: Start at A, Traverse 2 hops OUTGOING.
        // Path exists: A -> B -> A.
        let plan = PhysicalPlan {
            root: PhysicalOp::IndexedTraversal {
                input: Box::new(PhysicalOp::NodeLookup { node_ids: vec![a] }),
                direction: aletheiadb::query::ir::Direction::Outgoing,
                label: None,
                depth: 2,
                temporal_context: None,
            },
            estimated_cost: Default::default(),
            temporal_context: None,
            parallel: false,
            include_provenance: false,
        };

        let results = executor.execute(plan).expect("Execution failed");
        let rows: Vec<_> = results.collect_all().expect("Collection failed");

        // Node Isomorphism:
        // Depth 0: A (Visited: {A})
        // Depth 1: B (from A->B) (Visited: {A, B})
        // Depth 2: A (from B->A) -> Rejected because A is in Visited.
        assert_eq!(rows.len(), 0, "TraversalIterator should enforce Node Isomorphism and suppress cycles");
    }

    #[test]
    fn test_traversal_input_isolation() {
        // 🎯 Target: TraversalIterator state reset (visited.clear())
        // 💣 Risk: History from one input row affecting subsequent rows

        let (current, historical, a, b) = create_cycle_graph();
        let executor = QueryExecutor::new(current, historical);

        let plan = PhysicalPlan {
            root: PhysicalOp::IndexedTraversal {
                input: Box::new(PhysicalOp::NodeLookup { node_ids: vec![a, a] }), // Double A
                direction: aletheiadb::query::ir::Direction::Outgoing,
                label: None,
                depth: 1,
                temporal_context: None,
            },
            estimated_cost: Default::default(),
            temporal_context: None,
            parallel: false,
            include_provenance: false,
        };

        let results = executor.execute(plan).expect("Execution failed");
        let rows: Vec<_> = results.collect_all().expect("Collection failed");

        // Should return B twice (once for each input A).
        // If state leaked, the second A might see 'B' as visited (if visited wasn't cleared) or similar issues.
        assert_eq!(rows.len(), 2, "Should return result for each input node independently");
        assert_eq!(rows[0].entity.node_id(), Some(b));
        assert_eq!(rows[1].entity.node_id(), Some(b));
    }
}
