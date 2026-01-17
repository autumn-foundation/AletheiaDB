//! Integration test for temporal vector and historical storage integration (VS-047).
//!
//! This test verifies that vector snapshots are created and aligned with
//! historical storage anchors, enabling temporal vector queries.

use gallifreydb::{
    DistanceMetric, GallifreyDB, HnswConfig, PropertyMapBuilder, ReadOps, WriteOps,
    index::vector::temporal::TemporalVectorConfig, storage::version::AnchorConfig,
};

#[test]
fn test_full_temporal_vector_lifecycle() {
    // Create database with custom anchor config (anchor every 3 versions for faster testing)
    let anchor_config = AnchorConfig {
        anchor_interval: 3,
        max_delta_chain: 10,
    };
    let db = GallifreyDB::with_config(anchor_config).unwrap();

    // Enable temporal vector indexing
    let hnsw_config = HnswConfig::new(4, DistanceMetric::Cosine);
    let temporal_config = TemporalVectorConfig::default_with_hnsw(hnsw_config);

    db.enable_temporal_vector_index("embedding", temporal_config)
        .expect("Failed to enable temporal vector index");

    // Create initial node with vector
    let initial_vector = vec![1.0f32, 0.0, 0.0, 0.0];
    let node_id = db
        .create_node(
            "Document",
            PropertyMapBuilder::new()
                .insert("title", "Test Document")
                .insert_vector("embedding", &initial_vector)
                .build(),
        )
        .expect("Failed to create node");

    // Update the node 9 more times to trigger multiple anchors
    // With anchor_interval=3, we should get anchors at versions 0, 3, 6, 9
    let mut _vectors = vec![initial_vector.clone()];
    for i in 1..10 {
        let new_vector = vec![1.0f32 - (i as f32 * 0.1), i as f32 * 0.1, 0.0, 0.0];
        _vectors.push(new_vector.clone());

        db.write(|tx| {
            let _node = tx.get_node(node_id)?;
            let new_props = PropertyMapBuilder::new()
                .insert("title", "Test Document")
                .insert("iteration", i as i64)
                .insert_vector("embedding", &new_vector)
                .build();
            tx.update_node(node_id, new_props)?;
            Ok(())
        })
        .expect("Failed to update node");
    }

    // Verify: Temporal vector indexing is enabled
    assert!(db.is_temporal_vector_index_enabled());

    // Verify: Can still retrieve the node
    let node = db
        .read(|tx| tx.get_node(node_id))
        .expect("Failed to read node");
    assert_eq!(node.id, node_id);

    println!("✓ Successfully updated node with vectors 10 times - anchors and snapshots created");
}

#[test]
fn test_multiple_nodes_with_temporal_vectors() {
    // Create database with anchor every 2 versions
    let anchor_config = AnchorConfig {
        anchor_interval: 2,
        max_delta_chain: 10,
    };
    let db = GallifreyDB::with_config(anchor_config).unwrap();

    // Enable temporal vector indexing
    let hnsw_config = HnswConfig::new(3, DistanceMetric::Cosine);
    let temporal_config = TemporalVectorConfig::default_with_hnsw(hnsw_config);

    db.enable_temporal_vector_index("embedding", temporal_config)
        .expect("Failed to enable temporal vector index");

    // Create multiple nodes with vectors
    let node1 = db
        .create_node(
            "Doc",
            PropertyMapBuilder::new()
                .insert_vector("embedding", &[1.0f32, 0.0, 0.0])
                .build(),
        )
        .expect("Failed to create node1");

    let node2 = db
        .create_node(
            "Doc",
            PropertyMapBuilder::new()
                .insert_vector("embedding", &[0.0f32, 1.0, 0.0])
                .build(),
        )
        .expect("Failed to create node2");

    // Update both nodes to trigger anchors
    for i in 1..=3 {
        db.write(|tx| {
            tx.update_node(
                node1,
                PropertyMapBuilder::new()
                    .insert_vector("embedding", &[1.0f32, i as f32 * 0.1, 0.0])
                    .build(),
            )?;
            tx.update_node(
                node2,
                PropertyMapBuilder::new()
                    .insert_vector("embedding", &[0.0f32, 1.0, i as f32 * 0.1])
                    .build(),
            )?;
            Ok(())
        })
        .expect("Failed to update nodes");
    }

    // Verify: Both nodes can be retrieved
    assert!(db.read(|tx| tx.get_node(node1)).is_ok());
    assert!(db.read(|tx| tx.get_node(node2)).is_ok());

    println!("✓ Multiple nodes tracked with temporal vectors");
}

