use super::*;
use crate::core::property::PropertyMapBuilder;

#[test]
fn test_create_node() {
    let storage = CurrentStorage::new();

    let props = PropertyMapBuilder::new()
        .insert("name", "Alice")
        .insert("age", 30i64)
        .build();

    let node_id = storage.create_node("Person", props).unwrap();

    assert_eq!(storage.node_count(), 1);

    let node = storage.get_node(node_id).unwrap();
    assert_eq!(node.id, node_id);
    assert_eq!(
        node.get_property("name").and_then(|v| v.as_str()),
        Some("Alice")
    );
}

#[test]
fn test_create_edge() {
    let storage = CurrentStorage::new();

    let alice = storage
        .create_node("Person", PropertyMapBuilder::new().build())
        .unwrap();
    let bob = storage
        .create_node("Person", PropertyMapBuilder::new().build())
        .unwrap();

    let edge_id = storage
        .create_edge(
            alice,
            bob,
            "KNOWS",
            PropertyMapBuilder::new().insert("since", 2020i64).build(),
        )
        .unwrap();

    assert_eq!(storage.edge_count(), 1);

    let edge = storage.get_edge(edge_id).unwrap();
    assert_eq!(edge.source, alice);
    assert_eq!(edge.target, bob);
    assert_eq!(
        edge.get_property("since").and_then(|v| v.as_int()),
        Some(2020)
    );
}

#[test]
fn test_create_edge_invalid_nodes() {
    let storage = CurrentStorage::new();

    let result = storage.create_edge(
        NodeId::new(999).unwrap(),
        NodeId::new(1000).unwrap(),
        "KNOWS",
        PropertyMapBuilder::new().build(),
    );

    assert!(result.is_err());
}

#[test]
fn test_graph_traversal() {
    let storage = CurrentStorage::new();

    // Create nodes
    let n0 = storage
        .create_node("Person", PropertyMapBuilder::new().build())
        .unwrap();
    let n1 = storage
        .create_node("Person", PropertyMapBuilder::new().build())
        .unwrap();
    let n2 = storage
        .create_node("Person", PropertyMapBuilder::new().build())
        .unwrap();

    // Create edges
    storage
        .create_edge(n0, n1, "KNOWS", PropertyMapBuilder::new().build())
        .unwrap();
    storage
        .create_edge(n0, n2, "KNOWS", PropertyMapBuilder::new().build())
        .unwrap();
    storage
        .create_edge(n1, n2, "FOLLOWS", PropertyMapBuilder::new().build())
        .unwrap();

    // Test outgoing edges
    let outgoing = storage.get_outgoing_edges(n0);
    assert_eq!(outgoing.len(), 2);

    // Test incoming edges
    let incoming = storage.get_incoming_edges(n2);
    assert_eq!(incoming.len(), 2);

    // Test degree
    assert_eq!(storage.out_degree(n0), 2);
    assert_eq!(storage.in_degree(n2), 2);
}

#[test]
fn test_labeled_edges() {
    let storage = CurrentStorage::new();

    let n0 = storage
        .create_node("Person", PropertyMapBuilder::new().build())
        .unwrap();
    let n1 = storage
        .create_node("Person", PropertyMapBuilder::new().build())
        .unwrap();
    let n2 = storage
        .create_node("Person", PropertyMapBuilder::new().build())
        .unwrap();

    storage
        .create_edge(n0, n1, "KNOWS", PropertyMapBuilder::new().build())
        .unwrap();
    storage
        .create_edge(n0, n2, "FOLLOWS", PropertyMapBuilder::new().build())
        .unwrap();

    // Get only KNOWS edges
    let knows_edges = storage.get_outgoing_edges_with_label(n0, "KNOWS");
    assert_eq!(knows_edges.len(), 1);

    // Get only FOLLOWS edges
    let follows_edges = storage.get_outgoing_edges_with_label(n0, "FOLLOWS");
    assert_eq!(follows_edges.len(), 1);

    // Non-existent label
    let none_edges = storage.get_outgoing_edges_with_label(n0, "LOVES");
    assert_eq!(none_edges.len(), 0);
}

#[test]
fn test_delete_node() {
    let storage = CurrentStorage::new();

    let node_id = storage
        .create_node("Person", PropertyMapBuilder::new().build())
        .unwrap();

    assert_eq!(storage.node_count(), 1);

    let deleted = storage.delete_node(node_id).unwrap();
    assert_eq!(deleted.id, node_id);
    assert_eq!(storage.node_count(), 0);

    // Second delete should fail
    assert!(storage.delete_node(node_id).is_err());
}

#[test]
fn test_delete_edge() {
    let storage = CurrentStorage::new();

    let n0 = storage
        .create_node("Person", PropertyMapBuilder::new().build())
        .unwrap();
    let n1 = storage
        .create_node("Person", PropertyMapBuilder::new().build())
        .unwrap();

    let edge_id = storage
        .create_edge(n0, n1, "KNOWS", PropertyMapBuilder::new().build())
        .unwrap();

    assert_eq!(storage.edge_count(), 1);
    assert_eq!(storage.out_degree(n0), 1);

    storage.delete_edge(edge_id).unwrap();

    assert_eq!(storage.edge_count(), 0);
    assert_eq!(storage.out_degree(n0), 0);
}

// ========================================================================
// Vector Property Tests (VS-011)
// ========================================================================

#[test]
fn test_create_node_with_vector_property() {
    let storage = CurrentStorage::new();

    // Create a node with an embedding vector
    let embedding = vec![0.1f32, 0.2, 0.3, 0.4, 0.5];
    let props = PropertyMapBuilder::new()
        .insert("name", "Document")
        .insert_vector("embedding", &embedding)
        .build();

    let node_id = storage.create_node("Document", props).unwrap();

    // Retrieve and verify
    let node = storage.get_node(node_id).unwrap();
    assert_eq!(
        node.get_property("name").and_then(|v| v.as_str()),
        Some("Document")
    );

    assert_eq!(
        node.get_property("embedding").and_then(|v| v.as_vector()),
        Some(&embedding[..])
    );
}

#[test]
fn test_create_node_with_high_dimensional_vector() {
    let storage = CurrentStorage::new();

    // Create a 384-dimensional vector (common embedding size)
    let embedding: Vec<f32> = (0..384).map(|i| i as f32 / 384.0).collect();
    let props = PropertyMapBuilder::new()
        .insert_vector("embedding", &embedding)
        .build();

    let node_id = storage.create_node("Embedding", props).unwrap();

    let node = storage.get_node(node_id).unwrap();
    assert_eq!(
        node.get_property("embedding").and_then(|v| v.as_vector()),
        Some(&embedding[..])
    );
}

