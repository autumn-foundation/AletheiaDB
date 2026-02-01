#![cfg(test)]

//! Tests for Redb file locking issue with background persistence thread.
//!
//! These tests verify that RedbColdStorage can be safely reopened after
//! the database is dropped, even when a background persistence thread is running.

use gallifreydb::GallifreyDB;
use gallifreydb::PropertyMapBuilder;
use gallifreydb::config::GallifreyDBConfig;
use gallifreydb::storage::cold_storage::{ColdStorageConfig as RedbConfig, RedbColdStorage};
use gallifreydb::storage::index_persistence::PersistenceConfig;
use gallifreydb::storage::tiered_storage::{TieredStorage, TieredStorageConfig};
use std::sync::Arc;
use tempfile::TempDir;

/// Test that Redb file can be reopened after database shutdown with background thread.
///
/// This is the core issue: When GallifreyDB has a background persistence thread,
/// dropping the database signals shutdown but doesn't wait for the thread to finish.
/// The background thread holds Arc references to TieredStorage → RedbColdStorage,
/// which keeps the Redb database file locked. When we try to reopen the same file,
/// it fails because Redb requires exclusive file access.
///
/// Setup:
/// 1. Create database with Redb cold storage and persistence enabled
/// 2. Add some data to trigger background thread activity
/// 3. Drop the database (signals shutdown but returns immediately)
/// 4. Try to reopen the same Redb file
///
/// Expected (before fix): Fails with "Failed to open Redb database" (file still locked)
/// Expected (after fix): Successfully reopens the database file
#[test]
fn test_reopen_redb_after_shutdown_with_background_thread() {
    let temp_dir = TempDir::new().unwrap();
    let data_dir = temp_dir.path().join("data");
    let cold_path = data_dir.join("cold.redb");

    // Phase 1: Create database with persistence enabled (spawns background thread)
    {
        let config = GallifreyDBConfig::builder()
            .persistence(PersistenceConfig {
                enabled: true,
                data_dir: data_dir.clone(),
                load_on_startup: false,
                ..Default::default()
            })
            .build();

        let db = GallifreyDB::with_unified_config(config).unwrap();

        // Create Redb cold storage
        let cold = RedbColdStorage::new(&cold_path, RedbConfig::new()).unwrap();
        // Removed Box::new wrapper for RedbColdStorage
        let tiered = Arc::new(TieredStorage::new(TieredStorageConfig::default(), cold));

        // Wire up tiered storage to historical storage
        {
            let mut historical = db.__test_historical_storage().write();
            historical.set_tiered_storage(tiered.clone());
        }

        // Add some data to trigger background thread activity
        let _node1 = db
            .create_node(
                "Person",
                PropertyMapBuilder::new()
                    .insert("name", "Alice")
                    .insert("age", 30i64)
                    .build(),
            )
            .unwrap();

        let _node2 = db
            .create_node(
                "Person",
                PropertyMapBuilder::new()
                    .insert("name", "Bob")
                    .insert("age", 25i64)
                    .build(),
            )
            .unwrap();

        // Persist to ensure background thread has references
        db.persist_indexes().unwrap();

        // Drop database - this signals shutdown but returns immediately
        // The background thread may still be running and holding the Redb file
        println!("Dropping database...");
        drop(db);
        println!("Database dropped, but background thread may still be running");
    }

    // Phase 2: Try to reopen the same Redb file
    // THIS IS THE KEY TEST: Should the file still be locked?
    println!("Attempting to reopen Redb file...");

    // Before fix: This fails with "Failed to open Redb database: DatabaseAlreadyOpen"
    // After fix: This succeeds because background thread has fully exited
    let result = RedbColdStorage::new(&cold_path, RedbConfig::new());

    match result {
        Ok(_) => println!("✓ Successfully reopened Redb file"),
        Err(e) => {
            println!("✗ Failed to reopen Redb file: {}", e);
            panic!(
                "EXPECTED FAILURE (RED phase): Background thread still holds Redb file lock. \
                    This test should fail before the fix is implemented. \
                    Error: {}",
                e
            );
        }
    }
}

/// Test that Redb file can be reopened immediately without background thread.
///
/// This is a control test to verify that the issue is specifically related to the
/// background persistence thread, not Redb itself.
#[test]
fn test_reopen_redb_without_background_thread() {
    let temp_dir = TempDir::new().unwrap();
    let cold_path = temp_dir.path().join("cold.redb");

    // Create Redb storage without any background threads
    {
        let _cold = RedbColdStorage::new(&cold_path, RedbConfig::new()).unwrap();
        // Drop immediately
    }

    // Should be able to reopen immediately since no background thread involved
    let result = RedbColdStorage::new(&cold_path, RedbConfig::new());
    assert!(
        result.is_ok(),
        "Should be able to reopen Redb file without background thread"
    );
}
