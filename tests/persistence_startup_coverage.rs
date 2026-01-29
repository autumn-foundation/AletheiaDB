use gallifreydb::config::{GallifreyDBConfigBuilder, WalConfigBuilder};
use gallifreydb::core::property::PropertyMapBuilder;
use gallifreydb::storage::index_persistence::PersistenceConfig;
use gallifreydb::GallifreyDB;
use tempfile::tempdir;

#[test]
fn test_persistence_load_on_startup() {
    let dir = tempdir().unwrap();
    let data_dir = dir.path().join("data");
    let wal_dir = dir.path().join("wal");

    // 1. Configure and create DB with persistence enabled
    let config = GallifreyDBConfigBuilder::new()
        .wal(
            WalConfigBuilder::new()
                .wal_dir(wal_dir.clone())
                .build(),
        )
        .persistence(PersistenceConfig {
            enabled: true,
            data_dir: data_dir.clone(),
            load_on_startup: true,
            ..Default::default()
        })
        .build();

    let saved_node_id;

    {
        let db = GallifreyDB::with_unified_config(config.clone()).expect("Failed to create DB");

        // 2. Add some data
        let props = PropertyMapBuilder::new()
            .insert("name", "Alice")
            .build();
        let node_id = db.create_node("Person", props).expect("Failed to create node");
        saved_node_id = node_id;

        // 3. Persist manually to ensure data is on disk
        db.persist_indexes().expect("Failed to persist indexes");
    } // db is dropped here

    // 4. Re-open DB with same config (load_on_startup = true)
    let db = GallifreyDB::with_unified_config(config).expect("Failed to re-open DB");

    // 5. Verify data is loaded
    assert_eq!(db.node_count(), 1, "Should have loaded 1 node");

    // Check if we can retrieve the node
    let node = db.get_node(saved_node_id).expect("Node should exist");

    assert_eq!(
        node.get_property("name").and_then(|v| v.as_str()),
        Some("Alice"),
        "Node property should be preserved"
    );
}