#[test]
fn test_create_edge_with_vector_property() {
    let storage = CurrentStorage::new();

    // Create nodes
    let n1 = storage
        .create_node("Entity", PropertyMapBuilder::new().build())
        .unwrap();
    let n2 = storage
        .create_node("Entity", PropertyMapBuilder::new().build())
        .unwrap();

    // Create edge with a relationship embedding
    let edge_embedding = vec![0.5f32, -0.3, 0.8];
    let props = PropertyMapBuilder::new()
        .insert("weight", 0.95f64)
        .insert_vector("embedding", &edge_embedding)
        .build();

    let edge_id = storage.create_edge(n1, n2, "RELATES_TO", props).unwrap();

    // Retrieve and verify
    let edge = storage.get_edge(edge_id).unwrap();
    assert_eq!(
        edge.get_property("weight").and_then(|v| v.as_float()),
        Some(0.95)
    );

    assert_eq!(
        edge.get_property("embedding").and_then(|v| v.as_vector()),
        Some(&edge_embedding[..])
    );
}

#[test]
fn test_update_node_vector_property() {
    let storage = CurrentStorage::new();

    // Create node with initial embedding
    let initial_embedding = vec![0.1f32, 0.2, 0.3, 0.0];
    let props = PropertyMapBuilder::new()
        .insert("name", "Document")
        .insert_vector("embedding", &initial_embedding)
        .build();

    let node_id = storage.create_node("Document", props).unwrap();

    // Get node, update embedding, and save
    let mut node = storage.get_node(node_id).unwrap();

    // Update with new embedding
    let updated_embedding = vec![0.9f32, 0.8, 0.7, 0.0];
    let new_props = PropertyMapBuilder::new()
        .insert("name", "Document")
        .insert_vector("embedding", &updated_embedding)
        .build();
    node.properties = new_props;

    storage.update_node_direct(node, 1000.into()).unwrap();

    // Verify update
    let updated_node = storage.get_node(node_id).unwrap();
    assert_eq!(
        updated_node
            .get_property("embedding")
            .and_then(|v| v.as_vector()),
        Some(&updated_embedding[..])
    );
}

#[test]
fn test_update_edge_vector_property() {
    let storage = CurrentStorage::new();

    let n1 = storage
        .create_node("Entity", PropertyMapBuilder::new().build())
        .unwrap();
    let n2 = storage
        .create_node("Entity", PropertyMapBuilder::new().build())
        .unwrap();

    // Create edge with initial embedding
    let initial_embedding = vec![1.0f32, 0.0];
    let props = PropertyMapBuilder::new()
        .insert_vector("embedding", &initial_embedding)
        .build();

    let edge_id = storage.create_edge(n1, n2, "RELATES_TO", props).unwrap();

    // Update edge embedding
    let mut edge = storage.get_edge(edge_id).unwrap();
    let updated_embedding = vec![0.0f32, 1.0];
    edge.properties = PropertyMapBuilder::new()
        .insert_vector("embedding", &updated_embedding)
        .build();

    storage.update_edge_direct(edge).unwrap();

    // Verify
    let updated_edge = storage.get_edge(edge_id).unwrap();
    assert_eq!(
        updated_edge
            .get_property("embedding")
            .and_then(|v| v.as_vector()),
        Some(&updated_embedding[..])
    );
}

#[test]
fn test_create_node_with_multiple_vector_properties() {
    let storage = CurrentStorage::new();

    // Node with multiple embeddings (e.g., different model embeddings)
    let text_embedding = vec![0.1f32, 0.2, 0.3, 0.4];
    let image_embedding = vec![0.5f32, 0.6, 0.7, 0.8];
    let props = PropertyMapBuilder::new()
        .insert("content", "multimodal content")
        .insert_vector("text_embedding", &text_embedding)
        .insert_vector("image_embedding", &image_embedding)
        .build();

    let node_id = storage.create_node("MultimodalDoc", props).unwrap();

    let node = storage.get_node(node_id).unwrap();
    assert_eq!(
        node.get_property("text_embedding")
            .and_then(|v| v.as_vector()),
        Some(&text_embedding[..])
    );
    assert_eq!(
        node.get_property("image_embedding")
            .and_then(|v| v.as_vector()),
        Some(&image_embedding[..])
    );
}

#[test]
fn test_create_node_with_empty_vector() {
    let storage = CurrentStorage::new();

    // Empty vector should be allowed
    let empty_embedding: Vec<f32> = vec![];
    let props = PropertyMapBuilder::new()
        .insert_vector("embedding", &empty_embedding)
        .build();

    let node_id = storage.create_node("EmptyVec", props).unwrap();

    let node = storage.get_node(node_id).unwrap();
    assert_eq!(
        node.get_property("embedding").and_then(|v| v.as_vector()),
        Some(&empty_embedding[..])
    );
}

#[test]
fn test_create_node_with_normalized_vector() {
    let storage = CurrentStorage::new();

    // Vector with normalized values (common for embeddings)
    let normalized_embedding = vec![0.5773503f32, 0.5773503, 0.5773503, 0.0]; // unit vector
    let props = PropertyMapBuilder::new()
        .insert_vector("embedding", &normalized_embedding)
        .build();

    let node_id = storage.create_node("NormalizedDoc", props).unwrap();

    let node = storage.get_node(node_id).unwrap();
    let retrieved = node
        .get_property("embedding")
        .and_then(|v| v.as_vector())
        .expect("Embedding property should exist and be a vector");

    // Verify magnitude is approximately 1.0
    let magnitude: f32 = retrieved.iter().map(|x| x * x).sum::<f32>().sqrt();
    assert!((magnitude - 1.0).abs() < 1e-5);
}
// ========================================================================
// Vector Index Integration Tests (VS-030)
// ========================================================================

#[test]
fn test_enable_vector_index() {
    use crate::index::vector::DistanceMetric;
    let storage = CurrentStorage::new();
    assert!(!storage.is_vector_index_enabled());

    let config = HnswConfig::new(4, DistanceMetric::Cosine).with_capacity(100);
    storage.enable_vector_index("embedding", config).unwrap();
    assert!(storage.is_vector_index_enabled());
}

#[test]
fn test_enable_vector_index_twice_fails() {
    use crate::index::vector::DistanceMetric;
    let storage = CurrentStorage::new();

    let config = HnswConfig::new(4, DistanceMetric::Cosine).with_capacity(100);
    storage
        .enable_vector_index("embedding", config.clone())
        .unwrap();

    let result = storage.enable_vector_index("embedding", config);
    assert!(result.is_err());
}

#[test]
fn test_auto_index_on_create() {
    use crate::index::vector::DistanceMetric;
    let storage = CurrentStorage::new();

    let config = HnswConfig::new(4, DistanceMetric::Cosine).with_capacity(100);
    storage.enable_vector_index("embedding", config).unwrap();

    let embedding = vec![1.0f32, 0.0, 0.0, 0.0];
    let props = PropertyMapBuilder::new()
        .insert_vector("embedding", &embedding)
        .build();
    let node_id = storage.create_node("Document", props).unwrap();

    let node = storage.get_node(node_id).unwrap();
    assert_eq!(
        node.get_property("embedding").and_then(|v| v.as_vector()),
        Some(&embedding[..])
    );
}

