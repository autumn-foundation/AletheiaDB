//! Sentry Tests for Temporal Batch API
//!
//! These tests verify that the batch APIs (`get_nodes_at_time` and `get_edges_at_time`)
//! correctly enforce the temporal constraints (valid_time and transaction_time),
//! rather than just acting as a bulk lookup that ignores time.

#[cfg(test)]
mod tests {
    use aletheiadb::AletheiaDB;
    use aletheiadb::core::property::PropertyMapBuilder;
    use std::thread::sleep;
    use std::time::Duration;

    #[test]
    fn test_batch_api_enforces_temporal_bounds() {
        let db = AletheiaDB::new().unwrap();

        // T0: Before creation
        let t0 = aletheiadb::core::temporal::time::now();
        sleep(Duration::from_millis(10));

        // Create node and edge
        let n1 = db
            .create_node(
                "Node",
                PropertyMapBuilder::new().insert("val", 1i64).build(),
            )
            .unwrap();
        let n2 = db
            .create_node(
                "Node",
                PropertyMapBuilder::new().insert("val", 2i64).build(),
            )
            .unwrap();
        let e1 = db
            .create_edge(n1, n2, "KNOWS", PropertyMapBuilder::new().build())
            .unwrap();

        sleep(Duration::from_millis(10));
        let t1 = aletheiadb::core::temporal::time::now(); // T1: After creation

        // Query at T0 (before creation) using batch API
        let node_results_t0 = db.get_nodes_at_time(&[n1, n2], t0, t0).unwrap();
        assert_eq!(node_results_t0.len(), 2);
        assert!(
            node_results_t0[0].1.is_none(),
            "Node 1 should not exist at T0"
        );
        assert!(
            node_results_t0[1].1.is_none(),
            "Node 2 should not exist at T0"
        );

        let edge_results_t0 = db.get_edges_at_time(&[e1], t0, t0).unwrap();
        assert_eq!(edge_results_t0.len(), 1);
        assert!(
            edge_results_t0[0].1.is_none(),
            "Edge 1 should not exist at T0"
        );

        // Query at T1 (after creation)
        let node_results_t1 = db.get_nodes_at_time(&[n1, n2], t1, t1).unwrap();
        assert_eq!(node_results_t1.len(), 2);
        assert!(node_results_t1[0].1.is_some(), "Node 1 should exist at T1");
        assert!(node_results_t1[1].1.is_some(), "Node 2 should exist at T1");

        let edge_results_t1 = db.get_edges_at_time(&[e1], t1, t1).unwrap();
        assert_eq!(edge_results_t1.len(), 1);
        assert!(edge_results_t1[0].1.is_some(), "Edge 1 should exist at T1");
    }
}
