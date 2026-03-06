use aletheiadb::config::{AletheiaDBConfig, WalConfigBuilder};
use aletheiadb::storage::index_persistence::PersistenceConfig;
use aletheiadb::AletheiaDB;
use aletheiadb::prelude::*;
use aletheiadb::index::vector::{HnswConfig, DistanceMetric};
use aletheiadb::index::vector::temporal::{TemporalVectorConfig, SnapshotStrategy};
use tempfile::tempdir;

#[test]
fn test_persist_sparse_vector_delta_materializes() {
    let dir = tempdir().unwrap();
    let data_dir = dir.path().to_path_buf();

    let mut wal_config = WalConfigBuilder::new().build();
    wal_config.wal_dir = data_dir.join("wal");

    let config = AletheiaDBConfig::builder()
        .persistence(PersistenceConfig {
            enabled: true,
            data_dir: data_dir.clone(),
            ..Default::default()
        })
        .wal(wal_config)
        .build();

    let db = AletheiaDB::with_unified_config(config).unwrap();

    // Enable temporal vector index with sparse strategy
    let temp_config = TemporalVectorConfig { snapshot_strategy: SnapshotStrategy::TransactionInterval(10), ..Default::default() };

    db.vector_index("embedding")
        .hnsw(HnswConfig::new(384, DistanceMetric::Cosine))
        .temporal(temp_config)
        .enable()
        .unwrap();

    // Create two nodes
    let embedding1 = vec![0.1f32; 384];
    let node1_id = db.create_node("Doc", aletheiadb::properties! {
        "embedding" => &embedding1[..]
    }).unwrap();

    let node2_id = db.create_node("Doc", aletheiadb::properties! {
        "embedding" => &embedding1[..]
    }).unwrap();

    // Create an edge between them with a vector
    let edge_id = db.create_edge(node1_id, node2_id, "SIMILAR", aletheiadb::properties! {
        "embedding" => &embedding1[..]
    }).unwrap();

    // Update the node and edge vectors slightly to generate a Sparse VectorDelta
    let mut embedding2 = embedding1.clone();
    embedding2[0] = 0.5f32;

    db.write(|tx| {
        tx.update_node(node1_id, aletheiadb::properties! {
            "embedding" => &embedding2[..]
        })?;
        tx.update_edge(edge_id, aletheiadb::properties! {
            "embedding" => &embedding2[..]
        })
    }).unwrap();

    // Trigger persistence by dropping DB (this simulates the shutdown process which causes the error)
    drop(db);

    let mut wal_config2 = WalConfigBuilder::new().build();
    wal_config2.wal_dir = data_dir.join("wal");

    let config2 = AletheiaDBConfig::builder()
        .persistence(PersistenceConfig {
            enabled: true,
            data_dir: data_dir.clone(),
            load_on_startup: true,
            ..Default::default()
        })
        .wal(wal_config2)
        .build();

    let db2 = AletheiaDB::with_unified_config(config2).unwrap();
    let recovered_node = db2.get_node(node1_id).unwrap();
    let recovered_edge = db2.get_edge(edge_id).unwrap();

    // Convert properties back to vec to verify node
    let recovered_node_embedding: Vec<f32> = match recovered_node.properties.get("embedding").unwrap() {
        aletheiadb::core::property::PropertyValue::Vector(v) => v.to_vec(),
        _ => panic!("Expected vector property"),
    };

    assert_eq!(recovered_node_embedding[0], 0.5f32);
    assert_eq!(recovered_node_embedding[1], 0.1f32);

    // Convert properties back to vec to verify edge
    let recovered_edge_embedding: Vec<f32> = match recovered_edge.properties.get("embedding").unwrap() {
        aletheiadb::core::property::PropertyValue::Vector(v) => v.to_vec(),
        _ => panic!("Expected vector property"),
    };

    assert_eq!(recovered_edge_embedding[0], 0.5f32);
    assert_eq!(recovered_edge_embedding[1], 0.1f32);
}