#[test]
fn test_auto_index_dimension_mismatch_rolls_back() {
    use crate::index::vector::DistanceMetric;
    let storage = CurrentStorage::new();

    let config = HnswConfig::new(4, DistanceMetric::Cosine).with_capacity(100);
    storage.enable_vector_index("embedding", config).unwrap();

    // Wrong dimension - 2D instead of 3D
    let wrong_embedding = vec![1.0f32, 0.0];
    let props = PropertyMapBuilder::new()
        .insert_vector("embedding", &wrong_embedding)
        .build();

    let result = storage.create_node("Document", props);
    assert!(result.is_err());
    assert_eq!(storage.node_count(), 0); // Rollback worked
}

#[test]
fn test_find_similar() {
    use crate::index::vector::DistanceMetric;
    let storage = CurrentStorage::new();

    let config = HnswConfig::new(4, DistanceMetric::Cosine).with_capacity(100);
    storage.enable_vector_index("embedding", config).unwrap();

    let v1 = vec![1.0f32, 0.0, 0.0, 0.0];
    let v2 = vec![0.9f32, 0.1, 0.0, 0.0];
    let v3 = vec![0.0f32, 1.0, 0.0, 0.0];

    let node1 = storage
        .create_node(
            "Doc",
            PropertyMapBuilder::new()
                .insert_vector("embedding", &v1)
                .build(),
        )
        .unwrap();
    let node2 = storage
        .create_node(
            "Doc",
            PropertyMapBuilder::new()
                .insert_vector("embedding", &v2)
                .build(),
        )
        .unwrap();
    let _node3 = storage
        .create_node(
            "Doc",
            PropertyMapBuilder::new()
                .insert_vector("embedding", &v3)
                .build(),
        )
        .unwrap();

    let results = storage.find_similar(node1, 2).unwrap();
    assert_eq!(results.len(), 2);
    assert!(results.iter().all(|(id, _)| *id != node1));
    assert_eq!(results[0].0, node2); // Most similar
}

/// Test for Issue #323: delete_node() should remove vectors from HNSW index
///
/// This test verifies that CurrentStorage::delete_node() properly removes
/// vector embeddings from the HNSW index, preventing memory leaks and
/// incorrect search results.
#[test]
fn test_delete_node_removes_from_vector_index() {
    use crate::index::vector::DistanceMetric;
    let storage = CurrentStorage::new();

    // Enable vector index
    let config = HnswConfig::new(4, DistanceMetric::Cosine).with_capacity(100);
    storage.enable_vector_index("embedding", config).unwrap();

    // Create two nodes with embeddings
    let v1 = vec![1.0f32, 0.0, 0.0, 0.0];
    let v2 = vec![0.0f32, 1.0, 0.0, 0.0];

    let node1 = storage
        .create_node(
            "Doc",
            PropertyMapBuilder::new()
                .insert_vector("embedding", &v1)
                .build(),
        )
        .unwrap();

    let node2 = storage
        .create_node(
            "Doc",
            PropertyMapBuilder::new()
                .insert_vector("embedding", &v2)
                .build(),
        )
        .unwrap();

    // Verify both nodes are in the index
    let results_before = storage.find_similar_by_embedding(&v1, 10).unwrap();
    assert_eq!(
        results_before.len(),
        2,
        "Should find 2 nodes before deletion"
    );

    // Delete node1
    storage.delete_node(node1).unwrap();

    // Verify node1 is removed from the index
    let results_after = storage.find_similar_by_embedding(&v1, 10).unwrap();
    assert_eq!(
        results_after.len(),
        1,
        "Should find only 1 node after deletion"
    );
    assert_eq!(results_after[0].0, node2, "Remaining node should be node2");

    // Deleted node should not appear in find_similar results
    let similar_to_node2 = storage.find_similar(node2, 10).unwrap();
    assert!(
        !similar_to_node2.iter().any(|(id, _)| *id == node1),
        "Deleted node should not appear in similarity search"
    );
}

#[test]
fn test_find_similar_with_label() {
    use crate::index::vector::DistanceMetric;
    let storage = CurrentStorage::new();

    let config = HnswConfig::new(4, DistanceMetric::Cosine).with_capacity(100);
    storage.enable_vector_index("embedding", config).unwrap();

    let v1 = vec![1.0f32, 0.0, 0.0, 0.0];
    let v2 = vec![0.9f32, 0.1, 0.0, 0.0];
    let v3 = vec![0.8f32, 0.2, 0.0, 0.0];

    let node1 = storage
        .create_node(
            "Person",
            PropertyMapBuilder::new()
                .insert_vector("embedding", &v1)
                .build(),
        )
        .unwrap();
    let node2 = storage
        .create_node(
            "Person",
            PropertyMapBuilder::new()
                .insert_vector("embedding", &v2)
                .build(),
        )
        .unwrap();
    let _node3 = storage
        .create_node(
            "Document",
            PropertyMapBuilder::new()
                .insert_vector("embedding", &v3)
                .build(),
        )
        .unwrap();

    let results = storage.find_similar_with_label(node1, "Person", 2).unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].0, node2);
}

#[test]
fn test_find_similar_index_not_enabled() {
    let storage = CurrentStorage::new();
    let v1 = vec![1.0f32, 0.0, 0.0, 0.0];
    let node1 = storage
        .create_node(
            "Doc",
            PropertyMapBuilder::new()
                .insert_vector("embedding", &v1)
                .build(),
        )
        .unwrap();

    let result = storage.find_similar(node1, 2);
    assert!(result.is_err());
}

#[test]
fn test_find_similar_node_not_found() {
    use crate::index::vector::DistanceMetric;
    let storage = CurrentStorage::new();

    let config = HnswConfig::new(4, DistanceMetric::Cosine).with_capacity(100);
    storage.enable_vector_index("embedding", config).unwrap();

    let result = storage.find_similar(NodeId::new(999).unwrap(), 2);
    assert!(result.is_err());
}

#[test]
fn test_find_similar_property_not_found() {
    use crate::index::vector::DistanceMetric;
    let storage = CurrentStorage::new();

    let config = HnswConfig::new(4, DistanceMetric::Cosine).with_capacity(100);
    storage.enable_vector_index("embedding", config).unwrap();

    let node1 = storage
        .create_node(
            "Doc",
            PropertyMapBuilder::new().insert("name", "test").build(),
        )
        .unwrap();
    let result = storage.find_similar(node1, 2);
    assert!(result.is_err());
}

#[test]
fn test_update_node_updates_index() {
    use crate::index::vector::DistanceMetric;
    let storage = CurrentStorage::new();

    let config = HnswConfig::new(4, DistanceMetric::Cosine).with_capacity(100);
    storage.enable_vector_index("embedding", config).unwrap();

    let v1 = vec![1.0f32, 0.0, 0.0, 0.0];
    let v2 = vec![0.0f32, 1.0, 0.0, 0.0];

    let node1 = storage
        .create_node(
            "Doc",
            PropertyMapBuilder::new()
                .insert_vector("embedding", &v1)
                .build(),
        )
        .unwrap();
    let node2 = storage
        .create_node(
            "Doc",
            PropertyMapBuilder::new()
                .insert_vector("embedding", &v2)
                .build(),
        )
        .unwrap();

    // Update node1 to be similar to node2
    let v1_updated = vec![0.1f32, 0.9, 0.0, 0.0];
    let mut node1_obj = storage.get_node(node1).unwrap();
    node1_obj.properties = PropertyMapBuilder::new()
        .insert_vector("embedding", &v1_updated)
        .build();
    storage.update_node_direct(node1_obj, 2000.into()).unwrap();

    let results = storage.find_similar(node2, 1).unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].0, node1);
}

