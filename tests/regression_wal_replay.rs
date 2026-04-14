use aletheiadb::config::AletheiaDBConfig;
use aletheiadb::storage::index_persistence::PersistenceConfig;
use aletheiadb::{AletheiaDB, PropertyMapBuilder};
use tempfile::TempDir;

#[test]
fn test_repro_data_loss_missing_wal_replay() {
    let temp_dir = TempDir::new().unwrap();
    let data_dir = temp_dir.path().join("data");
    let wal_dir = temp_dir.path().join("wal");

    let config = AletheiaDBConfig::builder()
        .wal(
            aletheiadb::config::WalConfigBuilder::new()
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

    // 1. Create DB and write data (persisted to WAL)
    {
        let db = AletheiaDB::with_unified_config(config.clone()).unwrap();

        for i in 0..10 {
            db.create_node(
                "Person",
                PropertyMapBuilder::new().insert("id", i as i64).build(),
            )
            .unwrap();
        }

        // Graceful shutdown (flushes WAL)
    }

    // 2. SIMULATE INDEX CORRUPTION / LOSS
    // Remove the 'indexes' directory but KEEP the 'wal' directory.
    // This simulates a scenario where we want to recover from WAL because indexes are gone or stale.
    // Or simply the first start after enabling persistence on an existing WAL-only DB.
    let indexes_path = data_dir.join("indexes");
    if indexes_path.exists() {
        std::fs::remove_dir_all(&indexes_path).unwrap();
    }

    // 3. Reopen DB
    // Since indexes are missing, it should fallback to full WAL replay.
    // If WAL replay is missing, we get 0 nodes.
    {
        let db = AletheiaDB::with_unified_config(config).unwrap();

        let count = db.node_count();
        println!("Node count after recovery: {}", count);

        assert_eq!(
            count, 10,
            "Data loss detected! Expected 10 nodes (from WAL), found {}",
            count
        );
    }
}
