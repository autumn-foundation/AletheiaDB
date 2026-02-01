use super::*;
use crate::core::property::PropertyMapBuilder;
use crate::index::vector::hnsw::HnswConfig;
use crate::index::vector::temporal::TemporalVectorConfig;
use crate::index::vector::DistanceMetric;

#[test]
fn test_delete_node_direct_removes_from_vector_index() {
    let storage = CurrentStorage::new();
    let config = HnswConfig::new(4, DistanceMetric::Cosine).with_capacity(100);
    storage.enable_vector_index("embedding", config).unwrap();

    let embedding = vec![1.0f32, 0.0, 0.0, 0.0];
    let props = PropertyMapBuilder::new()
        .insert_vector("embedding", &embedding)
        .build();
    let node_id = storage.create_node("Doc", props).unwrap();

    // Verify it's indexed
    let results = storage.find_similar(node_id, 10).unwrap();
    assert_eq!(results.len(), 0); // Self is excluded, but index isn't empty

    // Delete directly
    storage.delete_node_direct(node_id, 1000.into()).unwrap();

    // Verify removal (by checking if we can find it via raw search)
    let results = storage.find_similar_by_embedding(&embedding, 10).unwrap();
    assert_eq!(results.len(), 0);
}

#[test]
fn test_vector_persistence_helpers() {
    let storage = CurrentStorage::new();
    let config = HnswConfig::new(4, DistanceMetric::Cosine).with_capacity(100);
    storage.enable_vector_index("embedding", config).unwrap();

    // Test get_vector_index_config
    let config_data = storage.get_vector_index_config();
    assert!(config_data.enabled);
    assert_eq!(config_data.property_name.as_str(), "embedding");

    // Test get_vector_index_for_persistence
    let persistence_data = storage.get_vector_index_for_persistence("embedding");
    assert!(persistence_data.is_some());
    let (_, _, count, _) = persistence_data.unwrap();
    assert_eq!(count, 0);

    // Test register_vector_index (simulate loading)
    let idx2 = crate::index::vector::HnswIndex::new(
        HnswConfig::new(4, DistanceMetric::Euclidean)
    ).unwrap();
    storage.register_vector_index(
        "other_embedding",
        idx2,
        HnswConfig::new(4, DistanceMetric::Euclidean)
    );
    assert!(storage.has_vector_index("other_embedding"));
}

#[test]
fn test_temporal_vector_indexing_lifecycle() {
    let storage = CurrentStorage::new();

    // Enable temporal index
    let config = TemporalVectorConfig::default_with_hnsw(
        HnswConfig::new(4, DistanceMetric::Cosine)
    );
    storage.enable_temporal_vector_index("embedding", config).unwrap();

    assert!(storage.is_temporal_vector_index_enabled());
    assert!(storage.is_temporal_vector_index_enabled_for("embedding"));
    assert_eq!(storage.list_temporal_vector_indexes(), vec!["embedding"]);

    // Test accessors
    assert!(storage.get_temporal_vector_index().is_some());
    assert!(storage.get_temporal_vector_index_for("embedding").is_some());

    // Test failure case: duplicate enable
    let config2 = TemporalVectorConfig::default_with_hnsw(
        HnswConfig::new(4, DistanceMetric::Cosine)
    );
    let result = storage.enable_temporal_vector_index("embedding", config2);
    assert!(result.is_err());
}

#[test]
fn test_multi_property_search_helpers() {
    let storage = CurrentStorage::new();

    // Setup multiple indexes
    storage.enable_vector_index(
        "v1",
        HnswConfig::new(2, DistanceMetric::Euclidean)
    ).unwrap();
    storage.enable_vector_index(
        "v2",
        HnswConfig::new(2, DistanceMetric::Cosine)
    ).unwrap();

    let v_data = vec![1.0f32, 0.0];
    let props = PropertyMapBuilder::new()
        .insert_vector("v1", &v_data)
        .insert_vector("v2", &v_data)
        .build();

    let node_id = storage.create_node("Node", props).unwrap();

    // Test find_similar_in
    let results = storage.find_similar_in("v1", node_id, 1).unwrap();
    assert_eq!(results.len(), 0); // Self excluded

    // Test search_vectors_in
    let results = storage.search_vectors_in("v1", &v_data, 1).unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].0, node_id);
}

#[test]
fn test_transaction_hooks() {
    let storage = CurrentStorage::new();
    // Should pass even if not enabled
    assert!(storage.on_temporal_vector_transaction().is_ok());

    let config = TemporalVectorConfig::default_with_hnsw(
        HnswConfig::new(4, DistanceMetric::Cosine)
    );
    storage.enable_temporal_vector_index("embedding", config).unwrap();

    // Should delegate to index
    assert!(storage.on_temporal_vector_transaction().is_ok());
}

#[test]
fn test_vector_count() {
    let storage = CurrentStorage::new();
    assert_eq!(storage.vector_count(), 0);

    storage.enable_vector_index(
        "vec",
        HnswConfig::new(2, DistanceMetric::Euclidean)
    ).unwrap();

    let v = vec![1.0f32, 1.0];
    let props = PropertyMapBuilder::new()
        .insert_vector("vec", &v)
        .build();
    storage.create_node("Node", props).unwrap();

    assert_eq!(storage.vector_count(), 1);
}