#[test]
fn test_delete_node_removes_from_index() {
    use crate::index::vector::DistanceMetric;
    let storage = CurrentStorage::new();

    let config = HnswConfig::new(4, DistanceMetric::Cosine).with_capacity(100);
    storage.enable_vector_index("embedding", config).unwrap();

    let v1 = vec![1.0f32, 0.0, 0.0, 0.0];
    let v2 = vec![0.9f32, 0.1, 0.0, 0.0];

    let node1 = storage
        .create_node(
            "Doc",
            PropertyMapBuilder::new()
                .insert_vector("embedding", &v1)
                .build(),
        )
        .unwrap();
    let node2 = storage
        .create_node(
            "Doc",
            PropertyMapBuilder::new()
                .insert_vector("embedding", &v2)
                .build(),
        )
        .unwrap();

    storage.delete_node_direct(node2, 3000.into()).unwrap();

    let results = storage.find_similar(node1, 2).unwrap();
    assert_eq!(results.len(), 0);
}

#[test]
fn test_create_node_without_vector_property() {
    use crate::index::vector::DistanceMetric;
    let storage = CurrentStorage::new();

    let config = HnswConfig::new(4, DistanceMetric::Cosine).with_capacity(100);
    storage.enable_vector_index("embedding", config).unwrap();

    // Create node without vector - should succeed
    let node1 = storage
        .create_node(
            "Doc",
            PropertyMapBuilder::new().insert("name", "test").build(),
        )
        .unwrap();
    assert_eq!(storage.node_count(), 1);
    let node = storage.get_node(node1).unwrap();
    assert_eq!(
        node.get_property("name").and_then(|v| v.as_str()),
        Some("test")
    );
}

// ========================================================================
// Tests for Issue #24: delete_node/delete_edge with &self (P3-2)
// ========================================================================

/// Test that delete_node can be called with an immutable reference.
///
/// This test verifies that delete_node(&self) works correctly since
/// the underlying DashMap doesn't require &mut self.
#[test]
fn test_delete_node_with_immutable_reference() {
    let storage = CurrentStorage::new();

    // Create a node
    let node_id = storage
        .create_node("Person", PropertyMapBuilder::new().build())
        .unwrap();

    assert_eq!(storage.node_count(), 1);

    // Delete using immutable reference - this should compile and work
    // Thanks to DashMap's interior mutability
    let deleted = storage.delete_node(node_id).unwrap();
    assert_eq!(deleted.id, node_id);
    assert_eq!(storage.node_count(), 0);
}

/// Test that delete_edge can be called with an immutable reference.
///
/// This test verifies that delete_edge(&self) works correctly since
/// the underlying DashMap doesn't require &mut self.
#[test]
fn test_delete_edge_with_immutable_reference() {
    let storage = CurrentStorage::new();

    // Create two nodes and an edge
    let n0 = storage
        .create_node("Person", PropertyMapBuilder::new().build())
        .unwrap();
    let n1 = storage
        .create_node("Person", PropertyMapBuilder::new().build())
        .unwrap();

    let edge_id = storage
        .create_edge(n0, n1, "KNOWS", PropertyMapBuilder::new().build())
        .unwrap();

    assert_eq!(storage.edge_count(), 1);

    // Delete using immutable reference - this should compile and work
    // Thanks to DashMap's interior mutability
    let deleted = storage.delete_edge(edge_id).unwrap();
    assert_eq!(deleted.id, edge_id);
    assert_eq!(storage.edge_count(), 0);
}

/// Test that delete operations can be called from a shared reference context.
///
/// This test demonstrates a real-world scenario where we have a shared
/// reference to storage and need to perform deletes.
#[test]
fn test_delete_operations_in_shared_context() {
    let storage = CurrentStorage::new();

    // Create test data
    let node1 = storage
        .create_node("Person", PropertyMapBuilder::new().build())
        .unwrap();
    let node2 = storage
        .create_node("Person", PropertyMapBuilder::new().build())
        .unwrap();
    let edge_id = storage
        .create_edge(node1, node2, "KNOWS", PropertyMapBuilder::new().build())
        .unwrap();

    assert_eq!(storage.node_count(), 2);
    assert_eq!(storage.edge_count(), 1);

    // Helper function that takes &CurrentStorage (shared reference)
    fn delete_data(storage: &CurrentStorage, node_id: NodeId, edge_id: EdgeId) {
        storage.delete_edge(edge_id).unwrap();
        storage.delete_node(node_id).unwrap();
    }

    // This should compile because delete methods accept &self
    delete_data(&storage, node1, edge_id);

    assert_eq!(storage.node_count(), 1);
    assert_eq!(storage.edge_count(), 0);
}

// ========================================================================
// Tests for Issue #389: Multi-property Vector Index API
// ========================================================================

/// Test that multiple vector indexes can be enabled on different properties.
///
/// This is the core multi-property feature: users should be able to index
/// "title_embedding" and "body_embedding" simultaneously with different configs.
#[test]
fn test_enable_multiple_vector_indexes_on_different_properties() {
    use crate::index::vector::DistanceMetric;
    let storage = CurrentStorage::new();

    // Enable first index on "title_embedding" (384 dimensions)
    let config1 = HnswConfig::new(384, DistanceMetric::Cosine).with_capacity(100);
    storage
        .enable_vector_index("title_embedding", config1)
        .expect("Should enable first vector index");

    // Enable second index on "body_embedding" (1536 dimensions)
    let config2 = HnswConfig::new(1536, DistanceMetric::Cosine).with_capacity(100);
    storage
        .enable_vector_index("body_embedding", config2)
        .expect("Should enable second vector index on different property");

    // Both should be enabled
    assert!(storage.has_vector_index("title_embedding"));
    assert!(storage.has_vector_index("body_embedding"));
}

/// Test that re-enabling the same property fails.
///
/// While different properties can have indexes, the same property
/// cannot be indexed twice.
#[test]
fn test_enable_same_property_twice_fails() {
    use crate::index::vector::DistanceMetric;
    let storage = CurrentStorage::new();

    let config = HnswConfig::new(384, DistanceMetric::Cosine).with_capacity(100);
    storage
        .enable_vector_index("embedding", config.clone())
        .unwrap();

    // Second enable on SAME property should fail
    let result = storage.enable_vector_index("embedding", config);
    assert!(
        result.is_err(),
        "Should not allow re-enabling same property"
    );
}