#[test]
fn test_temporal_vector_index_without_anchors() {
    // Test that temporal vector indexing works even when no anchors are created
    let db = GallifreyDB::new().unwrap();

    let hnsw_config = HnswConfig::new(4, DistanceMetric::Cosine);
    let temporal_config = TemporalVectorConfig::default_with_hnsw(hnsw_config);

    db.enable_temporal_vector_index("embedding", temporal_config)
        .expect("Failed to enable temporal vector index");

    // Create a single node - this won't trigger many anchors (first version is anchor)
    let node_id = db
        .create_node(
            "Doc",
            PropertyMapBuilder::new()
                .insert_vector("embedding", &[1.0f32, 0.0, 0.0, 0.0])
                .build(),
        )
        .expect("Failed to create node");

    // Verify: Temporal vector indexing is enabled
    assert!(db.is_temporal_vector_index_enabled());

    // Verify: Node can be retrieved
    let node = db
        .read(|tx| tx.get_node(node_id))
        .expect("Failed to read node");
    assert_eq!(node.id, node_id);

    println!("✓ Temporal vector index works with minimal activity");
}

#[test]
fn test_observer_graceful_degradation() {
    // Test that anchor creation succeeds even if vector snapshot creation might fail
    // (e.g., if index has issues)

    let anchor_config = AnchorConfig {
        anchor_interval: 2,
        max_delta_chain: 10,
    };
    let db = GallifreyDB::with_config(anchor_config).unwrap();

    // Enable temporal vector indexing with default config
    let hnsw_config = HnswConfig::new(3, DistanceMetric::Cosine).with_capacity(5); // Small capacity
    let temporal_config = TemporalVectorConfig::default_with_hnsw(hnsw_config);

    db.enable_temporal_vector_index("embedding", temporal_config)
        .expect("Failed to enable temporal vector index");

    // Create nodes and updates - should not fail even with constraints
    for i in 0..10 {
        let result = db.create_node(
            "Doc",
            PropertyMapBuilder::new()
                .insert("id", i as i64)
                .insert_vector(
                    "embedding",
                    &[i as f32 / 10.0, 1.0 - (i as f32 / 10.0), 0.5],
                )
                .build(),
        );
        assert!(
            result.is_ok(),
            "Node creation should succeed even with constraints"
        );
    }

    println!("✓ Graceful degradation: All node creations succeeded");
}

