//! Tests verifying usearch native delete functionality.

use gallifreydb::core::id::NodeId;
use gallifreydb::index::vector::{DistanceMetric, HnswIndexBuilder, VectorIndex};

/// Test that native deletes truly remove vectors from the index.
#[test]
fn test_native_delete_removes_from_index() {
    let index = HnswIndexBuilder::new(4, DistanceMetric::Cosine)
        .build()
        .unwrap();

    let node1 = NodeId::new(1).unwrap();
    let node2 = NodeId::new(2).unwrap();
    let node3 = NodeId::new(3).unwrap();

    // Add three vectors
    index.add(node1, &[1.0, 0.0, 0.0, 0.0]).unwrap();
    index.add(node2, &[0.0, 1.0, 0.0, 0.0]).unwrap();
    index.add(node3, &[0.0, 0.0, 1.0, 0.0]).unwrap();

    assert_eq!(index.len(), 3);

    // Delete node2
    index.remove(node2).unwrap();

    // Verify length decreased
    assert_eq!(index.len(), 2);

    // Search for vectors similar to node2's original position
    let results = index.search(&[0.0, 1.0, 0.0, 0.0], 10).unwrap();

    // node2 should NOT appear in results (native delete, not soft delete)
    for (id, _) in &results {
        assert_ne!(
            *id, node2,
            "Deleted node should not appear in search results"
        );
    }
}

/// Test that re-adding a deleted node works correctly.
#[test]
fn test_readd_after_delete() {
    let index = HnswIndexBuilder::new(4, DistanceMetric::Cosine)
        .build()
        .unwrap();

    let node1 = NodeId::new(1).unwrap();

    // Add, delete, re-add with different vector
    index.add(node1, &[1.0, 0.0, 0.0, 0.0]).unwrap();
    index.remove(node1).unwrap();
    index.add(node1, &[0.0, 0.0, 0.0, 1.0]).unwrap();

    assert_eq!(index.len(), 1);

    // Search should find the new vector
    let results = index.search(&[0.0, 0.0, 0.0, 1.0], 1).unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].0, node1);
    assert!(results[0].1 > 0.99); // Should be very similar
}

/// Test batch delete.
#[test]
fn test_batch_delete() {
    let index = HnswIndexBuilder::new(4, DistanceMetric::Cosine)
        .build()
        .unwrap();

    // Add 100 nodes
    for i in 1..=100 {
        let node = NodeId::new(i).unwrap();
        index.add(node, &[i as f32, 0.0, 0.0, 0.0]).unwrap();
    }
    assert_eq!(index.len(), 100);

    // Delete nodes 1-50
    let to_delete: Vec<NodeId> = (1..=50).map(|i| NodeId::new(i).unwrap()).collect();
    index.remove_batch(&to_delete).unwrap();

    // Verify length
    assert_eq!(index.len(), 50);

    // Verify deleted nodes don't appear in search
    let results = index.search(&[25.0, 0.0, 0.0, 0.0], 100).unwrap();
    for (id, _) in &results {
        assert!(
            id.as_u64() > 50,
            "Deleted node {} found in results",
            id.as_u64()
        );
    }
}