/// Test find_similar_in to search a specific property's index.
///
/// With multiple indexes, users need to specify which property to search.
#[test]
fn test_find_similar_in_specific_property() {
    use crate::index::vector::DistanceMetric;
    let storage = CurrentStorage::new();

    // Enable two indexes with different dimensions
    let config_title = HnswConfig::new(4, DistanceMetric::Cosine).with_capacity(100);
    let config_body = HnswConfig::new(8, DistanceMetric::Cosine).with_capacity(100);
    storage
        .enable_vector_index("title_embedding", config_title)
        .unwrap();
    storage
        .enable_vector_index("body_embedding", config_body)
        .unwrap();

    // Create nodes with both properties
    let title_vec1 = vec![1.0f32, 0.0, 0.0, 0.0];
    let title_vec2 = vec![0.9f32, 0.1, 0.0, 0.0];
    let body_vec1 = vec![1.0f32, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0];
    let body_vec2 = vec![0.0f32, 1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0]; // Different direction

    let node1 = storage
        .create_node(
            "Document",
            PropertyMapBuilder::new()
                .insert_vector("title_embedding", &title_vec1)
                .insert_vector("body_embedding", &body_vec1)
                .build(),
        )
        .unwrap();
    let node2 = storage
        .create_node(
            "Document",
            PropertyMapBuilder::new()
                .insert_vector("title_embedding", &title_vec2)
                .insert_vector("body_embedding", &body_vec2)
                .build(),
        )
        .unwrap();

    // Search title_embedding - node2 should be similar to node1
    let title_results = storage
        .find_similar_in("title_embedding", node1, 1)
        .unwrap();
    assert_eq!(title_results.len(), 1);
    assert_eq!(title_results[0].0, node2);

    // Search body_embedding - node2 is orthogonal to node1, so less similar
    let body_results = storage.find_similar_in("body_embedding", node1, 1).unwrap();
    assert_eq!(body_results.len(), 1);
    // Cosine similarity with orthogonal vectors should be ~0
    assert!(
        body_results[0].1 < 0.5,
        "Orthogonal vectors should have low similarity"
    );
}

/// Test that find_similar_in fails for non-indexed property.
#[test]
fn test_find_similar_in_property_not_indexed() {
    use crate::index::vector::DistanceMetric;
    let storage = CurrentStorage::new();

    let config = HnswConfig::new(4, DistanceMetric::Cosine).with_capacity(100);
    storage
        .enable_vector_index("title_embedding", config)
        .unwrap();

    let vec1 = vec![1.0f32, 0.0, 0.0, 0.0];
    let node1 = storage
        .create_node(
            "Document",
            PropertyMapBuilder::new()
                .insert_vector("title_embedding", &vec1)
                .build(),
        )
        .unwrap();

    // Search on non-indexed property should fail
    let result = storage.find_similar_in("body_embedding", node1, 1);
    assert!(result.is_err(), "Should fail for non-indexed property");
}

/// Test list_vector_indexes returns all configured indexes.
#[test]
fn test_list_vector_indexes() {
    use crate::index::vector::DistanceMetric;
    let storage = CurrentStorage::new();

    // Initially empty
    let indexes = storage.list_vector_indexes();
    assert!(indexes.is_empty());

    // Add two indexes
    let config1 = HnswConfig::new(384, DistanceMetric::Cosine).with_capacity(100);
    let config2 = HnswConfig::new(1536, DistanceMetric::Euclidean).with_capacity(200);
    storage
        .enable_vector_index("title_embedding", config1)
        .unwrap();
    storage
        .enable_vector_index("body_embedding", config2)
        .unwrap();

    // Should list both
    let indexes = storage.list_vector_indexes();
    assert_eq!(indexes.len(), 2);

    let names: Vec<&str> = indexes.iter().map(|i| i.property_name.as_str()).collect();
    assert!(names.contains(&"title_embedding"));
    assert!(names.contains(&"body_embedding"));

    // Verify configs are preserved
    let title_idx = indexes
        .iter()
        .find(|i| i.property_name == "title_embedding")
        .unwrap();
    assert_eq!(title_idx.dimensions, 384);
    assert_eq!(title_idx.distance_metric, DistanceMetric::Cosine);

    let body_idx = indexes
        .iter()
        .find(|i| i.property_name == "body_embedding")
        .unwrap();
    assert_eq!(body_idx.dimensions, 1536);
    assert_eq!(body_idx.distance_metric, DistanceMetric::Euclidean);
}

/// Test has_vector_index for specific properties.
#[test]
fn test_has_vector_index_specific_property() {
    use crate::index::vector::DistanceMetric;
    let storage = CurrentStorage::new();

    assert!(!storage.has_vector_index("title_embedding"));
    assert!(!storage.has_vector_index("body_embedding"));

    let config = HnswConfig::new(384, DistanceMetric::Cosine).with_capacity(100);
    storage
        .enable_vector_index("title_embedding", config)
        .unwrap();

    assert!(storage.has_vector_index("title_embedding"));
    assert!(!storage.has_vector_index("body_embedding")); // Still not indexed
}

/// Test auto-indexing only indexes properties that have enabled indexes.
#[test]
fn test_auto_index_multiple_properties_selective() {
    use crate::index::vector::DistanceMetric;
    let storage = CurrentStorage::new();

    // Only enable index for title_embedding
    let config = HnswConfig::new(4, DistanceMetric::Cosine).with_capacity(100);
    storage
        .enable_vector_index("title_embedding", config)
        .unwrap();

    // Create node with both properties
    let title_vec = vec![1.0f32, 0.0, 0.0, 0.0];
    let body_vec = vec![1.0f32, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0];
    let node1 = storage
        .create_node(
            "Document",
            PropertyMapBuilder::new()
                .insert_vector("title_embedding", &title_vec)
                .insert_vector("body_embedding", &body_vec)
                .build(),
        )
        .unwrap();

    // Create another node for similarity search
    let title_vec2 = vec![0.9f32, 0.1, 0.0, 0.0];
    let _node2 = storage
        .create_node(
            "Document",
            PropertyMapBuilder::new()
                .insert_vector("title_embedding", &title_vec2)
                .build(),
        )
        .unwrap();

    // Search on title should work
    let results = storage
        .find_similar_in("title_embedding", node1, 1)
        .unwrap();
    assert_eq!(results.len(), 1);

    // Search on body should fail (not indexed)
    let result = storage.find_similar_in("body_embedding", node1, 1);
    assert!(result.is_err());
}

