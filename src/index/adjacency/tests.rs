// use super::*;
    use super::*;
    use crate::core::interning::GLOBAL_INTERNER;

    #[test]
    fn test_empty_index() {
        let index = AdjacencyIndex::new();
        assert_eq!(index.edge_count(), 0);
        assert_eq!(index.degree(NodeId::new(0).unwrap()), 0);
        assert_eq!(index.get_adjacency(NodeId::new(0).unwrap()).len(), 0);
    }

    #[test]
    fn test_build_simple_graph() {
        let knows = GLOBAL_INTERNER.intern("KNOWS").unwrap();

        let edges = vec![
            (
                NodeId::new(0).unwrap(),
                NodeId::new(1).unwrap(),
                EdgeId::new(0).unwrap(),
                knows,
            ),
            (
                NodeId::new(0).unwrap(),
                NodeId::new(2).unwrap(),
                EdgeId::new(1).unwrap(),
                knows,
            ),
            (
                NodeId::new(1).unwrap(),
                NodeId::new(2).unwrap(),
                EdgeId::new(2).unwrap(),
                knows,
            ),
        ];

        let index = AdjacencyIndex::build(edges);

        // Node 0 has 2 outgoing edges
        assert_eq!(index.degree(NodeId::new(0).unwrap()), 2);
        let adj0 = index.get_adjacency(NodeId::new(0).unwrap());
        assert_eq!(adj0.len(), 2);
        assert_eq!(adj0[0].target, NodeId::new(1).unwrap());
        assert_eq!(adj0[1].target, NodeId::new(2).unwrap());

        // Node 1 has 1 outgoing edge
        assert_eq!(index.degree(NodeId::new(1).unwrap()), 1);
        let adj1 = index.get_adjacency(NodeId::new(1).unwrap());
        assert_eq!(adj1.len(), 1);
        assert_eq!(adj1[0].target, NodeId::new(2).unwrap());

        // Node 2 has no outgoing edges
        assert_eq!(index.degree(NodeId::new(2).unwrap()), 0);

        // Total edges
        assert_eq!(index.edge_count(), 3);
    }

    #[test]
    fn test_multiple_edge_labels() {
        let knows = GLOBAL_INTERNER.intern("KNOWS").unwrap();
        let follows = GLOBAL_INTERNER.intern("FOLLOWS").unwrap();

        let edges = vec![
            (
                NodeId::new(0).unwrap(),
                NodeId::new(1).unwrap(),
                EdgeId::new(0).unwrap(),
                knows,
            ),
            (
                NodeId::new(0).unwrap(),
                NodeId::new(2).unwrap(),
                EdgeId::new(1).unwrap(),
                follows,
            ),
            (
                NodeId::new(0).unwrap(),
                NodeId::new(3).unwrap(),
                EdgeId::new(2).unwrap(),
                knows,
            ),
        ];

        let index = AdjacencyIndex::build(edges);

        // Get all edges from node 0
        assert_eq!(index.degree(NodeId::new(0).unwrap()), 3);

        // Get only KNOWS edges from node 0
        let knows_edges: Vec<_> = index
            .get_adjacency_with_label(NodeId::new(0).unwrap(), knows)
            .collect();
        assert_eq!(knows_edges.len(), 2);

        // Get only FOLLOWS edges from node 0
        let follows_edges: Vec<_> = index
            .get_adjacency_with_label(NodeId::new(0).unwrap(), follows)
            .collect();
        assert_eq!(follows_edges.len(), 1);
        assert_eq!(follows_edges[0].target, NodeId::new(2).unwrap());
    }

    #[test]
    fn test_node_without_edges() {
        let knows = GLOBAL_INTERNER.intern("KNOWS").unwrap();

        let edges = vec![(
            NodeId::new(0).unwrap(),
            NodeId::new(1).unwrap(),
            EdgeId::new(0).unwrap(),
            knows,
        )];

        let index = AdjacencyIndex::build(edges);

        // Node 5 doesn't exist
        assert_eq!(index.degree(NodeId::new(5).unwrap()), 0);
        assert!(!index.has_edges(NodeId::new(5).unwrap()));
        assert_eq!(index.get_adjacency(NodeId::new(5).unwrap()).len(), 0);
    }

    #[test]
    fn test_adjacency_entry() {
        let label = GLOBAL_INTERNER.intern("TEST").unwrap();
        let entry = AdjacencyEntry::new(NodeId::new(1).unwrap(), EdgeId::new(100).unwrap(), label);

        assert_eq!(entry.target, NodeId::new(1).unwrap());
        assert_eq!(entry.edge_id, EdgeId::new(100).unwrap());
        assert_eq!(entry.label, label);
    }

    #[test]
    fn test_sorted_adjacency() {
        // Edges deliberately out of order
        let knows = GLOBAL_INTERNER.intern("KNOWS").unwrap();
        let edges = vec![
            (
                NodeId::new(0).unwrap(),
                NodeId::new(3).unwrap(),
                EdgeId::new(2).unwrap(),
                knows,
            ),
            (
                NodeId::new(0).unwrap(),
                NodeId::new(1).unwrap(),
                EdgeId::new(0).unwrap(),
                knows,
            ),
            (
                NodeId::new(0).unwrap(),
                NodeId::new(2).unwrap(),
                EdgeId::new(1).unwrap(),
                knows,
            ),
        ];

        let index = AdjacencyIndex::build(edges);
        let adj = index.get_adjacency(NodeId::new(0).unwrap());

        // Should be sorted by target
        assert_eq!(adj[0].target, NodeId::new(1).unwrap());
        assert_eq!(adj[1].target, NodeId::new(2).unwrap());
        assert_eq!(adj[2].target, NodeId::new(3).unwrap());
    }

    #[test]
    fn test_sparse_node_ids() {
        // Simulate scenario after deletions: only nodes 10, 1000, and 1_000_000 exist
        // This tests that we handle sparse IDs efficiently
        let knows = GLOBAL_INTERNER.intern("KNOWS").unwrap();
        let edges = vec![
            (
                NodeId::new(10).unwrap(),
                NodeId::new(20).unwrap(),
                EdgeId::new(0).unwrap(),
                knows,
            ),
            (
                NodeId::new(1000).unwrap(),
                NodeId::new(2000).unwrap(),
                EdgeId::new(1).unwrap(),
                knows,
            ),
            (
                NodeId::new(1_000_000).unwrap(),
                NodeId::new(2_000_000).unwrap(),
                EdgeId::new(2).unwrap(),
                knows,
            ),
        ];

        let index = AdjacencyIndex::build(edges);

        // Verify correctness for sparse nodes
        assert_eq!(index.degree(NodeId::new(10).unwrap()), 1);
        assert_eq!(index.degree(NodeId::new(1000).unwrap()), 1);
        assert_eq!(index.degree(NodeId::new(1_000_000).unwrap()), 1);

        // Verify intermediate non-existent nodes return empty adjacency
        assert_eq!(index.degree(NodeId::new(0).unwrap()), 0);
        assert_eq!(index.degree(NodeId::new(100).unwrap()), 0);
        assert_eq!(index.degree(NodeId::new(50000).unwrap()), 0);

        // Verify adjacency list content
        let adj10 = index.get_adjacency(NodeId::new(10).unwrap());
        assert_eq!(adj10.len(), 1);
        assert_eq!(adj10[0].target, NodeId::new(20).unwrap());

        let adj1000 = index.get_adjacency(NodeId::new(1000).unwrap());
        assert_eq!(adj1000.len(), 1);
        assert_eq!(adj1000[0].target, NodeId::new(2000).unwrap());

        let adj1m = index.get_adjacency(NodeId::new(1_000_000).unwrap());
        assert_eq!(adj1m.len(), 1);
        assert_eq!(adj1m[0].target, NodeId::new(2_000_000).unwrap());

        // Total edges should still be 3
        assert_eq!(index.edge_count(), 3);
    }

    #[test]
    fn test_sparse_ids_memory_efficiency() {
        // Test that sparse IDs don't cause excessive memory allocation
        // With old implementation: offsets would be Vec with 1_000_001 elements
        // With new implementation: offsets should only have entries for actual nodes
        let knows = GLOBAL_INTERNER.intern("KNOWS").unwrap();
        let edges = vec![
            (
                NodeId::new(0).unwrap(),
                NodeId::new(1).unwrap(),
                EdgeId::new(0).unwrap(),
                knows,
            ),
            (
                NodeId::new(1_000_000).unwrap(),
                NodeId::new(1_000_001).unwrap(),
                EdgeId::new(1).unwrap(),
                knows,
            ),
        ];

        let index = AdjacencyIndex::build(edges);

        // After optimization, offsets should be proportional to number of nodes, not max_node_id
        // With 2 source nodes, we should have at most a few entries, not 1_000_001
        // Allow some overhead for implementation details
        assert!(
            index.offsets.len() < 100,
            "Offsets array should be compact for sparse IDs, got {} entries",
            index.offsets.len()
        );

        // Verify correctness
        assert_eq!(index.degree(NodeId::new(0).unwrap()), 1);
        assert_eq!(index.degree(NodeId::new(1_000_000).unwrap()), 1);
        assert_eq!(index.edge_count(), 2);
    }

    #[test]
    fn test_sparse_ids_with_multiple_edges_per_node() {
        // Test sparse IDs where some nodes have multiple outgoing edges
        let knows = GLOBAL_INTERNER.intern("KNOWS").unwrap();
        let follows = GLOBAL_INTERNER.intern("FOLLOWS").unwrap();
        let edges = vec![
            (
                NodeId::new(100).unwrap(),
                NodeId::new(101).unwrap(),
                EdgeId::new(0).unwrap(),
                knows,
            ),
            (
                NodeId::new(100).unwrap(),
                NodeId::new(102).unwrap(),
                EdgeId::new(1).unwrap(),
                follows,
            ),
            (
                NodeId::new(100).unwrap(),
                NodeId::new(103).unwrap(),
                EdgeId::new(2).unwrap(),
                knows,
            ),
            (
                NodeId::new(500_000).unwrap(),
                NodeId::new(500_001).unwrap(),
                EdgeId::new(3).unwrap(),
                knows,
            ),
            (
                NodeId::new(500_000).unwrap(),
                NodeId::new(500_002).unwrap(),
                EdgeId::new(4).unwrap(),
                follows,
            ),
        ];

        let index = AdjacencyIndex::build(edges);

        // Verify node 100 has 3 edges
        assert_eq!(index.degree(NodeId::new(100).unwrap()), 3);
        let adj100 = index.get_adjacency(NodeId::new(100).unwrap());
        assert_eq!(adj100.len(), 3);
        // Should be sorted by target
        assert_eq!(adj100[0].target, NodeId::new(101).unwrap());
        assert_eq!(adj100[1].target, NodeId::new(102).unwrap());
        assert_eq!(adj100[2].target, NodeId::new(103).unwrap());

        // Verify node 500_000 has 2 edges
        assert_eq!(index.degree(NodeId::new(500_000).unwrap()), 2);
        let adj500k = index.get_adjacency(NodeId::new(500_000).unwrap());
        assert_eq!(adj500k.len(), 2);
        assert_eq!(adj500k[0].target, NodeId::new(500_001).unwrap());
        assert_eq!(adj500k[1].target, NodeId::new(500_002).unwrap());

        // Verify intermediate nodes have no edges
        assert_eq!(index.degree(NodeId::new(200_000).unwrap()), 0);
        assert_eq!(index.degree(NodeId::new(300_000).unwrap()), 0);

        // Total edges
        assert_eq!(index.edge_count(), 5);
    }

    #[test]
    fn test_build_with_many_edges_preallocation() {
        // Test that building with many edges works correctly.
        // This test verifies the scenario mentioned in issue #193 where
        // pre-allocating the flat_edges Vec avoids ~14 reallocations for 10,000 edges.
        let knows = GLOBAL_INTERNER.intern("KNOWS").unwrap();

        // Create 10,000 edges across 1,000 nodes
        let edge_count = 10_000;
        let node_count = 1_000;

        let mut edges = Vec::with_capacity(edge_count);
        for i in 0..edge_count {
            let source = NodeId::new((i % node_count) as u64).unwrap();
            let target = NodeId::new(((i + 1) % node_count) as u64).unwrap();
            let edge_id = EdgeId::new(i as u64).unwrap();
            edges.push((source, target, edge_id, knows));
        }

        // Build the index (should pre-allocate to avoid reallocations)
        let index = AdjacencyIndex::build(edges);

        // Verify correctness
        assert_eq!(index.edge_count(), edge_count);

        // Verify that each node has the correct number of outgoing edges.
        // In this test setup, each node is a source for `edge_count / node_count` edges.
        let expected_degree = edge_count / node_count;
        for i in 0..node_count {
            let node = NodeId::new(i as u64).unwrap();
            let adj = index.get_adjacency(node);
            assert_eq!(
                adj.len(),
                expected_degree,
                "Node {} has an unexpected degree",
                i
            );
            // All adjacency entries should be valid
            for entry in adj {
                assert!(entry.edge_id.as_u64() < edge_count as u64);
                assert!(entry.target.as_u64() < node_count as u64);
            }
        }
    }

    #[test]
    fn test_max_node_id_from_target() {
        let knows = GLOBAL_INTERNER.intern("KNOWS").unwrap();
        let edges = vec![
            (
                NodeId::new(1).unwrap(),
                NodeId::new(1000).unwrap(),
                EdgeId::new(0).unwrap(),
                knows,
            ),
            (
                NodeId::new(2).unwrap(),
                NodeId::new(500).unwrap(),
                EdgeId::new(1).unwrap(),
                knows,
            ),
        ];
        let index = AdjacencyIndex::build(edges);
        assert_eq!(
            index.max_node_id(),
            1000,
            "max_node_id should consider target nodes"
        );
    }

    #[test]
    fn test_transmute_vec_correctness() {
        let original = vec![1u64, 2, 3];
        let ptr = original.as_ptr();
        let cap = original.capacity();

        // Use NodeId which is transparent wrapper around u64
        let transmuted: Vec<NodeId> = bytemuck::cast_vec(original);

        assert_eq!(transmuted.len(), 3);
        assert_eq!(transmuted.capacity(), cap);
        assert_eq!(transmuted[0], NodeId::new(1).unwrap());
        assert_eq!(transmuted[1], NodeId::new(2).unwrap());
        assert_eq!(transmuted[2], NodeId::new(3).unwrap());

        // Verify no copy happened (best effort check, pointers should match)
        assert_eq!(transmuted.as_ptr() as *const u64, ptr);
    }

    #[test]
    fn test_import_csr_integration() {
        // This test ensures import_csr works in the standard test module scope
        let node_ids = vec![1, 2];
        let offsets = vec![0, 1, 2];
        let edge_ids = vec![10, 20];
        let mut edges_map =
            std::collections::HashMap::with_hasher(std::hash::BuildHasherDefault::<
                crate::core::hasher::IdentityHasher,
            >::default());

        let label = crate::core::interning::GLOBAL_INTERNER
            .intern("TEST")
            .unwrap();
        edges_map.insert(EdgeId::new(10).unwrap(), (NodeId::new(2).unwrap(), label));
        edges_map.insert(EdgeId::new(20).unwrap(), (NodeId::new(1).unwrap(), label));

        let index = AdjacencyIndex::import_csr(node_ids, offsets, edge_ids, &edges_map);
        assert_eq!(index.node_count(), 2);
        assert_eq!(index.edge_count(), 2);
    }
