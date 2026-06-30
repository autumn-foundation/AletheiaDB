//! Integration tests for portable backup / restore (issue #3217).
//!
//! # TDD Phase
//!
//! **Red phase**: these tests are intentionally written before the implementation
//! exists.  They will fail to compile until the following are in place:
//!
//! - `aletheiadb::storage::backup` module
//! - `AletheiaDB::backup(&self, path)` / `::restore(path)` / `::restore_to_data_dir(path, dir)`
//! - `aletheiadb::BackupError` and `aletheiadb::BackupSummary` public types
//! - `aletheiadb::core::error::Error::Backup(BackupError)` variant
//!
//! Once the implementation exists the tests are expected to **pass** (green phase).

use aletheiadb::core::error::Error;
use aletheiadb::core::id::NodeId;
use aletheiadb::storage::backup::BackupError;
use aletheiadb::{
    AletheiaDB, BackupSummary, PropertyMapBuilder, Timestamp, WriteOps, WriteTransaction, time,
};
use proptest::prelude::*;
use serial_test::serial;
use tempfile::TempDir;

// ============================================================================
// Helpers
// ============================================================================

/// Create a small graph: Alice→Bob with a vector embedding on Alice.
fn build_sample_db() -> (AletheiaDB, NodeId, NodeId) {
    let db = AletheiaDB::new().unwrap();

    let alice_id = db
        .create_node(
            "Person",
            PropertyMapBuilder::new()
                .insert("name", "Alice")
                .insert("age", 30i64)
                .insert_vector("embedding", &[0.1f32, 0.2, 0.3, 0.4])
                .build(),
        )
        .unwrap();

    let bob_id = db
        .create_node(
            "Person",
            PropertyMapBuilder::new()
                .insert("name", "Bob")
                .insert("age", 25i64)
                .build(),
        )
        .unwrap();

    db.create_edge(
        alice_id,
        bob_id,
        "KNOWS",
        PropertyMapBuilder::new().insert("since", 2020i64).build(),
    )
    .unwrap();

    (db, alice_id, bob_id)
}

// ============================================================================
// Test 1 — backup_creates_artifact
// ============================================================================

/// A `backup()` call must produce a non-empty file at the specified path.
#[test]
#[serial]
fn backup_creates_artifact() {
    let (db, _, _) = build_sample_db();
    let tmp = TempDir::new().unwrap();
    let backup_path = tmp.path().join("test.albk");

    let summary: BackupSummary = db.backup(&backup_path).unwrap();

    assert!(backup_path.exists(), "backup file must be created");
    assert!(
        backup_path.metadata().unwrap().len() > 0,
        "backup file must be non-empty"
    );
    assert!(
        summary.bytes_written > 0,
        "summary must report bytes written"
    );
    // At least one entity (node version or current node) must be recorded.
    assert!(
        summary.node_versions > 0 || summary.current_node_count > 0,
        "summary must report at least one node/version"
    );
}

// ============================================================================
// Test 2 — roundtrip_preserves_current_state
// ============================================================================

/// After restore the current-state node/edge counts and all properties
/// (including a vector embedding) must match the original database.
#[test]
#[serial]
fn roundtrip_preserves_current_state() {
    let (db, alice_id, bob_id) = build_sample_db();
    let tmp = TempDir::new().unwrap();
    let backup_path = tmp.path().join("state.albk");

    db.backup(&backup_path).unwrap();

    let alice_orig = db.get_node(alice_id).unwrap();
    let bob_orig = db.get_node(bob_id).unwrap();

    // Restore into a fresh ephemeral instance.
    let restored = AletheiaDB::restore(&backup_path).unwrap();

    assert_eq!(
        restored.node_count(),
        db.node_count(),
        "node count must match after restore"
    );
    assert_eq!(
        restored.edge_count(),
        db.edge_count(),
        "edge count must match after restore"
    );

    let alice_rest = restored.get_node(alice_id).unwrap();
    assert_eq!(
        alice_rest.get_property("name"),
        alice_orig.get_property("name"),
        "alice.name must survive backup/restore"
    );
    assert_eq!(
        alice_rest.get_property("age"),
        alice_orig.get_property("age"),
        "alice.age must survive backup/restore"
    );
    // Vector embedding must be preserved exactly.
    assert_eq!(
        alice_rest.get_property("embedding"),
        alice_orig.get_property("embedding"),
        "vector embedding must survive backup/restore"
    );

    let bob_rest = restored.get_node(bob_id).unwrap();
    assert_eq!(
        bob_rest.get_property("name"),
        bob_orig.get_property("name"),
        "bob.name must survive backup/restore"
    );
}