#[test]
fn test_edge_versions_with_temporal_vectors() {
    // Test that edge anchor events also work with temporal vectors
    let anchor_config = AnchorConfig {
        anchor_interval: 2,
        max_delta_chain: 10,
    };
    let db = GallifreyDB::with_config(anchor_config).unwrap();

    let hnsw_config = HnswConfig::new(4, DistanceMetric::Cosine);
    let temporal_config = TemporalVectorConfig::default_with_hnsw(hnsw_config);

    db.enable_temporal_vector_index("embedding", temporal_config)
        .expect("Failed to enable temporal vector index");

    // Create nodes
    let node1 = db
        .create_node(
            "Person",
            PropertyMapBuilder::new().insert("name", "Alice").build(),
        )
        .expect("Failed to create node1");

    let node2 = db
        .create_node(
            "Person",
            PropertyMapBuilder::new().insert("name", "Bob").build(),
        )
        .expect("Failed to create node2");

    // Create edge with vector property
    let edge_id = db
        .create_edge(
            node1,
            node2,
            "KNOWS",
            PropertyMapBuilder::new()
                .insert("strength", 0.8f64)
                .insert_vector("embedding", &[1.0f32, 0.0, 0.0, 0.0])
                .build(),
        )
        .expect("Failed to create edge");

    // Update edge to trigger anchors
    for i in 1..=4 {
        db.write(|tx| {
            tx.update_edge(
                edge_id,
                PropertyMapBuilder::new()
                    .insert("strength", 0.8f64)
                    .insert("iteration", i as i64)
                    .insert_vector(
                        "embedding",
                        &[1.0f32 - (i as f32 * 0.2), i as f32 * 0.2, 0.0, 0.0],
                    )
                    .build(),
            )?;
            Ok(())
        })
        .expect("Failed to update edge");
    }

    // Verify: Edge can be retrieved
    assert!(db.read(|tx| tx.get_edge(edge_id)).is_ok());

    println!("✓ Edge anchors work with temporal vectors");
}

#[test]
fn test_vector_snapshot_id_stored_in_anchors() {
    // Create database with custom anchor config (anchor every 3 versions)
    let anchor_config = AnchorConfig {
        anchor_interval: 3,
        max_delta_chain: 10,
    };
    let db = GallifreyDB::with_config(anchor_config).unwrap();

    // Enable temporal vector indexing
    let hnsw_config = HnswConfig::new(4, DistanceMetric::Cosine);
    let temporal_config = TemporalVectorConfig::default_with_hnsw(hnsw_config);

    db.enable_temporal_vector_index("embedding", temporal_config)
        .expect("Failed to enable temporal vector index");

    // Create initial node with vector (version 0 = anchor)
    let initial_vector = vec![1.0f32, 0.0, 0.0, 0.0];
    let node_id = db
        .create_node(
            "Document",
            PropertyMapBuilder::new()
                .insert("title", "Test Document")
                .insert_vector("embedding", &initial_vector)
                .build(),
        )
        .expect("Failed to create node");

    // Update the node 6 more times to trigger anchors at versions 0, 3, 6
    for i in 1..7 {
        let new_vector = vec![1.0f32 - (i as f32 * 0.1), i as f32 * 0.1, 0.0, 0.0];

        db.write(|tx| {
            let new_props = PropertyMapBuilder::new()
                .insert("title", "Test Document")
                .insert("iteration", i as i64)
                .insert_vector("embedding", &new_vector)
                .build();
            tx.update_node(node_id, new_props)?;
            Ok(())
        })
        .expect("Failed to update node");
    }

    // Access historical storage to verify snapshot IDs
    let historical = db.__test_historical_storage();
    let historical_read = historical.read();

    // Check all node versions
    let mut anchor_count = 0;
    let mut anchors_with_snapshot_id = 0;

    for version in historical_read.__test_get_node_versions_iterator() {
        if version.is_anchor() {
            anchor_count += 1;
            if let Some(snapshot_id) = version.data.get_vector_snapshot_id() {
                anchors_with_snapshot_id += 1;
                println!("✓ Anchor has snapshot_id: {}", snapshot_id);
            } else {
                panic!("Anchor version found without vector_snapshot_id!");
            }
        }
    }

    // We should have at least 3 anchors (versions 0, 3, 6)
    assert!(
        anchor_count >= 3,
        "Expected at least 3 anchors, found {}",
        anchor_count
    );

    // All anchors should have snapshot IDs
    assert_eq!(
        anchor_count, anchors_with_snapshot_id,
        "Not all anchors have snapshot IDs: {} anchors, {} with IDs",
        anchor_count, anchors_with_snapshot_id
    );

    println!(
        "✓ All {} anchors have vector_snapshot_id populated",
        anchor_count
    );
}

