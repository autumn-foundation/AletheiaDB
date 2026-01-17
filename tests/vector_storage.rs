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

/// Sleep briefly to ensure timestamps differ between operations.
///
/// GallifreyDB uses `time::now()` for transaction timestamps. Operations
/// executed within the same millisecond may receive identical timestamps,
/// which can affect version ordering. This helper ensures sufficient time
/// passes between operations for distinct timestamps.
///
/// Note: 10ms is chosen as a balance between test speed and reliability.
/// On heavily loaded CI systems, consider increasing if tests become flaky.
fn advance_time() {
    std::thread::sleep(std::time::Duration::from_millis(10));
}

// ============================================================
// Node Vector Tests
// ============================================================

/// Test creating a node with a vector property and retrieving it.
///
/// Note on floating-point comparison: We use exact equality (`==`) because these tests
/// verify storage/retrieval without arithmetic operations. The values are bit-identical
/// copies, not computed results. For tests involving vector math (e.g., cosine similarity),
/// use approximate equality with an epsilon tolerance.
#[test]
fn test_create_node_with_vector_and_retrieve() {
    let db = GallifreyDB::new().unwrap();

    // Create node with embedding
    let embedding = vec![0.1f32, 0.2, 0.3, 0.4, 0.5];
    let props = PropertyMapBuilder::new()
        .insert("title", "Test Document")
        .insert_vector("embedding", &embedding)
        .build();

    let node_id = db.create_node("Document", props).unwrap();

    // Retrieve and verify - exact equality is safe here (no arithmetic, just storage/retrieval)
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
    let db = GallifreyDB::new().unwrap();

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
    let db = GallifreyDB::new().unwrap();

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
    let new_embedding = vec![0.5f32, 0.5, 0.5];
    {
        let mut tx = db.write_transaction().unwrap();
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
    assert_eq!(
        db.get_node(node_a)
            .unwrap()
            .get_property("embedding")
            .and_then(|v| v.as_vector()),
        Some(&new_embedding[..])
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
    let db = GallifreyDB::new().unwrap();

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
    let db = GallifreyDB::new().unwrap();

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
    let db = GallifreyDB::new().unwrap();

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
    let db = GallifreyDB::new().unwrap();

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

/// Macro to generate tests for common embedding dimensions.
/// This reduces duplication while maintaining clear test names.
macro_rules! test_embedding_dimension {
    ($test_name:ident, $dim:expr, $model:expr, $label:expr) => {
        #[test]
        fn $test_name() {
            let db = GallifreyDB::new().unwrap();
            const DIMENSIONS: usize = $dim;
            let embedding = generate_embedding(DIMENSIONS, 0.0);

            let node_id = db
                .create_node(
                    $label,
                    PropertyMapBuilder::new()
                        .insert("model", $model)
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
                Some($model)
            );
        }
    };
}

// MiniLM / all-MiniLM-L6-v2 (384 dimensions)
test_embedding_dimension!(
    test_common_embedding_dimensions_384,
    384,
    "all-MiniLM-L6-v2",
    "MiniLMDoc"
);

// BERT / all-mpnet-base-v2 (768 dimensions)
test_embedding_dimension!(
    test_common_embedding_dimensions_768,
    768,
    "all-mpnet-base-v2",
    "BertDoc"
);

// OpenAI text-embedding-ada-002 (1536 dimensions)
test_embedding_dimension!(
    test_common_embedding_dimensions_1536,
    1536,
    "text-embedding-ada-002",
    "OpenAIDoc"
);

// OpenAI text-embedding-3-large (3072 dimensions)
test_embedding_dimension!(
    test_common_embedding_dimensions_3072,
    3072,
    "text-embedding-3-large",
    "OpenAI3LargeDoc"
);

// ============================================================
// Version History Tests
// ============================================================

#[test]
fn test_multiple_vector_updates_version_chain() {
    let db = GallifreyDB::new().unwrap();

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
    let db = GallifreyDB::new().unwrap();

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
    // 1 create + 4 updates = 5 total versions
    let stats = db.historical_stats().unwrap();
    assert_eq!(
        stats.total_node_versions, 5,
        "Expected 1 create + 4 updates = 5 versions"
    );
    assert_eq!(stats.unique_nodes, 1);

    // Anchors + deltas should equal total versions
    let total = stats.node_anchor_count + stats.node_delta_count;
    assert_eq!(total, 5, "Anchor + delta count should equal total versions");
    // Should have at least one anchor (the first version)
    assert!(
        stats.node_anchor_count > 0,
        "Should have at least one anchor version"
    );
}

// ============================================================
// Edge Case Tests
// ============================================================

#[test]
fn test_empty_vector() {
    let db = GallifreyDB::new().unwrap();

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
    let db = GallifreyDB::new().unwrap();

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
    let db = GallifreyDB::new().unwrap();

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

// ============================================================
// Phase 2: HNSW Vector Index Integration Tests
// ============================================================

/// Helper function to create a database with vector index enabled.
fn setup_indexed_db(dimensions: usize) -> GallifreyDB {
    use gallifreydb::index::vector::{DistanceMetric, HnswConfig};

    let db = GallifreyDB::new().unwrap();
    let config = HnswConfig::new(dimensions, DistanceMetric::Cosine).with_capacity(100);
    db.enable_vector_index("embedding", config)
        .expect("Failed to enable vector index");
    db
}

// ============================================================
// Index Lifecycle Tests
// ============================================================

#[test]
fn test_enable_vector_index() {
    use gallifreydb::index::vector::{DistanceMetric, HnswConfig};

    let db = GallifreyDB::new().unwrap();

    // Index should not be enabled initially
    assert!(!db.is_vector_index_enabled());

    // Enable index
    let config = HnswConfig::new(384, DistanceMetric::Cosine);
    db.enable_vector_index("embedding", config).unwrap();

    // Index should now be enabled
    assert!(db.is_vector_index_enabled());
}

#[test]
fn test_double_enable_vector_index_fails() {
    use gallifreydb::index::vector::{DistanceMetric, HnswConfig};

    let db = GallifreyDB::new().unwrap();

    // Enable index once
    let config = HnswConfig::new(384, DistanceMetric::Cosine);
    db.enable_vector_index("embedding", config.clone()).unwrap();

    // Attempt to enable again should fail
    let result = db.enable_vector_index("embedding", config);
    assert!(result.is_err());
}

// ============================================================
// Search Tests
// ============================================================

#[test]
fn test_find_similar_by_node_id() {
    let db = setup_indexed_db(384);

    // Create nodes with embeddings
    let emb1 = generate_embedding(384, 1.0);
    let emb2 = generate_embedding(384, 1.1); // Very similar to emb1
    let emb3 = generate_embedding(384, 10.0); // Very different

    let node1 = db
        .create_node(
            "Document",
            PropertyMapBuilder::new()
                .insert("title", "Doc 1")
                .insert_vector("embedding", &emb1)
                .build(),
        )
        .unwrap();

    let node2 = db
        .create_node(
            "Document",
            PropertyMapBuilder::new()
                .insert("title", "Doc 2")
                .insert_vector("embedding", &emb2)
                .build(),
        )
        .unwrap();

    let node3 = db
        .create_node(
            "Document",
            PropertyMapBuilder::new()
                .insert("title", "Doc 3")
                .insert_vector("embedding", &emb3)
                .build(),
        )
        .unwrap();

    // Search for similar nodes to node1
    let results = db.find_similar(node1, 2).unwrap();

    // Should return node2 (most similar) and node3
    assert_eq!(results.len(), 2);
    // First result should be node2 (more similar)
    assert_eq!(results[0].0, node2);
    // Second result should be node3 (less similar)
    assert_eq!(results[1].0, node3);
}

#[test]
fn test_find_similar_by_embedding() {
    let db = setup_indexed_db(384);

    // Create nodes
    let emb1 = generate_embedding(384, 1.0);
    let emb2 = generate_embedding(384, 1.1);

    db.create_node(
        "Document",
        PropertyMapBuilder::new()
            .insert_vector("embedding", &emb1)
            .build(),
    )
    .unwrap();

    db.create_node(
        "Document",
        PropertyMapBuilder::new()
            .insert_vector("embedding", &emb2)
            .build(),
    )
    .unwrap();

    // Search with a query embedding
    let query = generate_embedding(384, 1.05);
    let results = db.find_similar_by_embedding(&query, 2).unwrap();

    // Should return 2 results
    assert_eq!(results.len(), 2);
    // Results should have similarity scores
    assert!(results[0].1 > 0.0);
    assert!(results[1].1 > 0.0);
}

#[test]
fn test_find_similar_with_label_filter() {
    let db = setup_indexed_db(128);

    let emb = generate_embedding(128, 1.0);

    // Create nodes with different labels
    let doc1 = db
        .create_node(
            "Document",
            PropertyMapBuilder::new()
                .insert_vector("embedding", &emb)
                .build(),
        )
        .unwrap();

    let doc2 = db
        .create_node(
            "Document",
            PropertyMapBuilder::new()
                .insert_vector("embedding", &emb)
                .build(),
        )
        .unwrap();

    let _image = db
        .create_node(
            "Image",
            PropertyMapBuilder::new()
                .insert_vector("embedding", &emb)
                .build(),
        )
        .unwrap();

    // Search with label filter - should only return Documents
    let results = db.find_similar_with_label(doc1, "Document", 10).unwrap();

    // Should only return doc2 (not the Image node)
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].0, doc2);
}

// ============================================================
// Update Semantics Tests
// ============================================================

#[test]
fn test_update_node_updates_index() {
    let db = setup_indexed_db(128);

    let emb1 = generate_embedding(128, 1.0);
    let emb2 = generate_embedding(128, 10.0); // Very different

    // Create node with initial embedding
    let node_id = db
        .create_node(
            "Document",
            PropertyMapBuilder::new()
                .insert_vector("embedding", &emb1)
                .build(),
        )
        .unwrap();

    // Update the embedding
    let mut tx = db.write_transaction().unwrap();
    tx.update_node(
        node_id,
        PropertyMapBuilder::new()
            .insert_vector("embedding", &emb2)
            .build(),
    )
    .unwrap();
    tx.commit().unwrap();

    // Search should reflect the updated embedding
    let results = db.find_similar_by_embedding(&emb2, 1).unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].0, node_id);
}

