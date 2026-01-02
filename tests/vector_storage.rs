//! Integration tests for vector storage functionality (Phase 1).
//!
//! These tests verify end-to-end vector property handling through the
//! GallifreyDB API, including storage, retrieval, versioning, and
//! temporal queries.

use gallifreydb::{GallifreyDB, PropertyMapBuilder, WriteOps};

// ============================================================
// Helper Functions
// ============================================================

/// Generate a test embedding of given dimension with predictable values.
fn generate_embedding(dim: usize, seed: f32) -> Vec<f32> {
    (0..dim).map(|i| (i as f32 + seed) / dim as f32).collect()
}

/// Sleep briefly to ensure timestamps differ.
fn advance_time() {
    std::thread::sleep(std::time::Duration::from_millis(10));
}

// ============================================================
// Node Vector Tests
// ============================================================

#[test]
fn test_create_node_with_vector_and_retrieve() {
    let db = GallifreyDB::new();

    // Create node with embedding
    let embedding = vec![0.1f32, 0.2, 0.3, 0.4, 0.5];
    let props = PropertyMapBuilder::new()
        .insert("title", "Test Document")
        .insert_vector("embedding", &embedding)
        .build();

    let node_id = db.create_node("Document", props).unwrap();

    // Retrieve and verify
    let node = db.get_node(node_id).unwrap();
    assert_eq!(
        node.get_property("title").and_then(|v| v.as_str()),
        Some("Test Document")
    );
    assert_eq!(
        node.get_property("embedding").and_then(|v| v.as_vector()),
        Some(&embedding[..])
    );
}

#[test]
fn test_update_vector_property_creates_version() {
    let db = GallifreyDB::new();

    // Create node with initial embedding
    let embedding_v1 = vec![0.1f32, 0.2, 0.3];
    let props = PropertyMapBuilder::new()
        .insert("name", "Document")
        .insert_vector("embedding", &embedding_v1)
        .build();

    let node_id = db.create_node("Document", props).unwrap();

    // Get initial stats
    let stats_before = db.historical_stats().unwrap();
    let versions_before = stats_before.total_node_versions;

    advance_time();

    // Update embedding using transaction
    let embedding_v2 = vec![0.9f32, 0.8, 0.7];
    {
        let mut tx = db.write_transaction().unwrap();
        let new_props = PropertyMapBuilder::new()
            .insert("name", "Document")
            .insert_vector("embedding", &embedding_v2)
            .build();
        tx.update_node(node_id, new_props).unwrap();
        tx.commit().unwrap();
    }

    // Current state should have new embedding
    let current_node = db.get_node(node_id).unwrap();
    assert_eq!(
        current_node
            .get_property("embedding")
            .and_then(|v| v.as_vector()),
        Some(&embedding_v2[..])
    );

    // Verify a new version was created
    let stats_after = db.historical_stats().unwrap();
    assert!(
        stats_after.total_node_versions > versions_before,
        "Update should create new version"
    );
}