// ============================================================================
// Test 3 — roundtrip_preserves_bitemporal_history
// ============================================================================

/// After restore, `get_node_history` and `get_node_at_time` at multiple
/// (valid-time, tx-time) coordinates must return identical results including
/// byte-for-byte time stamps on both axes.
#[test]
#[serial]
fn roundtrip_preserves_bitemporal_history() {
    let db = AletheiaDB::new().unwrap();

    let t0 = time::from_secs(1_000_000);
    let t1 = time::from_secs(2_000_000);
    let t2 = time::from_secs(3_000_000);

    let mut tx0: WriteTransaction = db.write_transaction().unwrap();
    let node_id = tx0
        .create_node_with_valid_time(
            "Event",
            PropertyMapBuilder::new().insert("val", 0i64).build(),
            Some(t0),
        )
        .unwrap();
    let tt0 = tx0.commit_with_timestamp().unwrap();

    let mut tx1: WriteTransaction = db.write_transaction().unwrap();
    tx1.update_node_with_valid_time(
        node_id,
        PropertyMapBuilder::new().insert("val", 1i64).build(),
        Some(t1),
    )
    .unwrap();
    let tt1 = tx1.commit_with_timestamp().unwrap();

    let mut tx2: WriteTransaction = db.write_transaction().unwrap();
    tx2.update_node_with_valid_time(
        node_id,
        PropertyMapBuilder::new().insert("val", 2i64).build(),
        Some(t2),
    )
    .unwrap();
    let tt2 = tx2.commit_with_timestamp().unwrap();

    let orig_history = db.get_node_history(node_id).unwrap();
    assert!(
        orig_history.versions.len() >= 3,
        "expected at least 3 versions before backup"
    );

    let tmp = TempDir::new().unwrap();
    let backup_path = tmp.path().join("history.albk");
    db.backup(&backup_path).unwrap();

    let restored = AletheiaDB::restore(&backup_path).unwrap();

    let rest_history = restored.get_node_history(node_id).unwrap();
    assert_eq!(
        orig_history.versions.len(),
        rest_history.versions.len(),
        "version count must match after restore"
    );

    // Every version must carry identical bi-temporal intervals.
    for (orig_v, rest_v) in orig_history
        .versions
        .iter()
        .zip(rest_history.versions.iter())
    {
        assert_eq!(
            orig_v.temporal.valid_time(),
            rest_v.temporal.valid_time(),
            "valid-time range must match"
        );
        assert_eq!(
            orig_v.temporal.transaction_time(),
            rest_v.temporal.transaction_time(),
            "transaction-time range must be byte-for-byte identical"
        );
        assert_eq!(
            orig_v.version_number, rest_v.version_number,
            "version number must match"
        );
    }

    // Point-in-time reconstruction must return the same property values.
    let checkpoints: &[(i64, Timestamp, Timestamp)] = &[(0, t0, tt0), (1, t1, tt1), (2, t2, tt2)];
    for (expected, vt, tt) in checkpoints {
        let orig_node = db.get_node_at_time(node_id, *vt, *tt).unwrap();
        let rest_node = restored
            .get_node_at_time(node_id, *vt, *tt)
            .unwrap_or_else(|_| panic!("restored DB lost node at vt={:?} tt={:?}", vt, tt));
        assert_eq!(
            orig_node.get_property("val"),
            rest_node.get_property("val"),
            "val at checkpoint {} must match",
            expected
        );
    }
}

// ============================================================================
// Test 4 — roundtrip_with_cold_storage_configured
// ============================================================================

