// use super::*;
    use super::*;
    use crate::core::TIMESTAMP_MAX;
    use crate::core::temporal::time;

    fn ts(t: i64) -> Timestamp {
        time::from_secs(t)
    }

    #[test]
    fn test_entry_is_valid_at() {
        let t0 = ts(10);
        let t1 = ts(20);
        let t2 = ts(30);

        let entry = TemporalAdjacencyEntry {
            edge_id: EdgeId::new(1).unwrap(),
            neighbor: NodeId::new(100).unwrap(),
            label: InternedString::from_raw(1),
            valid_from: t0,
            valid_to: t2,
            tx_from: t0,
            tx_to: TIMESTAMP_MAX,
        };

        // Valid at t1 (between t0 and t2)
        assert!(entry.is_valid_at(t1, t1));

        // Not valid at or after t2
        assert!(!entry.is_valid_at(t2, t1));
    }

    #[test]
    fn test_insert_and_retrieve_outgoing() {
        let index = TemporalAdjacencyIndex::new(TemporalAdjacencyConfig::default());
        let source = NodeId::new(1).unwrap();
        let target = NodeId::new(2).unwrap();
        let edge = EdgeId::new(100).unwrap();
        let label = InternedString::from_raw(1);

        index
            .insert_edge(
                edge,
                source,
                target,
                label,
                ts(10),
                ts(20), // valid range [10, 20)
                ts(5),
                TIMESTAMP_MAX, // tx range [5, MAX)
            )
            .unwrap();

        // Valid query
        let result = index.get_outgoing_at_time(source, ts(15), ts(10));
        assert_eq!(result.len(), 1);
        assert_eq!(result[0], edge);

        // Invalid query (time before valid_from)
        let result = index.get_outgoing_at_time(source, ts(5), ts(10));
        assert!(result.is_empty());

        // Invalid query (time after valid_to)
        let result = index.get_outgoing_at_time(source, ts(25), ts(10));
        assert!(result.is_empty());
    }

    #[test]
    fn test_insert_and_retrieve_incoming() {
        let index = TemporalAdjacencyIndex::new(TemporalAdjacencyConfig::default());
        let source = NodeId::new(1).unwrap();
        let target = NodeId::new(2).unwrap();
        let edge = EdgeId::new(100).unwrap();
        let label = InternedString::from_raw(1);

        index
            .insert_edge(
                edge,
                source,
                target,
                label,
                ts(10),
                ts(20),
                ts(5),
                TIMESTAMP_MAX,
            )
            .unwrap();

        // Incoming to target
        let result = index.get_incoming_at_time(target, ts(15), ts(10));
        assert_eq!(result.len(), 1);
        assert_eq!(result[0], edge);

        // Incoming to source (should be empty)
        let result = index.get_incoming_at_time(source, ts(15), ts(10));
        assert!(result.is_empty());
    }

    #[test]
    fn test_temporal_validity() {
        let index = TemporalAdjacencyIndex::new(TemporalAdjacencyConfig::default());
        let source = NodeId::new(1).unwrap();
        let target = NodeId::new(2).unwrap();
        let edge = EdgeId::new(100).unwrap();
        let label = InternedString::from_raw(1);

        index
            .insert_edge(
                edge,
                source,
                target,
                label,
                ts(100),
                ts(200),
                ts(0),
                TIMESTAMP_MAX,
            )
            .unwrap();

        // Valid boundary (inclusive start)
        assert_eq!(index.get_outgoing_at_time(source, ts(100), ts(10)).len(), 1);

        // Invalid boundary (exclusive end)
        assert_eq!(index.get_outgoing_at_time(source, ts(200), ts(10)).len(), 0);

        // Just before end
        assert_eq!(index.get_outgoing_at_time(source, ts(199), ts(10)).len(), 1);
    }

    #[test]
    fn test_transaction_visibility() {
        let index = TemporalAdjacencyIndex::new(TemporalAdjacencyConfig::default());
        let source = NodeId::new(1).unwrap();
        let target = NodeId::new(2).unwrap();
        let edge = EdgeId::new(100).unwrap();
        let label = InternedString::from_raw(1);

        index
            .insert_edge(
                edge,
                source,
                target,
                label,
                ts(10),
                ts(20),
                ts(100),
                ts(200), // Recorded between 100 and 200
            )
            .unwrap();

        // Query at valid time 15, tx time 150 (visible)
        assert_eq!(index.get_outgoing_at_time(source, ts(15), ts(150)).len(), 1);

        // Query at valid time 15, tx time 50 (before recorded)
        assert_eq!(index.get_outgoing_at_time(source, ts(15), ts(50)).len(), 0);

        // Query at valid time 15, tx time 250 (after superseded/deleted)
        assert_eq!(index.get_outgoing_at_time(source, ts(15), ts(250)).len(), 0);
    }

    #[test]
    fn test_edge_updates() {
        let index = TemporalAdjacencyIndex::new(TemporalAdjacencyConfig::default());
        let source = NodeId::new(1).unwrap();
        let target = NodeId::new(2).unwrap();
        let edge = EdgeId::new(100).unwrap();
        let label = InternedString::from_raw(1);

        // Insert open-ended edge
        index
            .insert_edge(
                edge,
                source,
                target,
                label,
                ts(10),
                ts(100), // Initially valid until 100
                ts(0),
                TIMESTAMP_MAX,
            )
            .unwrap();

        assert_eq!(index.get_outgoing_at_time(source, ts(50), ts(10)).len(), 1);

        // Close valid time at 40
        index.close_edge_valid_time(edge, source, target, ts(40));

        // Should be valid at 30
        assert_eq!(index.get_outgoing_at_time(source, ts(30), ts(10)).len(), 1);

        // Should NOT be valid at 50 anymore
        assert_eq!(index.get_outgoing_at_time(source, ts(50), ts(10)).len(), 0);
    }

    #[test]
    fn test_capacity_limit() {
        let config = TemporalAdjacencyConfig {
            max_entries_per_node: 2,
        };
        let index = TemporalAdjacencyIndex::new(config);
        let source = NodeId::new(1).unwrap();
        let target = NodeId::new(2).unwrap();
        let label = InternedString::from_raw(1);

        // Insert 2 edges (limit reached)
        index
            .insert_edge(
                EdgeId::new(1).unwrap(),
                source,
                target,
                label,
                ts(10),
                ts(20),
                ts(0),
                TIMESTAMP_MAX,
            )
            .unwrap();
        index
            .insert_edge(
                EdgeId::new(2).unwrap(),
                source,
                target,
                label,
                ts(20),
                ts(30),
                ts(0),
                TIMESTAMP_MAX,
            )
            .unwrap();

        // Insert 3rd edge (should fail)
        let result = index.insert_edge(
            EdgeId::new(3).unwrap(),
            source,
            target,
            label,
            ts(30),
            ts(40),
            ts(0),
            TIMESTAMP_MAX,
        );
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            StorageError::CapacityExceeded { .. }
        ));
    }

    #[test]
    fn test_self_loop() {
        let index = TemporalAdjacencyIndex::new(TemporalAdjacencyConfig::default());
        let node = NodeId::new(1).unwrap();
        let edge = EdgeId::new(100).unwrap();
        let label = InternedString::from_raw(1);

        // Insert self-loop
        index
            .insert_edge(
                edge,
                node,
                node,
                label,
                ts(10),
                ts(20),
                ts(0),
                TIMESTAMP_MAX,
            )
            .unwrap();

        // Should be outgoing from node
        let out_res = index.get_outgoing_at_time(node, ts(15), ts(10));
        assert_eq!(out_res.len(), 1);
        assert_eq!(out_res[0], edge);

        // Should be incoming to node
        let in_res = index.get_incoming_at_time(node, ts(15), ts(10));
        assert_eq!(in_res.len(), 1);
        assert_eq!(in_res[0], edge);
    }

    #[test]
    fn test_multiple_versions_deduplication() {
        let index = TemporalAdjacencyIndex::new(TemporalAdjacencyConfig::default());
        let source = NodeId::new(1).unwrap();
        let target = NodeId::new(2).unwrap();
        let edge = EdgeId::new(100).unwrap(); // Same edge ID
        let label = InternedString::from_raw(1);

        // Version 1: [10, 20)
        index
            .insert_edge(
                edge,
                source,
                target,
                label,
                ts(10),
                ts(20),
                ts(0),
                TIMESTAMP_MAX,
            )
            .unwrap();

        // Version 2: [30, 40)
        index
            .insert_edge(
                edge,
                source,
                target,
                label,
                ts(30),
                ts(40),
                ts(0),
                TIMESTAMP_MAX,
            )
            .unwrap();

        // Query at 15 -> find V1
        assert_eq!(index.get_outgoing_at_time(source, ts(15), ts(10)).len(), 1);

        // Query at 35 -> find V2 (same edge ID)
        assert_eq!(index.get_outgoing_at_time(source, ts(35), ts(10)).len(), 1);

        // Query at 25 -> no version valid
        assert_eq!(index.get_outgoing_at_time(source, ts(25), ts(10)).len(), 0);
    }

    #[test]
    fn test_lock_ordering_deadlock_prevention() {
        // This test ensures that the insertion logic handles node ID ordering correctly.
        // While we can't easily detect deadlocks in a unit test, we can verify that insertions work
        // regardless of node ID order (source < target vs source > target).

        let index = TemporalAdjacencyIndex::new(TemporalAdjacencyConfig::default());
        let small_id = NodeId::new(10).unwrap();
        let large_id = NodeId::new(20).unwrap();
        let label = InternedString::from_raw(1);

        // Case 1: source < target
        index
            .insert_edge(
                EdgeId::new(1).unwrap(),
                small_id,
                large_id,
                label,
                ts(10),
                ts(20),
                ts(0),
                TIMESTAMP_MAX,
            )
            .unwrap();

        // Case 2: source > target
        index
            .insert_edge(
                EdgeId::new(2).unwrap(),
                large_id,
                small_id,
                label,
                ts(10),
                ts(20),
                ts(0),
                TIMESTAMP_MAX,
            )
            .unwrap();

        // Verify both were inserted
        assert_eq!(
            index.get_outgoing_at_time(small_id, ts(15), ts(10)).len(),
            1
        );
        assert_eq!(
            index.get_outgoing_at_time(large_id, ts(15), ts(10)).len(),
            1
        );
    }

    #[test]
    fn test_get_outgoing_with_label() {
        let index = TemporalAdjacencyIndex::new(TemporalAdjacencyConfig::default());
        let source = NodeId::new(1).unwrap();
        let target = NodeId::new(2).unwrap();
        let label1 = InternedString::from_raw(1);
        let label2 = InternedString::from_raw(2);

        index
            .insert_edge(
                EdgeId::new(1).unwrap(),
                source,
                target,
                label1,
                ts(10),
                ts(20),
                ts(0),
                TIMESTAMP_MAX,
            )
            .unwrap();
        index
            .insert_edge(
                EdgeId::new(2).unwrap(),
                source,
                target,
                label2,
                ts(10),
                ts(20),
                ts(0),
                TIMESTAMP_MAX,
            )
            .unwrap();

        let res1 = index.get_outgoing_with_label_at_time(source, label1, ts(15), ts(10));
        assert_eq!(res1.len(), 1);
        assert_eq!(res1[0], EdgeId::new(1).unwrap());

        let res2 = index.get_outgoing_with_label_at_time(source, label2, ts(15), ts(10));
        assert_eq!(res2.len(), 1);
        assert_eq!(res2[0], EdgeId::new(2).unwrap());
    }