/// Test update_node updates the correct property index.
#[test]
fn test_update_node_updates_correct_property_index() {
    use crate::index::vector::DistanceMetric;
    let storage = CurrentStorage::new();

    // Enable both indexes
    let config = HnswConfig::new(4, DistanceMetric::Cosine).with_capacity(100);
    storage
        .enable_vector_index("title_embedding", config.clone())
        .unwrap();
    storage
        .enable_vector_index("body_embedding", config)
        .unwrap();

    // Create nodes
    let v1 = vec![1.0f32, 0.0, 0.0, 0.0];
    let v2 = vec![0.0f32, 1.0, 0.0, 0.0];
    let node1 = storage
        .create_node(
            "Document",
            PropertyMapBuilder::new()
                .insert_vector("title_embedding", &v1)
                .insert_vector("body_embedding", &v1)
                .build(),
        )
        .unwrap();
    let node2 = storage
        .create_node(
            "Document",
            PropertyMapBuilder::new()
                .insert_vector("title_embedding", &v2)
                .insert_vector("body_embedding", &v2)
                .build(),
        )
        .unwrap();

    // Update node1's title_embedding to be similar to node2
    let v1_updated = vec![0.1f32, 0.9, 0.0, 0.0];
    let mut node1_obj = storage.get_node(node1).unwrap();
    node1_obj.properties = PropertyMapBuilder::new()
        .insert_vector("title_embedding", &v1_updated)
        .insert_vector("body_embedding", &v1) // Keep body the same
        .build();
    storage.update_node_direct(node1_obj, 2000.into()).unwrap();

    // Title search should now find node1 as similar to node2
    let title_results = storage
        .find_similar_in("title_embedding", node2, 1)
        .unwrap();
    assert_eq!(title_results[0].0, node1);

    // Body search should still find nodes dissimilar (orthogonal)
    let body_results = storage.find_similar_in("body_embedding", node2, 1).unwrap();
    // node1's body_embedding is still v1, orthogonal to v2
    assert!(body_results[0].1 < 0.5);
}

/// Test delete_node removes from all property indexes.
#[test]
fn test_delete_node_removes_from_all_indexes() {
    use crate::index::vector::DistanceMetric;
    let storage = CurrentStorage::new();

    // Enable two indexes
    let config = HnswConfig::new(4, DistanceMetric::Cosine).with_capacity(100);
    storage
        .enable_vector_index("title_embedding", config.clone())
        .unwrap();
    storage
        .enable_vector_index("body_embedding", config)
        .unwrap();

    let v1 = vec![1.0f32, 0.0, 0.0, 0.0];
    let v2 = vec![0.9f32, 0.1, 0.0, 0.0];

    let node1 = storage
        .create_node(
            "Document",
            PropertyMapBuilder::new()
                .insert_vector("title_embedding", &v1)
                .insert_vector("body_embedding", &v1)
                .build(),
        )
        .unwrap();
    let node2 = storage
        .create_node(
            "Document",
            PropertyMapBuilder::new()
                .insert_vector("title_embedding", &v2)
                .insert_vector("body_embedding", &v2)
                .build(),
        )
        .unwrap();

    // Delete node2
    storage.delete_node_direct(node2, 3000.into()).unwrap();

    // Search from node1 should return empty in both indexes
    let title_results = storage
        .find_similar_in("title_embedding", node1, 2)
        .unwrap();
    assert_eq!(title_results.len(), 0);

    let body_results = storage.find_similar_in("body_embedding", node1, 2).unwrap();
    assert_eq!(body_results.len(), 0);
}

/// Test search_vectors_in for direct embedding queries.
#[test]
fn test_search_vectors_in_by_embedding() {
    use crate::index::vector::DistanceMetric;
    let storage = CurrentStorage::new();

    let config = HnswConfig::new(4, DistanceMetric::Cosine).with_capacity(100);
    storage.enable_vector_index("embedding", config).unwrap();

    let v1 = vec![1.0f32, 0.0, 0.0, 0.0];
    let v2 = vec![0.9f32, 0.1, 0.0, 0.0];
    let v3 = vec![0.0f32, 1.0, 0.0, 0.0];

    let node1 = storage
        .create_node(
            "Doc",
            PropertyMapBuilder::new()
                .insert_vector("embedding", &v1)
                .build(),
        )
        .unwrap();
    let node2 = storage
        .create_node(
            "Doc",
            PropertyMapBuilder::new()
                .insert_vector("embedding", &v2)
                .build(),
        )
        .unwrap();
    let _node3 = storage
        .create_node(
            "Doc",
            PropertyMapBuilder::new()
                .insert_vector("embedding", &v3)
                .build(),
        )
        .unwrap();

    // Search with direct embedding
    let query = vec![1.0f32, 0.0, 0.0, 0.0];
    let results = storage.search_vectors_in("embedding", &query, 2).unwrap();
    assert_eq!(results.len(), 2);
    // v1 is identical to query, so node1 is most similar
    assert_eq!(results[0].0, node1);
    // v2 is second most similar (close to [1,0,0,0])
    assert_eq!(results[1].0, node2);
}

/// Test dimension mismatch fails for specific property.
#[test]
fn test_dimension_mismatch_specific_property() {
    use crate::index::vector::DistanceMetric;
    let storage = CurrentStorage::new();

    // Title: 4D, Body: 8D
    let config_title = HnswConfig::new(4, DistanceMetric::Cosine).with_capacity(100);
    let config_body = HnswConfig::new(8, DistanceMetric::Cosine).with_capacity(100);
    storage
        .enable_vector_index("title_embedding", config_title)
        .unwrap();
    storage
        .enable_vector_index("body_embedding", config_body)
        .unwrap();

    // Wrong dimension for title (8D instead of 4D)
    let wrong_title = vec![1.0f32, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0];
    let correct_body = vec![1.0f32, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0];

    let result = storage.create_node(
        "Document",
        PropertyMapBuilder::new()
            .insert_vector("title_embedding", &wrong_title)
            .insert_vector("body_embedding", &correct_body)
            .build(),
    );

    assert!(result.is_err(), "Should fail on dimension mismatch");
}

// TDD tests for Issue #187 - Performance optimization for graph traversal methods
// These tests verify the new iterator-based API that avoids unnecessary Vec allocations

#[test]
fn test_get_outgoing_edges_iter_basic() {
    let storage = CurrentStorage::new();

    // Create nodes
    let n0 = storage
        .create_node("Person", PropertyMapBuilder::new().build())
        .unwrap();
    let n1 = storage
        .create_node("Person", PropertyMapBuilder::new().build())
        .unwrap();
    let n2 = storage
        .create_node("Person", PropertyMapBuilder::new().build())
        .unwrap();

    // Create edges
    let e1 = storage
        .create_edge(n0, n1, "KNOWS", PropertyMapBuilder::new().build())
        .unwrap();
    let e2 = storage
        .create_edge(n0, n2, "FOLLOWS", PropertyMapBuilder::new().build())
        .unwrap();

    // Test iterator returns same results as Vec version using HashSet for robust comparison
    let vec_result: std::collections::HashSet<EdgeId> =
        storage.get_outgoing_edges(n0).into_iter().collect();
    let iter_result: std::collections::HashSet<EdgeId> =
        storage.get_outgoing_edges_iter(n0).collect();

    assert_eq!(vec_result, iter_result);
    assert!(iter_result.contains(&e1));
    assert!(iter_result.contains(&e2));
}

#[test]
fn test_get_outgoing_edges_iter_empty() {
    let storage = CurrentStorage::new();

    let n0 = storage
        .create_node("Person", PropertyMapBuilder::new().build())
        .unwrap();

    // Node with no outgoing edges
    let count = storage.get_outgoing_edges_iter(n0).count();
    assert_eq!(count, 0);
}