/// When cold storage is enabled the backup must include versions from every
/// tier and the restore must reproduce them all.
#[test]
#[serial]
fn roundtrip_with_cold_storage_configured() {
    use aletheiadb::config::{AletheiaDBConfig, HistoricalConfigBuilder, WalConfigBuilder};
    use std::time::Duration;

    let tmp_src = TempDir::new().unwrap();
    let cold_path = tmp_src.path().join("cold.redb");
    // Give the source DB its own isolated WAL directory so that concurrent
    // test binaries running against the default relative "aletheiadb/wal/"
    // path cannot contaminate the WAL segments read during restore.
    let wal_path = tmp_src.path().join("wal");

    let config = AletheiaDBConfig::builder()
        .historical(
            HistoricalConfigBuilder::new()
                .enable_cold_storage(true)
                .cold_storage_path(&cold_path)
                // Keep very few hot versions so cold migration can occur.
                .max_hot_versions(2)
                .migration_age_threshold(Duration::from_nanos(1))
                .build(),
        )
        .wal(WalConfigBuilder::new().wal_dir(wal_path).build())
        .build();

    let db = AletheiaDB::with_unified_config(config).unwrap();

    let node_id = db
        .create_node(
            "Record",
            PropertyMapBuilder::new().insert("v", 0i64).build(),
        )
        .unwrap();

    for i in 1..=6i64 {
        db.write(|tx| {
            tx.update_node(node_id, PropertyMapBuilder::new().insert("v", i).build())?;
            Ok::<_, aletheiadb::Error>(())
        })
        .unwrap();
    }

    let orig_history = db.get_node_history(node_id).unwrap();
    assert!(
        orig_history.versions.len() >= 3,
        "need at least 3 versions to exercise multi-tier backup"
    );

    let backup_path = tmp_src.path().join("cold_backup.albk");
    let summary = db.backup(&backup_path).unwrap();
    // Backup must report it captured at least as many versions as we can query.
    assert!(
        summary.node_versions >= orig_history.versions.len() as u64,
        "backup must capture all versions visible through get_node_history"
    );

    // Drop the source DB before restoring to avoid GLOBAL_INTERNER conflicts:
    // materialize_to_dir calls GLOBAL_INTERNER.clear() then repopulates it from
    // the backup, so any live DB sharing that interner would see stale indices.
    drop(db);

    let restored = AletheiaDB::restore(&backup_path).unwrap();

    let rest_history = restored.get_node_history(node_id).unwrap();
    assert_eq!(
        orig_history.versions.len(),
        rest_history.versions.len(),
        "all versions (incl. cold-tier) must survive backup/restore"
    );
}

// ============================================================================
// Test 5 — restore_rejects_nonempty_target
// ============================================================================

/// Restoring into a directory that already contains a manifest must yield
/// a typed `BackupError::TargetNotEmpty` error without modifying anything.
#[test]
#[serial]
fn restore_rejects_nonempty_target() {
    let (db, _, _) = build_sample_db();
    let tmp = TempDir::new().unwrap();
    let backup_path = tmp.path().join("ne_test.albk");
    db.backup(&backup_path).unwrap();

    // Simulate a non-empty target: write a fake manifest at the path that
    // IndexPersistenceManager checks (data_dir/indexes/manifest.idx).
    let data_dir = tmp.path().join("target_data");
    std::fs::create_dir_all(data_dir.join("indexes")).unwrap();
    std::fs::write(data_dir.join("indexes").join("manifest.idx"), b"fake").unwrap();

    let err = AletheiaDB::restore_to_data_dir(&backup_path, &data_dir).unwrap_err();

    assert!(
        matches!(err, Error::Backup(BackupError::TargetNotEmpty)),
        "expected BackupError::TargetNotEmpty, got: {err:?}"
    );
}

// ============================================================================
// Test 6 — restore_rejects_bad_version
// ============================================================================

/// A backup whose format_version is set to an unknown value must be rejected
/// with a typed `BackupError::IncompatibleVersion` error.
#[test]
#[serial]
fn restore_rejects_bad_version() {
    let (db, _, _) = build_sample_db();
    let tmp = TempDir::new().unwrap();
    let backup_path = tmp.path().join("version_test.albk");
    db.backup(&backup_path).unwrap();

    // Bytes 4-5 (little-endian u16) hold format_version; flip them to 0xFFFF.
    let mut data = std::fs::read(&backup_path).unwrap();
    assert!(
        data.len() >= 6,
        "backup file too short to be a valid artifact"
    );
    data[4] = 0xFF;
    data[5] = 0xFF;
    std::fs::write(&backup_path, &data).unwrap();

    let err = AletheiaDB::restore(&backup_path).unwrap_err();

    assert!(
        matches!(err, Error::Backup(BackupError::IncompatibleVersion { .. })),
        "expected BackupError::IncompatibleVersion, got: {err:?}"
    );
}

// ============================================================================
// Test 7 — restore_rejects_corrupt_magic
// ============================================================================