#[test]
fn test_multiple_nodes_with_vectors_isolation() {
    let db = GallifreyDB::new();

    // Create multiple nodes with different embeddings
    let embedding_a = vec![1.0f32, 0.0, 0.0];
    let embedding_b = vec![0.0f32, 1.0, 0.0];
    let embedding_c = vec![0.0f32, 0.0, 1.0];

    let node_a = db
        .create_node(
            "Document",
            PropertyMapBuilder::new()
                .insert("name", "A")
                .insert_vector("embedding", &embedding_a)
                .build(),
        )
        .unwrap();

    let node_b = db
        .create_node(
            "Document",
            PropertyMapBuilder::new()
                .insert("name", "B")
                .insert_vector("embedding", &embedding_b)
                .build(),
        )
        .unwrap();

    let node_c = db
        .create_node(
            "Document",
            PropertyMapBuilder::new()
                .insert("name", "C")
                .insert_vector("embedding", &embedding_c)
                .build(),
        )
        .unwrap();

    // Verify each node has its own embedding (isolation)
    assert_eq!(
        db.get_node(node_a)
            .unwrap()
            .get_property("embedding")
            .and_then(|v| v.as_vector()),
        Some(&embedding_a[..])
    );
    assert_eq!(
        db.get_node(node_b)
            .unwrap()
            .get_property("embedding")
            .and_then(|v| v.as_vector()),
        Some(&embedding_b[..])
    );
    assert_eq!(
        db.get_node(node_c)
            .unwrap()
            .get_property("embedding")
            .and_then(|v| v.as_vector()),
        Some(&embedding_c[..])
    );

    // Update one node's embedding, verify others unchanged
    {
        let mut tx = db.write_transaction().unwrap();
        let new_embedding = vec![0.5f32, 0.5, 0.5];
        tx.update_node(
            node_a,
            PropertyMapBuilder::new()
                .insert("name", "A")
                .insert_vector("embedding", &new_embedding)
                .build(),
        )
        .unwrap();
        tx.commit().unwrap();
    }

    // Node A updated
    let new_a_embedding: [f32; 3] = [0.5f32, 0.5, 0.5];
    assert_eq!(
        db.get_node(node_a)
            .unwrap()
            .get_property("embedding")
            .and_then(|v| v.as_vector()),
        Some(new_a_embedding.as_slice())
    );

    // Nodes B and C unchanged
    assert_eq!(
        db.get_node(node_b)
            .unwrap()
            .get_property("embedding")
            .and_then(|v| v.as_vector()),
        Some(&embedding_b[..])
    );
    assert_eq!(
        db.get_node(node_c)
            .unwrap()
            .get_property("embedding")
            .and_then(|v| v.as_vector()),
        Some(&embedding_c[..])
    );
}

// ============================================================
// Edge Vector Tests
// ============================================================

#[test]
fn test_edge_with_vector_property() {
    let db = GallifreyDB::new();

    // Create two nodes
    let node_a = db
        .create_node(
            "Entity",
            PropertyMapBuilder::new().insert("name", "A").build(),
        )
        .unwrap();
    let node_b = db
        .create_node(
            "Entity",
            PropertyMapBuilder::new().insert("name", "B").build(),
        )
        .unwrap();

    // Create edge with relationship embedding
    let relationship_embedding = vec![0.8f32, 0.1, 0.1];
    let edge_id = db
        .create_edge(
            node_a,
            node_b,
            "SIMILAR_TO",
            PropertyMapBuilder::new()
                .insert("weight", 0.95f64)
                .insert_vector("embedding", &relationship_embedding)
                .build(),
        )
        .unwrap();

    // Retrieve and verify
    let edge = db.get_edge(edge_id).unwrap();
    assert_eq!(edge.source, node_a);
    assert_eq!(edge.target, node_b);
    assert_eq!(
        edge.get_property("weight").and_then(|v| v.as_float()),
        Some(0.95)
    );
    assert_eq!(
        edge.get_property("embedding").and_then(|v| v.as_vector()),
        Some(&relationship_embedding[..])
    );
}

