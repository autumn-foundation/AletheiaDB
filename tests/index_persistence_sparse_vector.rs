use aletheiadb::AletheiaDB;
use aletheiadb::config::{AletheiaDBConfig, WalConfigBuilder};
use aletheiadb::index::vector::temporal::{SnapshotStrategy, TemporalVectorConfig};
use aletheiadb::index::vector::{DistanceMetric, HnswConfig};
use aletheiadb::prelude::*;
use aletheiadb::storage::index_persistence::PersistenceConfig;
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
    let temp_config = TemporalVectorConfig {
        snapshot_strategy: SnapshotStrategy::TransactionInterval(10),
        ..Default::default()
    };

    db.vector_index("embedding")
        .hnsw(HnswConfig::new(384, DistanceMetric::Cosine))
        .temporal(temp_config)
        .enable()
        .unwrap();

    // Create a node with a vector to establish the anchor
    let embedding1 = vec![0.1f32; 384];
    let node_id = db
        .create_node(
            "Doc",
            aletheiadb::properties! {
                "embedding" => &embedding1[..]
            },
        )
        .unwrap();

    // Update the node's vector slightly to generate a Sparse VectorDelta
    let mut embedding2 = embedding1.clone();
    embedding2[0] = 0.5f32;
    db.write(|tx| {
        tx.update_node(
            node_id,
            aletheiadb::properties! {
                "embedding" => &embedding2[..]
            },
        )
    })
    .unwrap();

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
    let recovered_node = db2.get_node(node_id).unwrap();

    // Convert property back to vec to verify
    let recovered_embedding: Vec<f32> = match recovered_node.properties.get("embedding").unwrap() {
        aletheiadb::core::property::PropertyValue::Vector(v) => v.to_vec(),
        _ => panic!("Expected vector property"),
    };

    assert_eq!(recovered_embedding[0], 0.5f32);
    assert_eq!(recovered_embedding[1], 0.1f32);
}
