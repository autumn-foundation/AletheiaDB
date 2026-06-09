#[allow(unused_imports)]
use super::*;
use crate::core::id::VersionId;
use crate::core::interning::GLOBAL_INTERNER;
use crate::core::property::PropertyMapBuilder;

pub(super) fn create_test_node(id: u64, label: &str) -> Node {
    Node::new(
        NodeId::new(id).unwrap(),
        GLOBAL_INTERNER.intern(label).unwrap(),
        PropertyMapBuilder::new().build(),
        VersionId::new(1).unwrap(),
    )
}

pub(super) fn create_test_edge(id: u64, source: u64, target: u64, label: &str) -> Edge {
    Edge::new(
        EdgeId::new(id).unwrap(),
        GLOBAL_INTERNER.intern(label).unwrap(),
        NodeId::new(source).unwrap(),
        NodeId::new(target).unwrap(),
        PropertyMapBuilder::new().build(),
        VersionId::new(1).unwrap(),
    )
}

#[test]
fn test_node_operations() {
    let indexes = CurrentIndexes::new();

    // Initially empty
    assert_eq!(indexes.node_count(), 0);
    assert!(!indexes.contains_node(NodeId::new(1).unwrap()));

    // Insert node
    let node = create_test_node(1, "Person");
    indexes.insert_node(node.clone());

    assert_eq!(indexes.node_count(), 1);
    assert!(indexes.contains_node(NodeId::new(1).unwrap()));

    // Get node
    let retrieved = indexes.get_node(NodeId::new(1).unwrap()).unwrap();
    assert_eq!(retrieved.id, node.id);
    assert_eq!(retrieved.label, node.label);

    // Remove node
    let removed = indexes.remove_node(NodeId::new(1).unwrap()).unwrap();
    assert_eq!(removed.id, node.id);
    assert_eq!(indexes.node_count(), 0);
}

#[test]
fn test_edge_operations() {
    let indexes = CurrentIndexes::new();

    // Insert edge
    let edge = create_test_edge(1, 0, 1, "KNOWS");
    indexes.insert_edge(edge.clone());

    assert_eq!(indexes.edge_count(), 1);
    assert!(indexes.contains_edge(EdgeId::new(1).unwrap()));

    // Get edge
    let retrieved = indexes.get_edge(EdgeId::new(1).unwrap()).unwrap();
    assert_eq!(retrieved.id, edge.id);
    assert_eq!(retrieved.source, edge.source);
    assert_eq!(retrieved.target, edge.target);

    // Remove edge
    let removed = indexes.remove_edge(EdgeId::new(1).unwrap()).unwrap();
    assert_eq!(removed.id, edge.id);
    assert_eq!(indexes.edge_count(), 0);
}

#[test]
fn test_adjacency_rebuild() {
    let indexes = CurrentIndexes::new();

    // Add nodes
    indexes.insert_node(create_test_node(0, "Person"));
    indexes.insert_node(create_test_node(1, "Person"));
    indexes.insert_node(create_test_node(2, "Person"));

    // Add edges
    indexes.insert_edge(create_test_edge(0, 0, 1, "KNOWS"));
    indexes.insert_edge(create_test_edge(1, 0, 2, "KNOWS"));
    indexes.insert_edge(create_test_edge(2, 1, 2, "KNOWS"));

    // Rebuild adjacency indexes
    indexes.compact_adjacency();

    // Test outgoing edges
    assert_eq!(indexes.out_degree(NodeId::new(0).unwrap()), 2);
    assert_eq!(indexes.out_degree(NodeId::new(1).unwrap()), 1);
    assert_eq!(indexes.out_degree(NodeId::new(2).unwrap()), 0);

    let outgoing = indexes.get_outgoing(NodeId::new(0).unwrap());
    assert_eq!(outgoing.len(), 2);

    // Test incoming edges
    assert_eq!(indexes.in_degree(NodeId::new(0).unwrap()), 0);
    assert_eq!(indexes.in_degree(NodeId::new(1).unwrap()), 1);
    assert_eq!(indexes.in_degree(NodeId::new(2).unwrap()), 2);
}

