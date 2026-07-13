//! Test utilities for AletheiaDB.
//!
//! This module provides helpers for writing tests that don't leak resources.

use crate::AletheiaDB;
use crate::config::{AletheiaDBConfig, WalConfigBuilder};
use crate::core::error::Result;
use crate::storage::index_persistence::PersistenceConfig;
use crate::storage::wal::DurabilityMode;
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
    let temp_dir = tempfile::tempdir().map_err(crate::core::error::Error::Io)?;

    let wal_dir = temp_dir.path().join("wal");
    let data_dir = temp_dir.path().join("data");

    let config = AletheiaDBConfig::builder()
        .wal(
            WalConfigBuilder::new()
                .wal_dir(wal_dir)
                .durability_mode(DurabilityMode::Async {
                    flush_interval_ms: 10,
                })
                .build(),
        )
        .persistence(PersistenceConfig {
            enabled: true,
            data_dir,
            load_on_startup: false,
            ..Default::default()
        })
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
    let temp_dir = tempfile::tempdir().map_err(crate::core::error::Error::Io)?;

    // Override storage directories to keep all test artifacts isolated.
    config.wal.wal_dir = temp_dir.path().join("wal");
    config.persistence.data_dir = temp_dir.path().join("data");
    // Avoid loading stale indexes in helpers meant for fresh test databases.
    config.persistence.load_on_startup = false;

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

/// Return a fresh, process-unique temporary directory to use as an isolated WAL
/// directory in a test that builds its own [`AletheiaDBConfig`].
///
/// # Why this exists (Issue #3500)
///
/// The default `WalConfig::wal_dir` is the **CWD-relative** path
/// `"aletheiadb/wal"` (`src/config.rs`). Every durable-config test that forgets
/// to set an explicit `wal_dir` therefore writes into the SAME `./aletheiadb/wal`
/// directory within a single `cargo test` process. Because DB startup eagerly
/// decodes the ENTIRE WAL directory, a stray segment left behind by one test is
/// then read by a later test's startup and can hard-error with
/// `CorruptedData("Unknown WAL operation type: ...")`. That makes it an
/// order-dependent, cross-pollinating flake rather than a deterministic failure.
///
/// Recovery-style tests that reopen the same `wal_dir` across DB drops cannot use
/// [`create_test_db`] (which returns a live DB and owns its own tempdir), so they
/// build their own config. `unique_wal_dir()` gives those tests a one-liner for an
/// isolated directory: each call returns a DISTINCT OS temp dir, so two tests can
/// never share a WAL directory.
///
/// ```ignore
/// use aletheiadb::test_utils::unique_wal_dir;
/// use aletheiadb::config::{AletheiaDBConfig, WalConfigBuilder};
///
/// let wal_home = unique_wal_dir();
/// let config = AletheiaDBConfig::builder()
///     .wal(WalConfigBuilder::new().wal_dir(wal_home.path().join("wal")).build())
///     .build();
/// // ... keep `wal_home` in scope for the lifetime of the DB(s) using it ...
/// ```
///
/// # Note
///
/// **You MUST keep the returned `TempDir` in scope** for as long as any database
/// uses it; dropping it deletes the directory (mirroring [`create_test_db`]).
///
/// # Panics
///
/// Panics if the OS temporary directory cannot be created. This is acceptable in
/// test code and consistent with `tempfile::tempdir()` usage across the suite.
pub fn unique_wal_dir() -> tempfile::TempDir {
    tempfile::tempdir().expect("failed to create unique temp dir for WAL isolation")
}

/// Return a fresh, process-unique temporary directory to use as an isolated
/// persistence/data directory in a test that builds its own config.
///
/// Analogous to [`unique_wal_dir`]; see its docs for the CWD-relative-default
/// cross-pollution rationale (Issue #3500). Provided as a companion so a test
/// that isolates both a `wal_dir` and a `data_dir` reads clearly at the call
/// site. Like [`unique_wal_dir`], each call returns a DISTINCT directory that
/// **must be kept in scope** for the lifetime of the database.
///
/// # Panics
///
/// Panics if the OS temporary directory cannot be created.
pub fn unique_data_dir() -> tempfile::TempDir {
    tempfile::tempdir().expect("failed to create unique temp dir for data isolation")
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

    /// T1: `unique_wal_dir()` returns DISTINCT paths across two calls, each an
    /// existing empty directory.
    #[test]
    fn test_unique_wal_dir_returns_distinct_existing_dirs() {
        let a = unique_wal_dir();
        let b = unique_wal_dir();

        assert!(a.path().exists(), "first unique_wal_dir should exist");
        assert!(b.path().exists(), "second unique_wal_dir should exist");
        assert_ne!(
            a.path(),
            b.path(),
            "two unique_wal_dir() calls must return distinct paths"
        );

        // Each is a fresh, empty directory.
        assert_eq!(
            std::fs::read_dir(a.path()).unwrap().count(),
            0,
            "first unique_wal_dir should start empty"
        );
        assert_eq!(
            std::fs::read_dir(b.path()).unwrap().count(),
            0,
            "second unique_wal_dir should start empty"
        );

        // `unique_data_dir()` behaves the same and is distinct from the wal dirs.
        let d = unique_data_dir();
        assert!(d.path().exists());
        assert_ne!(d.path(), a.path());
        assert_ne!(d.path(), b.path());
    }

    /// T2: two databases built with `unique_wal_dir()` do NOT cross-pollute.
    ///
    /// DB A writes into its own isolated WAL dir and drops; DB B writes into a
    /// DIFFERENT isolated WAL dir, drops, and is reopened via WAL replay. B must
    /// see only its own data and must NOT hard-error decoding A's segments
    /// (the `CorruptedData("Unknown WAL operation type")` cross-pollution
    /// symptom from Issue #3500).
    #[test]
    fn test_unique_wal_dir_prevents_cross_pollution() {
        fn durable_config(home: &std::path::Path) -> AletheiaDBConfig {
            AletheiaDBConfig::builder()
                .wal(WalConfigBuilder::new().wal_dir(home.join("wal")).build())
                .persistence(PersistenceConfig {
                    enabled: true,
                    data_dir: home.join("data"),
                    load_on_startup: true,
                    ..Default::default()
                })
                .build()
        }

        // DB A: write 3 nodes into its own isolated WAL dir, then drop.
        let home_a = unique_wal_dir();
        {
            let db = AletheiaDB::with_unified_config(durable_config(home_a.path())).unwrap();
            for _ in 0..3 {
                db.create_node("A", PropertyMapBuilder::new().build())
                    .unwrap();
            }
        }

        // DB B: write 5 nodes into a DIFFERENT isolated WAL dir, then drop.
        let home_b = unique_wal_dir();
        {
            let db = AletheiaDB::with_unified_config(durable_config(home_b.path())).unwrap();
            for _ in 0..5 {
                db.create_node("B", PropertyMapBuilder::new().build())
                    .unwrap();
            }
        }

        // Force B to reconstruct from its WAL by removing its persisted indexes,
        // then reopen. With isolated dirs this decodes ONLY B's segments.
        std::fs::remove_dir_all(home_b.path().join("data").join("indexes")).ok();
        let db_b = AletheiaDB::with_unified_config(durable_config(home_b.path()))
            .expect("reopening B must not decode A's segments (no CorruptedData)");

        assert_eq!(
            db_b.node_count(),
            5,
            "DB B must see only its own 5 nodes, never A's segments"
        );
    }
}