// Note: test_delete_node_removes_from_index skipped - delete_node not yet implemented
// See issue #367: Add test when delete_node is implemented in GallifreyDB

#[test]
fn test_node_without_vector_property_not_indexed() {
    let db = setup_indexed_db(128);

    // Create node without embedding property
    let _node_no_emb = db
        .create_node(
            "Document",
            PropertyMapBuilder::new()
                .insert("title", "No Embedding")
                .build(),
        )
        .unwrap();

    // Create node with embedding
    let emb = generate_embedding(128, 1.0);
    let node_with_emb = db
        .create_node(
            "Document",
            PropertyMapBuilder::new()
                .insert_vector("embedding", &emb)
                .build(),
        )
        .unwrap();

    // Search should only return the node with embedding
    let results = db.find_similar_by_embedding(&emb, 10).unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].0, node_with_emb);
}

// ============================================================
// Error Handling Tests
// ============================================================

#[test]
fn test_find_similar_on_non_indexed_db_fails() {
    use gallifreydb::core::id::NodeId;

    let db = GallifreyDB::new().unwrap(); // No index enabled

    let node_id = NodeId::new(1).unwrap();
    let result = db.find_similar(node_id, 10);

    // Should return an error
    assert!(result.is_err());
}

#[test]
fn test_find_similar_with_invalid_node_id_fails() {
    use gallifreydb::core::id::NodeId;

    let db = setup_indexed_db(128);

    // Non-existent node ID
    let fake_id = NodeId::new(99999).unwrap();
    let result = db.find_similar(fake_id, 10);

    // Should return an error
    assert!(result.is_err());
}