#[test]
fn test_labeled_traversal() {
    let indexes = CurrentIndexes::new();

    let knows = GLOBAL_INTERNER.intern("KNOWS").unwrap();
    let follows = GLOBAL_INTERNER.intern("FOLLOWS").unwrap();

    // Add edges with different labels
    indexes.insert_edge(create_test_edge(0, 0, 1, "KNOWS"));
    indexes.insert_edge(create_test_edge(1, 0, 2, "FOLLOWS"));
    indexes.insert_edge(create_test_edge(2, 0, 3, "KNOWS"));

    indexes.compact_adjacency();

    // Get only KNOWS edges
    let knows_edges = indexes.get_outgoing_with_label(NodeId::new(0).unwrap(), knows);
    assert_eq!(knows_edges.len(), 2);

    // Get only FOLLOWS edges
    let follows_edges = indexes.get_outgoing_with_label(NodeId::new(0).unwrap(), follows);
    assert_eq!(follows_edges.len(), 1);
}

#[test]
fn test_iteration() {
    let indexes = CurrentIndexes::new();

    // Add some nodes and edges
    indexes.insert_node(create_test_node(0, "Person"));
    indexes.insert_node(create_test_node(1, "Person"));
    indexes.insert_edge(create_test_edge(0, 0, 1, "KNOWS"));

    // Test iteration
    let nodes: Vec<_> = indexes.iter_nodes().collect();
    assert_eq!(nodes.len(), 2);

    let edges: Vec<_> = indexes.iter_edges().collect();
    assert_eq!(edges.len(), 1);
}

#[test]
fn test_rebuild_idempotent() {
    let indexes = CurrentIndexes::new();

    // Add edges
    indexes.insert_edge(create_test_edge(0, 0, 1, "KNOWS"));
    indexes.insert_edge(create_test_edge(1, 0, 2, "KNOWS"));

    // Rebuild once
    indexes.compact_adjacency();
    let first_out = indexes.get_outgoing(NodeId::new(0).unwrap());
    let first_in = indexes.get_incoming(NodeId::new(1).unwrap());

    // Rebuild again
    indexes.compact_adjacency();
    let second_out = indexes.get_outgoing(NodeId::new(0).unwrap());
    let second_in = indexes.get_incoming(NodeId::new(1).unwrap());

    // Results should be identical
    assert_eq!(first_out.len(), second_out.len());
    assert_eq!(first_in.len(), second_in.len());
    assert_eq!(first_out, second_out);
    assert_eq!(first_in, second_in);
}

#[test]
fn test_lazy_rebuild_on_access() {
    let indexes = CurrentIndexes::new();

    // Add edges WITHOUT calling rebuild_adjacency()
    indexes.insert_edge(create_test_edge(0, 0, 1, "KNOWS"));
    indexes.insert_edge(create_test_edge(1, 0, 2, "KNOWS"));
    indexes.insert_edge(create_test_edge(2, 1, 2, "KNOWS"));

    // Adjacency is immediately visible via incremental index
    let outgoing = indexes.get_outgoing(NodeId::new(0).unwrap());
    assert_eq!(
        outgoing.len(),
        2,
        "Lazy rebuild should make edges accessible"
    );

    // Verify all adjacency data is correct
    assert_eq!(indexes.out_degree(NodeId::new(0).unwrap()), 2);
    assert_eq!(indexes.out_degree(NodeId::new(1).unwrap()), 1);
    assert_eq!(indexes.in_degree(NodeId::new(2).unwrap()), 2);
}

#[test]
fn test_lazy_rebuild_after_delete() {
    let indexes = CurrentIndexes::new();

    // Add edges and access to trigger initial rebuild
    indexes.insert_edge(create_test_edge(0, 0, 1, "KNOWS"));
    indexes.insert_edge(create_test_edge(1, 1, 2, "KNOWS"));
    let _ = indexes.get_outgoing(NodeId::new(0).unwrap());

    // Remove edge WITHOUT calling rebuild_adjacency()
    indexes.remove_edge(EdgeId::new(1).unwrap());

    // Adjacency should be rebuilt lazily on next access
    assert_eq!(indexes.out_degree(NodeId::new(1).unwrap()), 0);
    assert_eq!(indexes.in_degree(NodeId::new(2).unwrap()), 0);
}

#[test]
fn test_no_unnecessary_rebuilds() {
    let indexes = CurrentIndexes::new();

    // Add edges and trigger rebuild
    indexes.insert_edge(create_test_edge(0, 0, 1, "KNOWS"));
    let _ = indexes.get_outgoing(NodeId::new(0).unwrap());

    // Multiple accesses should not trigger additional rebuilds
    // (We can't directly observe this, but it's important for performance)
    for _ in 0..10 {
        let outgoing = indexes.get_outgoing(NodeId::new(0).unwrap());
        assert_eq!(outgoing.len(), 1);
    }

    // After accessing, if no modifications, adjacency should stay current
    assert_eq!(indexes.in_degree(NodeId::new(1).unwrap()), 1);
}

