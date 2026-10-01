use aletheiadb::config::{AletheiaDBConfig, HistoricalConfigBuilder, WalConfigBuilder};
use aletheiadb::storage::index_persistence::PersistenceConfig;
use aletheiadb::{AletheiaDB, PropertyMapBuilder, WriteOps};
use tempfile::tempdir;

#[test]
fn test_reproduce_config_gap() {
    let dir = tempdir().unwrap();
    let wal_dir = dir.path().join("wal");

    // Create config with disabled persistence
    // NOW: We CAN set anchor_interval here!
    let config = AletheiaDBConfig::builder()
        .wal(WalConfigBuilder::new().wal_dir(wal_dir).build())
        .historical(
            HistoricalConfigBuilder::new()
                .anchor_interval(5)
                .unwrap() // Setting custom interval
                .build(),
        )
        .persistence(PersistenceConfig {
            enabled: false,
            ..PersistenceConfig::default()
        })
        .build();

    let db = AletheiaDB::with_unified_config(config).unwrap();

    // Create a node (Version 1 - Anchor)
    let node_id = db
        .create_node("Test", PropertyMapBuilder::new().insert("v", 1).build())
        .unwrap();

    // Add 5 more versions (Total 6 versions)
    for i in 2..=6 {
        db.write(|tx| tx.update_node(node_id, PropertyMapBuilder::new().insert("v", i).build()))
            .unwrap();
    }

    // Verify stats
    let stats = db.historical_stats().unwrap();
    println!("Stats: {:?}", stats);

    // If interval is 5: each update also stores a structural carry-forward
    // (ADR-0061), so the chain holds 1 + 2 * 5 = 11 versions and every stored
    // version advances the anchor counter:
    // positions 0, 5, 10 are anchors -> 3 anchors, 8 deltas. (With the
    // default interval of 10 this would be only 2 anchors.)

    assert_eq!(
        stats.node_anchor_count, 3,
        "Should have 3 anchors with interval 5 (chain positions 0, 5, 10)"
    );
    assert_eq!(stats.node_delta_count, 8, "Should have 8 deltas");
}