#[test]
fn test_dimension_mismatch_in_indexed_property() {
    let db = setup_indexed_db(128);

    // Create node with correct dimensions
    let emb128 = generate_embedding(128, 1.0);
    db.create_node(
        "Document",
        PropertyMapBuilder::new()
            .insert_vector("embedding", &emb128)
            .build(),
    )
    .unwrap();

    // Attempt to create node with wrong dimensions
    let emb256 = generate_embedding(256, 1.0);
    let result = db.create_node(
        "Document",
        PropertyMapBuilder::new()
            .insert_vector("embedding", &emb256)
            .build(),
    );

    // Should fail due to dimension mismatch
    assert!(result.is_err());
}

// ============================================================
// Concurrent Operations Test
// ============================================================

#[test]
fn test_concurrent_index_operations() {
    use std::sync::Arc;
    use std::thread;

    let db = Arc::new(setup_indexed_db(64));
    let num_threads = 4;
    let nodes_per_thread = 10;

    // Spawn threads to concurrently add nodes
    let handles: Vec<_> = (0..num_threads)
        .map(|thread_id| {
            let db_clone = Arc::clone(&db);
            thread::spawn(move || {
                for i in 0..nodes_per_thread {
                    let emb = generate_embedding(64, (thread_id * 100 + i) as f32);
                    db_clone
                        .create_node(
                            "Document",
                            PropertyMapBuilder::new()
                                .insert_vector("embedding", &emb)
                                .build(),
                        )
                        .unwrap();
                }
            })
        })
        .collect();

    // Wait for all threads
    for handle in handles {
        handle.join().unwrap();
    }

    // Verify all nodes were indexed
    let query = generate_embedding(64, 0.0);
    let results = db.find_similar_by_embedding(&query, 100).unwrap();

    // Should have all nodes indexed
    assert_eq!(results.len(), (num_threads * nodes_per_thread) as usize);
}