#[test]
fn test_lazy_rebuild_with_labeled_traversal() {
    let indexes = CurrentIndexes::new();

    let knows = GLOBAL_INTERNER.intern("KNOWS").unwrap();
    let follows = GLOBAL_INTERNER.intern("FOLLOWS").unwrap();

    // Add edges with different labels WITHOUT explicit rebuild
    indexes.insert_edge(create_test_edge(0, 0, 1, "KNOWS"));
    indexes.insert_edge(create_test_edge(1, 0, 2, "FOLLOWS"));
    indexes.insert_edge(create_test_edge(2, 0, 3, "KNOWS"));

    // Lazy rebuild should happen on labeled access
    let knows_edges = indexes.get_outgoing_with_label(NodeId::new(0).unwrap(), knows);
    assert_eq!(knows_edges.len(), 2);

    let follows_edges = indexes.get_outgoing_with_label(NodeId::new(0).unwrap(), follows);
    assert_eq!(follows_edges.len(), 1);
}

/// Test that AdjacencyGuard works correctly and derefs to slice.
#[test]
fn test_adjacency_guard_deref() {
    let indexes = CurrentIndexes::new();

    // Add edges
    indexes.insert_edge(create_test_edge(0, 0, 1, "KNOWS"));
    indexes.insert_edge(create_test_edge(1, 0, 2, "KNOWS"));
    indexes.insert_edge(create_test_edge(2, 1, 2, "KNOWS"));
    indexes.compact_adjacency();

    // Get guard
    let guard = indexes.get_outgoing(NodeId::new(0).unwrap());

    // Should deref to slice
    assert_eq!(guard.len(), 2);
    assert_eq!(guard[0].target, NodeId::new(1).unwrap());
    assert_eq!(guard[1].target, NodeId::new(2).unwrap());

    // Should work with slice methods
    let targets: Vec<_> = guard.iter().map(|e| e.target).collect();
    assert_eq!(targets.len(), 2);
}

/// Test that AdjacencyGuard can be used in iterators.
#[test]
fn test_adjacency_guard_iteration() {
    let indexes = CurrentIndexes::new();

    // Add edges
    for i in 0..10 {
        indexes.insert_edge(create_test_edge(i, 0, i + 1, "LINK"));
    }
    indexes.compact_adjacency();

    // Get guard and iterate
    let guard = indexes.get_outgoing(NodeId::new(0).unwrap());
    let mut count = 0;
    for entry in guard.iter() {
        assert_eq!(entry.target.as_u64(), count + 1);
        count += 1;
    }
    assert_eq!(count, 10);
}

/// Test that AdjacencyGuard works with empty adjacency lists.
#[test]
fn test_adjacency_guard_empty() {
    let indexes = CurrentIndexes::new();
    indexes.compact_adjacency();

    // Get guard for node with no edges
    let guard = indexes.get_outgoing(NodeId::new(0).unwrap());
    assert_eq!(guard.len(), 0);
    assert!(guard.is_empty());
}

/// Test that AdjacencyGuard can be cloned (by cloning Arc).
#[test]
fn test_adjacency_guard_usage_patterns() {
    let indexes = CurrentIndexes::new();

    indexes.insert_edge(create_test_edge(0, 0, 1, "KNOWS"));
    indexes.insert_edge(create_test_edge(1, 0, 2, "KNOWS"));
    indexes.compact_adjacency();

    // Get guard
    let guard = indexes.get_outgoing(NodeId::new(0).unwrap());

    // Can use with functional operations
    let edge_ids: Vec<_> = guard.iter().map(|e| e.edge_id).collect();
    assert_eq!(edge_ids.len(), 2);

    // Can use with for loops
    for (i, entry) in guard.iter().enumerate() {
        assert_eq!(entry.edge_id, EdgeId::new(i as u64).unwrap());
    }

    // Can get length
    assert_eq!(guard.len(), 2);

    // Can index
    assert_eq!(guard[0].edge_id, EdgeId::new(0).unwrap());
    assert_eq!(guard[1].edge_id, EdgeId::new(1).unwrap());
}