/// A file whose first 4 bytes are not the backup magic must be rejected with
/// a typed `BackupError::BadMagic` error.
#[test]
#[serial]
fn restore_rejects_corrupt_magic() {
    let (db, _, _) = build_sample_db();
    let tmp = TempDir::new().unwrap();
    let backup_path = tmp.path().join("magic_test.albk");
    db.backup(&backup_path).unwrap();

    let mut data = std::fs::read(&backup_path).unwrap();
    data[0] = 0xFF;
    data[1] = 0xFF;
    data[2] = 0xFF;
    data[3] = 0xFF;
    std::fs::write(&backup_path, &data).unwrap();

    let err = AletheiaDB::restore(&backup_path).unwrap_err();

    assert!(
        matches!(err, Error::Backup(BackupError::BadMagic)),
        "expected BackupError::BadMagic, got: {err:?}"
    );
}

// ============================================================================
// Test 8 — prop_roundtrip_observationally_equivalent
// ============================================================================

/// Property: for any bi-temporal node lifecycle, restore(backup(db)) is
/// observationally indistinguishable from db when queried over all recorded
/// (valid-time, tx-time) checkpoints.
///
/// Uses a manual test-runner so `#[serial]` can guard GLOBAL_INTERNER access.
#[test]
#[serial]
fn prop_roundtrip_observationally_equivalent() {
    let config = ProptestConfig::with_cases(50);
    let mut runner = proptest::test_runner::TestRunner::new(config);

    runner
        .run(
            &(
                1usize..=6usize,
                proptest::collection::vec(1i64..=500_000i64, 1..=6),
            ),
            |(update_count, t_offsets)| {
                let db = AletheiaDB::new().unwrap();
                let t_base = time::from_secs(100_000i64);

                let mut tx0: WriteTransaction = db.write_transaction().unwrap();
                let node_id = tx0
                    .create_node_with_valid_time(
                        "Item",
                        PropertyMapBuilder::new().insert("v", 0i64).build(),
                        Some(t_base),
                    )
                    .unwrap();
                let tt0 = tx0.commit_with_timestamp().unwrap();

                let mut checkpoints: Vec<(i64, Timestamp, Timestamp)> = vec![(0, t_base, tt0)];

                // Sort offsets to guarantee monotone valid-times (avoids
                // ValidTimeBeforeEntityCreation on out-of-order inputs).
                let mut sorted_offsets = t_offsets.clone();
                sorted_offsets.sort();
                sorted_offsets.dedup();
                let ops = update_count.min(sorted_offsets.len());
                for (i, &offset) in sorted_offsets.iter().enumerate().take(ops) {
                    let vt = time::from_secs(100_000 + offset);
                    let mut tx: WriteTransaction = db.write_transaction().unwrap();
                    tx.update_node_with_valid_time(
                        node_id,
                        PropertyMapBuilder::new()
                            .insert("v", (i + 1) as i64)
                            .build(),
                        Some(vt),
                    )
                    .unwrap();
                    let tt = tx.commit_with_timestamp().unwrap();
                    checkpoints.push(((i + 1) as i64, vt, tt));
                }

                let tmp = TempDir::new().unwrap();
                let backup_path = tmp.path().join("prop.albk");
                db.backup(&backup_path).unwrap();

                let restored = AletheiaDB::restore(&backup_path).unwrap();

                // History lengths must match.
                let orig_hist = db.get_node_history(node_id).unwrap();
                let rest_hist = restored.get_node_history(node_id).unwrap();
                prop_assert_eq!(
                    orig_hist.versions.len(),
                    rest_hist.versions.len(),
                    "history version count must match"
                );

                // Every checkpoint must reconstruct the same value.
                for (expected, vt, tt) in &checkpoints {
                    let orig_node = match db.get_node_at_time(node_id, *vt, *tt) {
                        Ok(n) => n,
                        Err(_) => continue,
                    };
                    let rest_node = restored.get_node_at_time(node_id, *vt, *tt).map_err(|e| {
                        TestCaseError::fail(format!(
                            "restored DB lost node at vt={:?} tt={:?}: {e}",
                            vt, tt
                        ))
                    })?;
                    prop_assert_eq!(
                        orig_node.get_property("v"),
                        rest_node.get_property("v"),
                        "val must match at checkpoint {}",
                        expected
                    );
                }
                Ok(())
            },
        )
        .unwrap();
}
