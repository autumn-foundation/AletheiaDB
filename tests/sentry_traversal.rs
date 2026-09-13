//! Sentry Tests for Traversal Logic
//!
//! These tests verify critical edge cases in graph traversal, specifically:
//! 1. Node Isomorphism suppression (cycle detection)
//! 2. Input isolation (history clearing between inputs)
//!
//! These tests were added by Elenchus to address zero unit test coverage for `TraversalIterator`.

#[cfg(test)]
mod tests {
    use aletheiadb::core::id::NodeId;
    use aletheiadb::core::property::PropertyMapBuilder;
    use aletheiadb::query::QueryExecutor;
    use aletheiadb::query::planner::physical::PhysicalOp;
    use aletheiadb::query::planner::physical::PhysicalPlan;
    use aletheiadb::storage::current::CurrentStorage;
    use aletheiadb::storage::historical::HistoricalStorage;
    use parking_lot::RwLock;
    use std::sync::Arc;

    fn create_cycle_graph() -> (
        Arc<CurrentStorage>,
        Arc<RwLock<HistoricalStorage>>,
        NodeId,
        NodeId,
    ) {
        let current = Arc::new(CurrentStorage::new());
        let historical = Arc::new(RwLock::new(HistoricalStorage::new()));

        // Create A and B
        let a = current
            .create_node(
                "Person",
                PropertyMapBuilder::new().insert("name", "A").build(),
            )
            .unwrap();
        let b = current
            .create_node(
                "Person",
                PropertyMapBuilder::new().insert("name", "B").build(),
            )
            .unwrap();

        // A -> B
        current
            .create_edge(a, b, "KNOWS", PropertyMapBuilder::new().build())
            .unwrap();
        // B -> A (Cycle)
        current
            .create_edge(b, a, "KNOWS", PropertyMapBuilder::new().build())
            .unwrap();

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
                min_depth: 2,
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
        assert_eq!(
            rows.len(),
            0,
            "TraversalIterator should enforce Node Isomorphism and suppress cycles"
        );
    }

    #[test]
    fn test_traversal_input_isolation() {
        // 🎯 Target: TraversalIterator state reset (visited.clear())
        // 💣 Risk: History from one input row affecting subsequent rows

        let (current, historical, a, b) = create_cycle_graph();
        let executor = QueryExecutor::new(current, historical);

        let plan = PhysicalPlan {
            root: PhysicalOp::IndexedTraversal {
                input: Box::new(PhysicalOp::NodeLookup {
                    node_ids: vec![a, a],
                }), // Double A
                direction: aletheiadb::query::ir::Direction::Outgoing,
                label: None,
                min_depth: 1,
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
        assert_eq!(
            rows.len(),
            2,
            "Should return result for each input node independently"
        );
        assert_eq!(rows[0].entity.node_id(), Some(b));
        assert_eq!(rows[1].entity.node_id(), Some(b));
    }

    /// Regression test for Issue #3794: the path arena must materialize the
    /// exact same paths the old per-enqueue `Vec<EntityId>` clone produced.
    ///
    /// Graph: A -> B -> D -> E, A -> C -> D (diamond). Node-distinct BFS from
    /// A at depth 3 must emit B, C, D, E with full alternating
    /// [Node, Edge, Node, ...] paths, D reached via B (shortest first), and E
    /// via the same B-side chain.
    #[test]
    fn test_traversal_path_arena_materializes_exact_paths() {
        use aletheiadb::query::executor::EntityId;

        let current = Arc::new(CurrentStorage::new());
        let historical = Arc::new(RwLock::new(HistoricalStorage::new()));

        let mk = |name: &str| {
            current
                .create_node(
                    "Person",
                    PropertyMapBuilder::new().insert("name", name).build(),
                )
                .unwrap()
        };
        let (a, b, c, d, e) = (mk("A"), mk("B"), mk("C"), mk("D"), mk("E"));
        let link = |s, t| {
            current
                .create_edge(s, t, "KNOWS", PropertyMapBuilder::new().build())
                .unwrap()
        };
        let e_ab = link(a, b);
        let e_ac = link(a, c);
        let e_bd = link(b, d);
        let _e_cd = link(c, d);
        let e_de = link(d, e);

        let executor = QueryExecutor::new(current, historical);
        let plan = PhysicalPlan {
            root: PhysicalOp::IndexedTraversal {
                input: Box::new(PhysicalOp::NodeLookup { node_ids: vec![a] }),
                direction: aletheiadb::query::ir::Direction::Outgoing,
                label: None,
                min_depth: 1,
                depth: 3,
                temporal_context: None,
            },
            estimated_cost: Default::default(),
            temporal_context: None,
            parallel: false,
            // `ProvenanceFilterIterator` strips `row.path` when provenance is
            // excluded; this test asserts the exact materialized paths, so
            // provenance must be on for the paths to survive `execute`.
            include_provenance: true,
        };

        let results = executor.execute(plan).expect("Execution failed");
        let rows: Vec<_> = results.collect_all().expect("Collection failed");
        assert_eq!(rows.len(), 4, "expected B, C, D, E");

        let path_of = |row: &aletheiadb::query::executor::QueryRow| {
            row.path.clone().expect("traversal rows carry a path")
        };
        let n = EntityId::Node;
        let edge = EntityId::Edge;

        assert_eq!(rows[0].entity.node_id(), Some(b));
        assert_eq!(path_of(&rows[0]), vec![n(a), edge(e_ab), n(b)]);
        assert_eq!(rows[1].entity.node_id(), Some(c));
        assert_eq!(path_of(&rows[1]), vec![n(a), edge(e_ac), n(c)]);
        // D is node-distinct: reached once, via B (enqueued first).
        assert_eq!(rows[2].entity.node_id(), Some(d));
        assert_eq!(
            path_of(&rows[2]),
            vec![n(a), edge(e_ab), n(b), edge(e_bd), n(d)]
        );
        // E's path extends the D row's chain -- the arena must not alias or
        // truncate shared prefixes.
        assert_eq!(rows[3].entity.node_id(), Some(e));
        assert_eq!(
            path_of(&rows[3]),
            vec![n(a), edge(e_ab), n(b), edge(e_bd), n(d), edge(e_de), n(e)]
        );
    }
}