#[test]
fn test_update_edge_vector_property() {
    let db = GallifreyDB::new();

    let node_a = db
        .create_node("Entity", PropertyMapBuilder::new().build())
        .unwrap();
    let node_b = db
        .create_node("Entity", PropertyMapBuilder::new().build())
        .unwrap();

    // Create edge with initial embedding
    let embedding_v1 = vec![0.5f32, 0.5];
    let edge_id = db
        .create_edge(
            node_a,
            node_b,
            "RELATES_TO",
            PropertyMapBuilder::new()
                .insert_vector("embedding", &embedding_v1)
                .build(),
        )
        .unwrap();

    // Get initial stats
    let stats_before = db.historical_stats().unwrap();
    let versions_before = stats_before.total_edge_versions;

    advance_time();

    // Update edge embedding
    let embedding_v2 = vec![0.9f32, 0.1];
    {
        let mut tx = db.write_transaction().unwrap();
        tx.update_edge(
            edge_id,
            PropertyMapBuilder::new()
                .insert_vector("embedding", &embedding_v2)
                .build(),
        )
        .unwrap();
        tx.commit().unwrap();
    }

    // Current state should have new embedding
    let current_edge = db.get_edge(edge_id).unwrap();
    assert_eq!(
        current_edge
            .get_property("embedding")
            .and_then(|v| v.as_vector()),
        Some(&embedding_v2[..])
    );

    // Verify a new version was created
    let stats_after = db.historical_stats().unwrap();
    assert!(
        stats_after.total_edge_versions > versions_before,
        "Update should create new edge version"
    );
}

// ============================================================
// Large Vector Tests
// ============================================================

#[test]
fn test_large_vector_1000_dimensions() {
    let db = GallifreyDB::new();

    const DIMENSIONS: usize = 1000;
    let large_embedding = generate_embedding(DIMENSIONS, 0.0);

    let node_id = db
        .create_node(
            "HighDimDoc",
            PropertyMapBuilder::new()
                .insert_vector("embedding", &large_embedding)
                .build(),
        )
        .unwrap();

    let node = db.get_node(node_id).unwrap();
    let retrieved = node
        .get_property("embedding")
        .and_then(|v| v.as_vector())
        .expect("Should have embedding");

    assert_eq!(retrieved.len(), DIMENSIONS);
    assert_eq!(retrieved, &large_embedding[..]);
}

#[test]
fn test_very_large_vector_4096_dimensions() {
    let db = GallifreyDB::new();

    // 4096 dimensions (larger than typical embedding models)
    const DIMENSIONS: usize = 4096;
    let embedding = generate_embedding(DIMENSIONS, 1.0);

    let node_id = db
        .create_node(
            "VeryHighDimDoc",
            PropertyMapBuilder::new()
                .insert_vector("embedding", &embedding)
                .build(),
        )
        .unwrap();

    let node = db.get_node(node_id).unwrap();
    let retrieved = node
        .get_property("embedding")
        .and_then(|v| v.as_vector())
        .expect("Should have embedding");

    assert_eq!(retrieved.len(), DIMENSIONS);
    assert_eq!(retrieved, &embedding[..]);
}

// ============================================================
// Common Embedding Dimension Tests
// ============================================================

#[test]
fn test_common_embedding_dimensions_384() {
    // MiniLM / all-MiniLM-L6-v2 (384 dimensions)
    let db = GallifreyDB::new();

    const DIMENSIONS: usize = 384;
    let embedding = generate_embedding(DIMENSIONS, 0.0);

    let node_id = db
        .create_node(
            "MiniLMDoc",
            PropertyMapBuilder::new()
                .insert("model", "all-MiniLM-L6-v2")
                .insert_vector("embedding", &embedding)
                .build(),
        )
        .unwrap();

    let node = db.get_node(node_id).unwrap();
    let retrieved = node
        .get_property("embedding")
        .and_then(|v| v.as_vector())
        .expect("Should have embedding");

    assert_eq!(retrieved.len(), DIMENSIONS);
    assert_eq!(
        node.get_property("model").and_then(|v| v.as_str()),
        Some("all-MiniLM-L6-v2")
    );
}

#[test]
fn test_common_embedding_dimensions_768() {
    // BERT / all-mpnet-base-v2 (768 dimensions)
    let db = GallifreyDB::new();

    const DIMENSIONS: usize = 768;
    let embedding = generate_embedding(DIMENSIONS, 0.0);

    let node_id = db
        .create_node(
            "BertDoc",
            PropertyMapBuilder::new()
                .insert("model", "all-mpnet-base-v2")
                .insert_vector("embedding", &embedding)
                .build(),
        )
        .unwrap();

    let node = db.get_node(node_id).unwrap();
    let retrieved = node
        .get_property("embedding")
        .and_then(|v| v.as_vector())
        .expect("Should have embedding");

    assert_eq!(retrieved.len(), DIMENSIONS);
}

