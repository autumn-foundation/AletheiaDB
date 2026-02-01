use gallifreydb::config::{GallifreyDBConfig, HistoricalConfigBuilder, WalConfigBuilder};
use gallifreydb::storage::index_persistence::PersistenceConfig;
use gallifreydb::{GallifreyDB, PropertyMapBuilder, WriteOps};
use tempfile::tempdir;

#[test]
fn test_reproduce_config_gap() {
    let dir = tempdir().unwrap();
    let wal_dir = dir.path().join("wal");

    // Create config with disabled persistence
    // NOW: We CAN set anchor_interval here!
    let config = GallifreyDBConfig::builder()
        .wal(WalConfigBuilder::new().wal_dir(wal_dir).build())
        .historical(
            HistoricalConfigBuilder::new()
                .anchor_interval(5).unwrap() // Setting custom interval
                .build()
        )
        .persistence(PersistenceConfig {
            enabled: false,
            ..PersistenceConfig::default()
        })
        .build();

    let db = GallifreyDB::with_unified_config(config).unwrap();

    // Create a node (Version 1 - Anchor)
    let node_id = db
        .create_node("Test", PropertyMapBuilder::new().insert("v", 1).build())
        .unwrap();

    // Add 5 more versions (Total 6 versions)
    for i in 2..=6 {
        db.write(|tx| {
            tx.update_node(
                node_id,
                PropertyMapBuilder::new().insert("v", i).build(),
            )
        })
        .unwrap();
    }

    // Verify stats
    let stats = db.historical_stats().unwrap();
    println!("Stats: {:?}", stats);

    // If interval is 5:
    // v1: Anchor (always)
    // v2: Delta (since anchor: 1)
    // v3: Delta (since anchor: 2)
    // v4: Delta (since anchor: 3)
    // v5: Delta (since anchor: 4)
    // v6: Anchor (since anchor: 5 >= 5)
    // Total Anchors: 2

    assert_eq!(stats.node_anchor_count, 2, "Should have 2 anchors with interval 5 (v1 and v6)");
    assert_eq!(stats.node_delta_count, 4, "Should have 4 deltas");
}