/// Test that incoming guard works the same way.
#[test]
fn test_incoming_guard() {
    let indexes = CurrentIndexes::new();

    indexes.insert_edge(create_test_edge(0, 0, 2, "KNOWS"));
    indexes.insert_edge(create_test_edge(1, 1, 2, "KNOWS"));
    indexes.compact_adjacency();

    // Get incoming guard for node 2
    let guard = indexes.get_incoming(NodeId::new(2).unwrap());
    assert_eq!(guard.len(), 2);

    // AdjacencyIndex guarantees sorted order by target node
    let sources: Vec<_> = guard.iter().map(|e| e.target).collect();
    assert_eq!(
        sources,
        vec![NodeId::new(0).unwrap(), NodeId::new(1).unwrap()]
    );
}

/// Test AdjacencyGuard Debug implementation for coverage.
#[test]
fn test_adjacency_guard_debug() {
    let indexes = CurrentIndexes::new();

    indexes.insert_edge(create_test_edge(0, 5, 10, "KNOWS"));
    indexes.compact_adjacency();

    // Get guard and format with Debug
    let guard = indexes.get_outgoing(NodeId::new(5).unwrap());
    let debug_str = format!("{:?}", guard);

    // Should contain node ID and show it's an AdjacencyGuard
    assert!(debug_str.contains("AdjacencyGuard"));
    assert!(debug_str.contains("node"));
    assert!(debug_str.contains("entry_count"));
}

/// Test AdjacencyGuard with empty list for Debug coverage.
#[test]
fn test_adjacency_guard_debug_empty() {
    let indexes = CurrentIndexes::new();
    indexes.compact_adjacency();

    // Get guard for non-existent node (empty adjacency list)
    let guard = indexes.get_outgoing(NodeId::new(99).unwrap());
    let debug_str = format!("{:?}", guard);

    // Should format successfully even with empty entries
    assert!(debug_str.contains("AdjacencyGuard"));
    assert!(debug_str.contains("entry_count"));
}

/// Test rebuild_adjacency correctly handles many edges.
///
/// This test exercises the pre-allocated vector optimization in
/// rebuild_adjacency_internal() by creating a graph with many edges
/// and verifying all adjacencies are correctly computed.
#[test]
fn test_rebuild_adjacency_many_edges() {
    let indexes = CurrentIndexes::new();

    // Create a star graph: node 0 connects to nodes 1..=500
    // and nodes 501..=1000 connect to node 0
    // This creates 1000 total edges
    const NUM_EDGES: u64 = 1000;
    const HALF_EDGES: u64 = NUM_EDGES / 2;

    // Add outgoing edges from node 0
    for i in 0..HALF_EDGES {
        indexes.insert_edge(create_test_edge(i, 0, i + 1, "OUTGOING"));
    }

    // Add incoming edges to node 0
    for i in HALF_EDGES..NUM_EDGES {
        indexes.insert_edge(create_test_edge(i, i + 1, 0, "INCOMING"));
    }

    // Rebuild adjacency (this exercises the pre-allocation optimization)
    indexes.compact_adjacency();

    // Verify edge count
    assert_eq!(indexes.edge_count(), NUM_EDGES as usize);

    // Verify outgoing from node 0
    assert_eq!(
        indexes.out_degree(NodeId::new(0).unwrap()),
        HALF_EDGES as usize
    );

    // Verify incoming to node 0
    assert_eq!(
        indexes.in_degree(NodeId::new(0).unwrap()),
        HALF_EDGES as usize
    );

    // Verify all outgoing edges are accessible
    let outgoing = indexes.get_outgoing(NodeId::new(0).unwrap());
    assert_eq!(outgoing.len(), HALF_EDGES as usize);

    // Verify all targets are in expected range (1..=500)
    for entry in outgoing.iter() {
        let target_id = entry.target.as_u64();
        assert!(
            (1..=HALF_EDGES).contains(&target_id),
            "Unexpected outgoing target: {}",
            target_id
        );
    }

    // Verify all incoming edges are accessible
    let incoming = indexes.get_incoming(NodeId::new(0).unwrap());
    assert_eq!(incoming.len(), HALF_EDGES as usize);

    // Verify all sources are in expected range (501..=1000)
    for entry in incoming.iter() {
        let source_id = entry.target.as_u64(); // In incoming, target is the source node
        assert!(
            (HALF_EDGES + 1..=NUM_EDGES).contains(&source_id),
            "Unexpected incoming source: {}",
            source_id
        );
    }
}