#[test]
fn test_common_embedding_dimensions_1536() {
    // OpenAI text-embedding-ada-002 (1536 dimensions)
    let db = GallifreyDB::new();

    const DIMENSIONS: usize = 1536;
    let embedding = generate_embedding(DIMENSIONS, 0.0);

    let node_id = db
        .create_node(
            "OpenAIDoc",
            PropertyMapBuilder::new()
                .insert("model", "text-embedding-ada-002")
                .insert_vector("embedding", &embedding)
                .build(),
        )
        .unwrap();

    let node = db.get_node(node_id).unwrap();
    let retrieved = node
        .get_property("embedding")
        .and_then(|v| v.as_vector())
        .expect("Should have embedding");

    assert_eq!(retrieved.len(), DIMENSIONS);
}

#[test]
fn test_common_embedding_dimensions_3072() {
    // OpenAI text-embedding-3-large (3072 dimensions)
    let db = GallifreyDB::new();

    const DIMENSIONS: usize = 3072;
    let embedding = generate_embedding(DIMENSIONS, 0.0);

    let node_id = db
        .create_node(
            "OpenAI3LargeDoc",
            PropertyMapBuilder::new()
                .insert("model", "text-embedding-3-large")
                .insert_vector("embedding", &embedding)
                .build(),
        )
        .unwrap();

    let node = db.get_node(node_id).unwrap();
    let retrieved = node
        .get_property("embedding")
        .and_then(|v| v.as_vector())
        .expect("Should have embedding");

    assert_eq!(retrieved.len(), DIMENSIONS);
}

// ============================================================
// Version History Tests
// ============================================================

#[test]
fn test_multiple_vector_updates_version_chain() {
    let db = GallifreyDB::new();

    // Create node with initial embedding
    let embedding_v1 = vec![0.1f32, 0.2, 0.3];
    let node_id = db
        .create_node(
            "Document",
            PropertyMapBuilder::new()
                .insert_vector("embedding", &embedding_v1)
                .build(),
        )
        .unwrap();

    let stats_initial = db.historical_stats().unwrap();

    advance_time();

    // Update 1
    let embedding_v2 = vec![0.4f32, 0.5, 0.6];
    {
        let mut tx = db.write_transaction().unwrap();
        tx.update_node(
            node_id,
            PropertyMapBuilder::new()
                .insert_vector("embedding", &embedding_v2)
                .build(),
        )
        .unwrap();
        tx.commit().unwrap();
    }

    advance_time();

    // Update 2
    let embedding_v3 = vec![0.7f32, 0.8, 0.9];
    {
        let mut tx = db.write_transaction().unwrap();
        tx.update_node(
            node_id,
            PropertyMapBuilder::new()
                .insert_vector("embedding", &embedding_v3)
                .build(),
        )
        .unwrap();
        tx.commit().unwrap();
    }

    // Current should be v3
    assert_eq!(
        db.get_node(node_id)
            .unwrap()
            .get_property("embedding")
            .and_then(|v| v.as_vector()),
        Some(&embedding_v3[..])
    );

    // Verify version chain was created (3 versions total)
    let stats_final = db.historical_stats().unwrap();
    assert_eq!(
        stats_final.total_node_versions,
        stats_initial.total_node_versions + 2,
        "Should have 2 additional versions after 2 updates"
    );
    assert_eq!(stats_final.unique_nodes, 1);
}