#[test]
fn test_get_incoming_edges_iter_basic() {
    let storage = CurrentStorage::new();

    // Create nodes
    let n0 = storage
        .create_node("Person", PropertyMapBuilder::new().build())
        .unwrap();
    let n1 = storage
        .create_node("Person", PropertyMapBuilder::new().build())
        .unwrap();
    let n2 = storage
        .create_node("Person", PropertyMapBuilder::new().build())
        .unwrap();

    // Create edges pointing to n2
    let e1 = storage
        .create_edge(n0, n2, "KNOWS", PropertyMapBuilder::new().build())
        .unwrap();
    let e2 = storage
        .create_edge(n1, n2, "FOLLOWS", PropertyMapBuilder::new().build())
        .unwrap();

    // Test iterator returns same results as Vec version using HashSet for robust comparison
    let vec_result: std::collections::HashSet<EdgeId> =
        storage.get_incoming_edges(n2).into_iter().collect();
    let iter_result: std::collections::HashSet<EdgeId> =
        storage.get_incoming_edges_iter(n2).collect();

    assert_eq!(vec_result, iter_result);
    assert!(iter_result.contains(&e1));
    assert!(iter_result.contains(&e2));
}

#[test]
fn test_get_incoming_edges_iter_empty() {
    let storage = CurrentStorage::new();

    let n0 = storage
        .create_node("Person", PropertyMapBuilder::new().build())
        .unwrap();

    // Node with no incoming edges
    let count = storage.get_incoming_edges_iter(n0).count();
    assert_eq!(count, 0);
}

#[test]
fn test_get_outgoing_edges_with_label_iter_basic() {
    let storage = CurrentStorage::new();

    let n0 = storage
        .create_node("Person", PropertyMapBuilder::new().build())
        .unwrap();
    let n1 = storage
        .create_node("Person", PropertyMapBuilder::new().build())
        .unwrap();
    let n2 = storage
        .create_node("Person", PropertyMapBuilder::new().build())
        .unwrap();

    // Create edges with different labels
    let e1 = storage
        .create_edge(n0, n1, "KNOWS", PropertyMapBuilder::new().build())
        .unwrap();
    let _e2 = storage
        .create_edge(n0, n2, "FOLLOWS", PropertyMapBuilder::new().build())
        .unwrap();

    // Test iterator with label filter
    let iter_result: Vec<EdgeId> = storage
        .get_outgoing_edges_with_label_iter(n0, "KNOWS")
        .collect();
    assert_eq!(iter_result.len(), 1);
    assert!(iter_result.contains(&e1));

    // Compare with Vec version
    let vec_result = storage.get_outgoing_edges_with_label(n0, "KNOWS");
    assert_eq!(vec_result.len(), iter_result.len());
}

#[test]
fn test_get_outgoing_edges_with_label_iter_nonexistent_label() {
    let storage = CurrentStorage::new();

    let n0 = storage
        .create_node("Person", PropertyMapBuilder::new().build())
        .unwrap();
    let n1 = storage
        .create_node("Person", PropertyMapBuilder::new().build())
        .unwrap();

    storage
        .create_edge(n0, n1, "KNOWS", PropertyMapBuilder::new().build())
        .unwrap();

    // Non-existent label should return empty iterator
    let count = storage
        .get_outgoing_edges_with_label_iter(n0, "LOVES")
        .count();
    assert_eq!(count, 0);
}

#[test]
fn test_get_incoming_edges_with_label_iter_basic() {
    let storage = CurrentStorage::new();

    let n0 = storage
        .create_node("Person", PropertyMapBuilder::new().build())
        .unwrap();
    let n1 = storage
        .create_node("Person", PropertyMapBuilder::new().build())
        .unwrap();
    let n2 = storage
        .create_node("Person", PropertyMapBuilder::new().build())
        .unwrap();

    // Create edges with different labels pointing to n2
    let e1 = storage
        .create_edge(n0, n2, "KNOWS", PropertyMapBuilder::new().build())
        .unwrap();
    let _e2 = storage
        .create_edge(n1, n2, "FOLLOWS", PropertyMapBuilder::new().build())
        .unwrap();

    // Test iterator with label filter
    let iter_result: Vec<EdgeId> = storage
        .get_incoming_edges_with_label_iter(n2, "KNOWS")
        .collect();
    assert_eq!(iter_result.len(), 1);
    assert!(iter_result.contains(&e1));

    // Compare with Vec version
    let vec_result = storage.get_incoming_edges_with_label(n2, "KNOWS");
    assert_eq!(vec_result.len(), iter_result.len());
}

#[test]
fn test_get_incoming_edges_with_label_iter_nonexistent_label() {
    let storage = CurrentStorage::new();

    let n0 = storage
        .create_node("Person", PropertyMapBuilder::new().build())
        .unwrap();
    let n1 = storage
        .create_node("Person", PropertyMapBuilder::new().build())
        .unwrap();

    storage
        .create_edge(n0, n1, "KNOWS", PropertyMapBuilder::new().build())
        .unwrap();

    // Non-existent label should return empty iterator
    let count = storage
        .get_incoming_edges_with_label_iter(n1, "LOVES")
        .count();
    assert_eq!(count, 0);
}

#[test]
fn test_iterator_can_be_partially_consumed() {
    let storage = CurrentStorage::new();

    let n0 = storage
        .create_node("Person", PropertyMapBuilder::new().build())
        .unwrap();
    let n1 = storage
        .create_node("Person", PropertyMapBuilder::new().build())
        .unwrap();
    let n2 = storage
        .create_node("Person", PropertyMapBuilder::new().build())
        .unwrap();
    let n3 = storage
        .create_node("Person", PropertyMapBuilder::new().build())
        .unwrap();

    // Create multiple edges
    storage
        .create_edge(n0, n1, "KNOWS", PropertyMapBuilder::new().build())
        .unwrap();
    storage
        .create_edge(n0, n2, "KNOWS", PropertyMapBuilder::new().build())
        .unwrap();
    storage
        .create_edge(n0, n3, "KNOWS", PropertyMapBuilder::new().build())
        .unwrap();

    // Take only first 2 elements - demonstrating lazy evaluation benefit
    let first_two: Vec<EdgeId> = storage.get_outgoing_edges_iter(n0).take(2).collect();
    assert_eq!(first_two.len(), 2);
}

#[test]
fn test_iterator_consistency_with_vec() {
    let storage = CurrentStorage::new();

    // Create a more complex graph
    let nodes: Vec<NodeId> = (0..5)
        .map(|_| {
            storage
                .create_node("Person", PropertyMapBuilder::new().build())
                .unwrap()
        })
        .collect();

    // Create a star pattern: n0 -> n1, n2, n3, n4
    for i in 1..5 {
        storage
            .create_edge(
                nodes[0],
                nodes[i],
                "KNOWS",
                PropertyMapBuilder::new().build(),
            )
            .unwrap();
    }

    // Verify iterator and Vec return same edges (order may differ)
    let vec_edges: std::collections::HashSet<EdgeId> =
        storage.get_outgoing_edges(nodes[0]).into_iter().collect();
    let iter_edges: std::collections::HashSet<EdgeId> =
        storage.get_outgoing_edges_iter(nodes[0]).collect();

    assert_eq!(vec_edges, iter_edges);
}