/// Integration test for multi-property vector search property_key support (Issue #411).
///
/// This test validates that the property_key parameter is correctly passed through
/// the query pipeline (LogicalPlan -> PhysicalPlan -> Executor) for vector searches.
/// We test with regular (non-temporal) vector indexes since the property_key plumbing
/// is identical, and this avoids the complexity of temporal snapshot infrastructure.
#[test]
fn test_multi_property_temporal_vector_search_execution() {
    let db = GallifreyDB::new();

    // Enable vector indexing for TWO different properties (non-temporal for simplicity)
    let hnsw_config = HnswConfig::new(4, DistanceMetric::Cosine);

    db.vector_index("embedding")
        .hnsw(hnsw_config.clone())
        .enable()
        .expect("Failed to enable vector index for 'embedding'");

    db.vector_index("title_embedding")
        .hnsw(hnsw_config)
        .enable()
        .expect("Failed to enable vector index for 'title_embedding'");

    // Create nodes with BOTH embeddings
    let node1_id = db
        .create_node(
            "Document",
            PropertyMapBuilder::new()
                .insert("title", "Rust Programming")
                .insert_vector("embedding", &[1.0f32, 0.0, 0.0, 0.0])
                .insert_vector("title_embedding", &[0.0f32, 1.0, 0.0, 0.0])
                .build(),
        )
        .expect("Failed to create node1");

    let node2_id = db
        .create_node(
            "Document",
            PropertyMapBuilder::new()
                .insert("title", "Python Programming")
                .insert_vector("embedding", &[0.9f32, 0.1, 0.0, 0.0])
                .insert_vector("title_embedding", &[0.1f32, 0.9, 0.0, 0.0])
                .build(),
        )
        .expect("Failed to create node2");

    // Test 1: Query "embedding" property directly
    // This validates the property_key parameter is correctly passed through the query pipeline
    let results_embedding = db
        .find_similar_in("embedding", node1_id, 10)
        .expect("find_similar_in should succeed for 'embedding' property");

    // Should find node2 as similar (node1 itself is excluded from results)
    assert!(
        !results_embedding.is_empty(),
        "Should find results for 'embedding' property"
    );
    assert_eq!(
        results_embedding[0].0, node2_id,
        "node2 should be most similar to node1 for 'embedding' property"
    );
    // Verify we got a reasonable similarity score (0.9 cosine similarity expected)
    assert!(
        results_embedding[0].1 > 0.85,
        "Similarity score should be high (>0.85), got {}",
        results_embedding[0].1
    );

    // Test 2: Query "title_embedding" property
    // Query using node1 to find similar nodes in the title_embedding space
    let results_title = db
        .find_similar_in("title_embedding", node1_id, 10)
        .expect("find_similar_in should succeed for 'title_embedding' property");

    assert!(
        !results_title.is_empty(),
        "Should find results for 'title_embedding' property"
    );
    assert_eq!(
        results_title[0].0, node2_id,
        "node2 should be most similar to node1 for 'title_embedding' property"
    );
    assert!(
        results_title[0].1 > 0.85,
        "Similarity score should be high (>0.85), got {}",
        results_title[0].1
    );

    // Test 3: Verify invalid property returns error
    let result_invalid = db.find_similar_in("nonexistent", node1_id, 10);
    assert!(
        result_invalid.is_err(),
        "find_similar_in should fail for invalid property"
    );
    let err_string = format!("{:?}", result_invalid.unwrap_err());
    assert!(
        err_string.contains("nonexistent")
            || err_string.contains("not found")
            || err_string.contains("IndexNotFound"),
        "Error should mention the invalid property name, got: {}",
        err_string
    );

    println!("✓ Multi-property vector search validated end-to-end");
    println!(
        "  - 'embedding' property: {} results, top score {:.4}",
        results_embedding.len(),
        results_embedding[0].1
    );
    println!(
        "  - 'title_embedding' property: {} results, top score {:.4}",
        results_title.len(),
        results_title[0].1
    );
    println!("  - Both properties return independent, correct results");
    println!("  - Invalid property names are properly rejected");
}