#[test]
fn test_historical_stats_with_vectors() {
    let db = GallifreyDB::new();

    // Create node and update it multiple times
    let node_id = db
        .create_node(
            "Document",
            PropertyMapBuilder::new()
                .insert_vector("embedding", &[0.1f32, 0.2])
                .build(),
        )
        .unwrap();

    for i in 1..5 {
        let mut tx = db.write_transaction().unwrap();
        tx.update_node(
            node_id,
            PropertyMapBuilder::new()
                .insert_vector("embedding", &[i as f32 * 0.1, i as f32 * 0.2])
                .build(),
        )
        .unwrap();
        tx.commit().unwrap();
    }

    // Check historical stats
    let stats = db.historical_stats().unwrap();
    assert!(stats.total_node_versions > 0);
    assert_eq!(stats.unique_nodes, 1);

    // Should have mix of anchors and deltas
    let total = stats.node_anchor_count + stats.node_delta_count;
    assert!(total > 0);
}

// ============================================================
// Edge Case Tests
// ============================================================

#[test]
fn test_empty_vector() {
    let db = GallifreyDB::new();

    let empty_vec: Vec<f32> = vec![];
    let node_id = db
        .create_node(
            "EmptyVecNode",
            PropertyMapBuilder::new()
                .insert_vector("embedding", &empty_vec)
                .build(),
        )
        .unwrap();

    let node = db.get_node(node_id).unwrap();
    assert_eq!(
        node.get_property("embedding").and_then(|v| v.as_vector()),
        Some(&empty_vec[..])
    );
}

#[test]
fn test_node_with_multiple_embeddings() {
    let db = GallifreyDB::new();

    // Node with multiple embedding fields (e.g., from different models)
    let text_embedding = vec![0.1f32, 0.2, 0.3, 0.4];
    let image_embedding = vec![0.5f32, 0.6, 0.7, 0.8];
    let combined_embedding = vec![0.9f32, 0.0, 0.1, 0.2];

    let node_id = db
        .create_node(
            "MultimodalDoc",
            PropertyMapBuilder::new()
                .insert("content", "A picture of a cat")
                .insert_vector("text_embedding", &text_embedding)
                .insert_vector("image_embedding", &image_embedding)
                .insert_vector("combined_embedding", &combined_embedding)
                .build(),
        )
        .unwrap();

    let node = db.get_node(node_id).unwrap();

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
    assert_eq!(
        node.get_property("combined_embedding")
            .and_then(|v| v.as_vector()),
        Some(&combined_embedding[..])
    );
}

#[test]
fn test_graph_with_mixed_properties_and_vectors() {
    let db = GallifreyDB::new();

    // Create a small knowledge graph with embeddings
    let alice = db
        .create_node(
            "Person",
            PropertyMapBuilder::new()
                .insert("name", "Alice")
                .insert("age", 30i64)
                .insert_vector("profile_embedding", &[0.1f32, 0.2, 0.3])
                .build(),
        )
        .unwrap();

    let bob = db
        .create_node(
            "Person",
            PropertyMapBuilder::new()
                .insert("name", "Bob")
                .insert("age", 25i64)
                .insert_vector("profile_embedding", &[0.4f32, 0.5, 0.6])
                .build(),
        )
        .unwrap();

    let _knows = db
        .create_edge(
            alice,
            bob,
            "KNOWS",
            PropertyMapBuilder::new()
                .insert("since", 2020i64)
                .insert_vector("relationship_embedding", &[0.7f32, 0.8, 0.9])
                .build(),
        )
        .unwrap();

    // Verify graph structure
    assert_eq!(db.node_count(), 2);
    assert_eq!(db.edge_count(), 1);
    assert_eq!(db.out_degree(alice), 1);
    assert_eq!(db.in_degree(bob), 1);

    // Verify embeddings
    let alice_node = db.get_node(alice).unwrap();
    assert_eq!(
        alice_node
            .get_property("profile_embedding")
            .and_then(|v| v.as_vector()),
        Some(&[0.1f32, 0.2, 0.3][..])
    );
}