#[test]
fn test_legacy_default_property_selection_determinism() {
    use crate::index::vector::DistanceMetric;
    let storage = CurrentStorage::new();

    let config = HnswConfig::new(4, DistanceMetric::Cosine).with_capacity(100);

    // Insert in reverse alphabetical order to ensure min() is doing the work
    // "z_prop" inserted first
    storage
        .enable_vector_index("z_prop", config.clone())
        .unwrap();
    // "a_prop" inserted second
    storage
        .enable_vector_index("a_prop", config.clone())
        .unwrap();

    // Should pick "a_prop" alphabetically (deterministic default)
    assert_eq!(
        storage.get_indexed_property_name(),
        Some("a_prop".to_string())
    );

    // Also verify get_vector_property_name alias
    assert_eq!(
        storage.get_vector_property_name(),
        Some("a_prop".to_string())
    );
}

#[test]
fn test_legacy_vector_count() {
    use crate::index::vector::DistanceMetric;
    let storage = CurrentStorage::new();

    // Count with no index
    assert_eq!(storage.vector_count(), 0);

    let config = HnswConfig::new(4, DistanceMetric::Cosine).with_capacity(100);
    storage.enable_vector_index("embedding", config).unwrap();

    // Empty index count
    assert_eq!(storage.vector_count(), 0);

    // Add a node with vector
    let v1 = vec![1.0f32, 0.0, 0.0, 0.0];
    let _node1 = storage
        .create_node(
            "Doc",
            PropertyMapBuilder::new()
                .insert_vector("embedding", &v1)
                .build(),
        )
        .unwrap();

    // Count should be 1
    assert_eq!(storage.vector_count(), 1);
}

// Tests for zero-allocation entries iterators (Issue #405)

#[test]
fn test_get_outgoing_entries_iter_basic() {
    let storage = CurrentStorage::new();
    let n0 = storage.create_node("Person", PropertyMapBuilder::new().build()).unwrap();
    let n1 = storage.create_node("Person", PropertyMapBuilder::new().build()).unwrap();
    let e1 = storage.create_edge(n0, n1, "KNOWS", PropertyMapBuilder::new().build()).unwrap();

    let entries: Vec<_> = storage.get_outgoing_entries_iter(n0).collect();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].edge_id, e1);
    assert_eq!(entries[0].target, n1);
}

#[test]
fn test_get_incoming_entries_iter_basic() {
    let storage = CurrentStorage::new();
    let n0 = storage.create_node("Person", PropertyMapBuilder::new().build()).unwrap();
    let n1 = storage.create_node("Person", PropertyMapBuilder::new().build()).unwrap();
    let e1 = storage.create_edge(n0, n1, "KNOWS", PropertyMapBuilder::new().build()).unwrap();

    let entries: Vec<_> = storage.get_incoming_entries_iter(n1).collect();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].edge_id, e1);
    assert_eq!(entries[0].target, n0); // Source node
}

#[test]
fn test_entries_iter_len() {
    let storage = CurrentStorage::new();
    let n0 = storage.create_node("Source", PropertyMapBuilder::new().build()).unwrap();

    // Create 5 outgoing edges
    for _ in 0..5 {
        let target = storage.create_node("Target", PropertyMapBuilder::new().build()).unwrap();
        storage.create_edge(n0, target, "LINK", PropertyMapBuilder::new().build()).unwrap();
    }

    let iter = storage.get_outgoing_entries_iter(n0);
    assert_eq!(iter.len(), 5);

    // Verify ExactSizeIterator contract
    let count = iter.count();
    assert_eq!(count, 5);
}

#[test]
fn test_entries_iter_size_hint() {
    let storage = CurrentStorage::new();
    let n0 = storage.create_node("Source", PropertyMapBuilder::new().build()).unwrap();

    let iter = storage.get_outgoing_entries_iter(n0);
    let (lower, upper) = iter.size_hint();
    assert_eq!(lower, 0);
    assert!(upper.is_none());
}

#[test]
fn test_entries_iter_slow_path_uncompacted() {
    let storage = CurrentStorage::new();
    let n0 = storage.create_node("Source", PropertyMapBuilder::new().build()).unwrap();
    let target = storage.create_node("Target", PropertyMapBuilder::new().build()).unwrap();

    // Add edges to delta (no compaction)
    storage.create_edge(n0, target, "LINK", PropertyMapBuilder::new().build()).unwrap();

    // Verify iterators work correctly on uncompacted data
    let iter = storage.get_outgoing_entries_iter(n0);
    assert_eq!(iter.len(), 1);
    assert_eq!(iter.count(), 1);
}

#[test]
fn test_entries_iter_slow_path_tombstones() {
    let storage = CurrentStorage::new();
    let n0 = storage.create_node("Source", PropertyMapBuilder::new().build()).unwrap();
    let t1 = storage.create_node("Target1", PropertyMapBuilder::new().build()).unwrap();
    let t2 = storage.create_node("Target2", PropertyMapBuilder::new().build()).unwrap();

    let e1 = storage.create_edge(n0, t1, "LINK", PropertyMapBuilder::new().build()).unwrap();
    let e2 = storage.create_edge(n0, t2, "LINK", PropertyMapBuilder::new().build()).unwrap();

    storage.compact_adjacency(); // Move to frozen

    // Create tombstone
    storage.delete_edge(e1).unwrap();

    // Verify iterators filter tombstones correctly
    let iter = storage.get_outgoing_entries_iter(n0);
    assert_eq!(iter.len(), 1); // Should only see e2
    let entries: Vec<_> = iter.collect();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].edge_id, e2);
}

#[test]
fn test_entries_iter_mixed_frozen_delta_tombstones() {
    let storage = CurrentStorage::new();
    let n0 = storage.create_node("Source", PropertyMapBuilder::new().build()).unwrap();
    let t1 = storage.create_node("T1", PropertyMapBuilder::new().build()).unwrap();
    let t2 = storage.create_node("T2", PropertyMapBuilder::new().build()).unwrap();

    // 1. Frozen edge
    let e1 = storage.create_edge(n0, t1, "LINK", PropertyMapBuilder::new().build()).unwrap();
    storage.compact_adjacency();

    // 2. Delta edge
    let e2 = storage.create_edge(n0, t2, "LINK", PropertyMapBuilder::new().build()).unwrap();

    // 3. Tombstone on frozen edge
    storage.delete_edge(e1).unwrap();

    // Should see only e2 (from delta)
    let iter = storage.get_outgoing_entries_iter(n0);
    assert_eq!(iter.len(), 1);
    let entries: Vec<_> = iter.collect();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].edge_id, e2);
}
