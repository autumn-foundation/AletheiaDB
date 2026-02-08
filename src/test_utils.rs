//! Test utilities for AletheiaDB.
//!
//! This module provides helpers for writing tests that don't leak resources.

use crate::AletheiaDB;
use crate::config::{AletheiaDBConfig, WalConfigBuilder};
use crate::storage::wal::DurabilityMode;
use crate::utils::error::Result;
use std::path::PathBuf;

/// Create a test database with in-memory/temporary WAL.
///
/// This helper ensures tests don't leak WAL files by using a unique temporary directory
/// for each database instance. The directory is automatically cleaned up when the
/// `TempDir` is dropped.
///
/// # Examples
///
/// ```ignore
/// use aletheiadb::test_utils::create_test_db;
///
/// #[test]
/// fn test_something() {
///     let (_temp_dir, db) = create_test_db().unwrap();
///
///     // Use db for testing...
///     let node_id = db.create_node("Person", PropertyMapBuilder::new().build()).unwrap();
///
///     // _temp_dir is dropped here, cleaning up WAL files automatically
/// }
/// ```
///
/// # Errors
///
/// Returns an error if:
/// - Temporary directory creation fails
/// - Database initialization fails
///
/// # Note
///
/// **You MUST keep the returned `TempDir` in scope** for the duration of the test.
/// If you drop it early, the WAL directory will be deleted while the database is still active,
/// causing errors.
///
/// ```ignore
/// // ❌ WRONG - TempDir dropped immediately
/// let db = create_test_db().unwrap().1;
///
/// // ✅ CORRECT - TempDir kept in scope
/// let (_temp_dir, db) = create_test_db().unwrap();
/// ```
pub fn create_test_db() -> Result<(tempfile::TempDir, AletheiaDB)> {
    let temp_dir = tempfile::tempdir().map_err(crate::utils::error::Error::Io)?;

    let wal_dir = temp_dir.path().join("wal");

    let config = AletheiaDBConfig::builder()
        .wal(
            WalConfigBuilder::new()
                .wal_dir(wal_dir)
                .durability_mode(DurabilityMode::Async {
                    flush_interval_ms: 10,
                })
                .build(),
        )
        .build();

    let db = AletheiaDB::with_unified_config(config)?;

    Ok((temp_dir, db))
}

/// Create a test database with custom configuration.
///
/// This is similar to [`create_test_db`] but allows customizing the database configuration
/// while still ensuring WAL files are written to a temporary directory.
///
/// # Examples
///
/// ```ignore
/// use aletheiadb::test_utils::create_test_db_with_config;
/// use aletheiadb::config::{AletheiaDBConfig, WalConfigBuilder};
/// use aletheiadb::storage::wal::DurabilityMode;
///
/// #[test]
/// fn test_with_custom_config() {
///     let config = AletheiaDBConfig::builder()
///         .wal(WalConfigBuilder::new()
///             .num_stripes(8).unwrap()
///             .stripe_capacity(512).unwrap()
///             .durability_mode(DurabilityMode::Synchronous)
///             .build())
///         .build();
///
///     let (_temp_dir, db) = create_test_db_with_config(config).unwrap();
///     // Use db...
/// }
/// ```
///
/// # Errors
///
/// Returns an error if:
/// - Temporary directory creation fails
/// - Database initialization fails
///
/// # Note
///
/// The provided `config`'s `wal_dir` will be **overridden** to use a temporary directory.
/// This ensures tests don't leak WAL files.
pub fn create_test_db_with_config(
    mut config: AletheiaDBConfig,
) -> Result<(tempfile::TempDir, AletheiaDB)> {
    let temp_dir = tempfile::tempdir().map_err(crate::utils::error::Error::Io)?;

    // Override wal_dir to use temp directory
    config.wal.wal_dir = temp_dir.path().join("wal");

    let db = AletheiaDB::with_unified_config(config)?;

    Ok((temp_dir, db))
}

/// Create a test database at a specific path.
///
/// This is useful when you need control over the WAL directory location,
/// but remember: **you are responsible for cleanup**.
///
/// # Examples
///
/// ```ignore
/// use aletheiadb::test_utils::create_test_db_at;
/// use std::path::PathBuf;
///
/// #[test]
/// fn test_with_specific_path() {
///     let wal_dir = PathBuf::from("target/test-wal/my-test");
///     let db = create_test_db_at(wal_dir.clone()).unwrap();
///
///     // Use db...
///
///     // Clean up manually
///     drop(db);
///     std::fs::remove_dir_all(wal_dir).ok();
/// }
/// ```
///
/// # Errors
///
/// Returns an error if database initialization fails.
pub fn create_test_db_at(wal_dir: PathBuf) -> Result<AletheiaDB> {
    let config = AletheiaDBConfig::builder()
        .wal(
            WalConfigBuilder::new()
                .wal_dir(wal_dir)
                .durability_mode(DurabilityMode::Async {
                    flush_interval_ms: 10,
                })
                .build(),
        )
        .build();

    AletheiaDB::with_unified_config(config)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::property::PropertyMapBuilder;

    #[test]
    fn test_create_test_db_cleanup() {
        let temp_dir_path;
        {
            let (_temp_dir, db) = create_test_db().unwrap();
            temp_dir_path = _temp_dir.path().to_path_buf();

            // Directory should exist while in scope
            assert!(temp_dir_path.exists());

            // Should be able to use the database
            let node_id = db
                .create_node("Person", PropertyMapBuilder::new().build())
                .unwrap();
            assert_eq!(db.node_count(), 1);
            let node = db.get_node(node_id).unwrap();
            assert_eq!(node.id, node_id);
        }

        // Directory should be cleaned up after TempDir is dropped
        assert!(!temp_dir_path.exists());
    }

    #[test]
    fn test_create_test_db_with_config_cleanup() {
        let config = AletheiaDBConfig::builder()
            .wal(WalConfigBuilder::new().num_stripes(8).unwrap().build())
            .build();

        let temp_dir_path;
        {
            let (_temp_dir, db) = create_test_db_with_config(config).unwrap();
            temp_dir_path = _temp_dir.path().to_path_buf();

            assert!(temp_dir_path.exists());

            let _node_id = db
                .create_node("Person", PropertyMapBuilder::new().build())
                .unwrap();
            assert_eq!(db.node_count(), 1);
        }

        assert!(!temp_dir_path.exists());
    }
}
