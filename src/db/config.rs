//! Database configuration and initialization.
//!
//! Provides AletheiaDB initialization methods, including AletheiaDB::new() and AletheiaDB::with_unified_config().
use crate::api::transaction::TxVisibilityManager;
use crate::config::AletheiaDBConfig;
use crate::core::error::{Result, ResultExt, StorageError};
use crate::core::id::{IdGenerator, TxIdGenerator};
use crate::core::temporal::time;
use crate::core::version::AnchorConfig;
use crate::db::AletheiaDB;
use crate::index::temporal::TemporalIndexes;
use crate::query::planner::Statistics;
use crate::storage::current::CurrentStorage;
use crate::storage::historical::HistoricalStorage;
use crate::storage::index_persistence::tracker::PersistenceTracker;
use crate::storage::index_persistence::worker::spawn_background_persistence_thread;
use crate::storage::redb_cold_storage::{RedbColdStorage, RedbConfig};
use crate::storage::tiered_storage::{TieredStorage, TieredStorageConfig};
use crate::storage::wal::DurabilityMode;
use crate::storage::wal::concurrent_system::{ConcurrentWalSystem, ConcurrentWalSystemConfig};
use parking_lot::RwLock;
use std::sync::{Arc, Mutex};
use std::time::Instant;

fn bootstrap_timestamp(
    current: &CurrentStorage,
    historical: &RwLock<HistoricalStorage>,
) -> crate::core::temporal::Timestamp {
    let mut max_timestamp = time::now();

    for node in current.all_nodes() {
        if let Some(commit_ts) = node.metadata.commit_timestamp
            && commit_ts > max_timestamp
        {
            max_timestamp = commit_ts;
        }
    }

    for edge in current.all_edges() {
        if let Some(commit_ts) = edge.metadata.commit_timestamp
            && commit_ts > max_timestamp
        {
            max_timestamp = commit_ts;
        }
    }

    let historical = historical.read();
    for node_version in historical.get_node_versions().values() {
        let commit_ts = node_version.temporal.transaction_time().start();
        if commit_ts > max_timestamp {
            max_timestamp = commit_ts;
        }
    }

    for edge_version in historical.get_edge_versions().values() {
        let commit_ts = edge_version.temporal.transaction_time().start();
        if commit_ts > max_timestamp {
            max_timestamp = commit_ts;
        }
    }

    max_timestamp
}

fn seed_startup_current_timestamp(db: &AletheiaDB) -> Result<()> {
    let startup_timestamp = bootstrap_timestamp(&db.current, &db.historical);
    let mut current_timestamp = db.current_timestamp.lock().map_err(|_| {
        crate::core::error::Error::Storage(StorageError::LockPoisoned {
            resource: "current_timestamp".to_string(),
        })
    })?;
    *current_timestamp = startup_timestamp;
    Ok(())
}

/// Extract the effective flush interval from a durability mode, falling back to a default.
fn flush_interval_from_durability(mode: DurabilityMode, default_ms: u64) -> u64 {
    match mode {
        DurabilityMode::Async { flush_interval_ms } => flush_interval_ms,
        DurabilityMode::GroupCommit { max_delay_ms, .. } => max_delay_ms,
        DurabilityMode::AsyncBatched { max_delay_ms, .. } => max_delay_ms,
        _ => default_ms,
    }
}

/// Wire temporal indexes into historical storage in a single write-lock acquisition.
fn wire_temporal_indexes(db: &AletheiaDB) {
    let temporal_adjacency_index = Arc::new(
        crate::index::temporal_adjacency::TemporalAdjacencyIndex::new(
            crate::index::temporal_adjacency::TemporalAdjacencyConfig::default(),
        ),
    );

    let mut hist = db.historical.write();
    hist.set_temporal_indexes(Arc::clone(&db.temporal_indexes));
    hist.set_temporal_adjacency_index(temporal_adjacency_index);
}

impl AletheiaDB {
    /// Create a new **ephemeral** in-memory database.
    ///
    /// The WAL is backed by a freshly created temporary directory that is
    /// removed automatically when the returned [`AletheiaDB`] is dropped. No
    /// state survives the process; nothing is loaded from prior runs.
    ///
    /// This is the right constructor for tests, scratch sessions, and quick
    /// experiments. For durable storage that replays prior state on restart,
    /// use [`Self::with_unified_config`] with a config built from
    /// [`crate::config::durable_config_for_data_dir`], or call
    /// [`Self::open_from_env`] to honor the `ALETHEIADB_DATA_DIR` environment
    /// variable.
    ///
    /// # Errors
    ///
    /// Returns an error if the temporary directory cannot be created or WAL
    /// initialization fails inside it.
    #[must_use = "this Result must be used; ignoring errors can lead to silent failures"]
    pub fn new() -> Result<Self> {
        let tempdir = tempfile::Builder::new()
            .prefix("aletheiadb-")
            .tempdir()
            .map_err(crate::core::error::Error::Io)?;
        let wal_config = crate::config::WalConfigBuilder::new()
            .wal_dir(tempdir.path().join("wal"))
            .build();
        let mut db = Self::with_full_config(AnchorConfig::default(), wal_config)?;
        db._tempdir = Some(tempdir);
        Ok(db)
    }

    /// Open a database honouring the `ALETHEIADB_CONFIG` and `ALETHEIADB_DATA_DIR`
    /// environment variables.
    ///
    /// Precedence (first match wins):
    ///
    /// 1. `ALETHEIADB_CONFIG=/path/to/config.toml` — load the full
    ///    [`AletheiaDBConfig`] from TOML. Requires the `config-toml` feature
    ///    (enabled by default); without that feature this returns an error
    ///    when the variable is set.
    /// 2. `ALETHEIADB_DATA_DIR=/path` — open a durable database rooted at
    ///    that path with the canonical config from
    ///    [`crate::config::durable_config_for_data_dir`].
    /// 3. Neither set — fall back to [`Self::new`] (ephemeral, tempdir-backed).
    ///
    /// This is the entry point every exposed binary (HTTP server, MCP server,
    /// CLI, Python SDK) calls so the persistence story is consistent.
    ///
    /// # Errors
    ///
    /// Returns an error if the TOML file cannot be read or parsed, if WAL
    /// initialization fails, or if index loading fails.
    #[must_use = "this Result must be used; ignoring errors can lead to silent failures"]
    pub fn open_from_env() -> Result<Self> {
        if let Some(path) = crate::config::config_path_from_env() {
            return Self::open_from_toml_path(&path);
        }
        if let Some(path) = crate::config::data_dir_from_env() {
            return Self::with_unified_config(crate::config::durable_config_for_data_dir(path));
        }
        Self::new()
    }

    /// Load a TOML config and open the database with it. Used by
    /// [`Self::open_from_env`] when `ALETHEIADB_CONFIG` is set.
    #[cfg(feature = "config-toml")]
    fn open_from_toml_path(path: &std::path::Path) -> Result<Self> {
        let config = AletheiaDBConfig::from_toml_file(path).map_err(|e| {
            crate::core::error::Error::Other(format!(
                "failed to load TOML config from {}: {e}",
                path.display()
            ))
        })?;
        Self::with_unified_config(config)
    }

    #[cfg(not(feature = "config-toml"))]
    fn open_from_toml_path(_path: &std::path::Path) -> Result<Self> {
        Err(crate::core::error::Error::Other(format!(
            "{} is set but the `config-toml` feature is not enabled in this build",
            crate::config::CONFIG_ENV,
        )))
    }

    /// Create a new database with custom anchor configuration.
    ///
    /// # Errors
    ///
    /// Returns an error if WAL initialization fails (e.g., cannot create WAL directory).
    #[must_use = "this Result must be used; ignoring errors can lead to silent failures"]
    pub fn with_config(config: AnchorConfig) -> Result<Self> {
        Self::with_full_config(config, crate::config::WalConfig::default())
    }

    /// Create a new database with custom WAL configuration.
    ///
    /// This allows configuring the durability mode and other WAL settings.
    ///
    /// # Errors
    ///
    /// Returns an error if WAL initialization fails (e.g., cannot create WAL directory).
    ///
    /// # Example
    ///
    /// ```ignore
    /// use aletheiadb::{AletheiaDB, WalConfigBuilder, DurabilityMode};
    ///
    /// // High-throughput ACID mode with group commit
    /// let wal_config = WalConfigBuilder::new()
    ///     .durability_mode(DurabilityMode::group_commit(10, 200))
    ///     .build();
    /// let db = AletheiaDB::with_wal_config(wal_config)?;
    ///
    /// // Bulk loading mode with async durability
    /// let wal_config = WalConfigBuilder::new()
    ///     .durability_mode(DurabilityMode::async_mode(100))
    ///     .build();
    /// let db = AletheiaDB::with_wal_config(wal_config)?;
    /// ```
    #[must_use = "this Result must be used; ignoring errors can lead to silent failures"]
    pub fn with_wal_config(wal_config: crate::config::WalConfig) -> Result<Self> {
        Self::with_full_config(AnchorConfig::default(), wal_config)
    }

    /// Create a new database with unified configuration.
    ///
    /// This method accepts a [`AletheiaDBConfig`] which consolidates all configuration
    /// settings for the database, including WAL, historical storage, and vector indexes.
    ///
    /// # Errors
    ///
    /// Returns an error if WAL initialization fails (e.g., cannot create WAL directory).
    ///
    /// # Example
    ///
    /// ```ignore
    /// use aletheiadb::{AletheiaDB, config::AletheiaDBConfig, config::WalConfigBuilder};
    ///
    /// let config = AletheiaDBConfig::builder()
    ///     .wal(WalConfigBuilder::new()
    ///         .with_validated(32, 2048, 64 * 1024, 64 * 1024 * 1024, 10, 10).unwrap()
    ///         .build())
    ///     .build();
    ///
    /// let db = AletheiaDB::with_unified_config(config)?;
    /// ```
    #[must_use = "this Result must be used; ignoring errors can lead to silent failures"]
    pub fn with_unified_config(config: AletheiaDBConfig) -> Result<Self> {
        let result = (|| {
            let durability_mode = config.wal.durability_mode;

            // Create encryption manager if encryption is enabled
            let encryption_manager = if config.encryption.enabled {
                let manager = crate::encryption::EncryptionManager::from_config(&config.encryption)
                    .map_err(|e| -> crate::core::error::Error {
                        crate::core::error::StorageError::KeyProvider(e.to_string()).into()
                    })?;
                Some(Arc::new(manager))
            } else {
                None
            };

            // Extract WAL cipher from encryption manager (if enabled)
            let wal_cipher = encryption_manager
                .as_ref()
                .map(|mgr| Arc::clone(mgr.wal_cipher()));

            let wal_system_config = ConcurrentWalSystemConfig {
                wal_dir: config.wal.wal_dir,
                num_stripes: config.wal.num_stripes,
                stripe_capacity: config.wal.stripe_capacity,
                segment_size: config.wal.segment_size,
                segments_to_retain: config.wal.segments_to_retain,
                flush_interval_ms: flush_interval_from_durability(
                    durability_mode,
                    config.wal.flush_interval_ms,
                ),
                durability_mode,
                write_buffer_size: config.wal.write_buffer_size,
                wal_cipher,
            };

            let wal = Arc::new(ConcurrentWalSystem::new(wal_system_config)?);

            // Create persistence manager if enabled
            let persistence_manager = if config.persistence.enabled {
                Some(Arc::new(
                    crate::storage::index_persistence::IndexPersistenceManager::new(
                        &config.persistence.data_dir,
                    ),
                ))
            } else {
                None
            };

            // Create persistence tracker if persistence is enabled
            let persistence_tracker = if config.persistence.enabled {
                Some(Arc::new(PersistenceTracker::new()))
            } else {
                None
            };

            // Extract cold storage configuration before config.historical is moved
            let enable_cold_storage = config.historical.enable_cold_storage;
            let cold_storage_path = config.historical.cold_storage_path.clone();

            let mut db = AletheiaDB {
                current: Arc::new(CurrentStorage::new()),
                historical: Arc::new(RwLock::new(HistoricalStorage::from_unified_config(
                    config.historical,
                ))),
                temporal_indexes: Arc::new(TemporalIndexes::new()),
                wal,
                current_timestamp: Arc::new(Mutex::new(time::now())),
                commit_clock_observed_at: Arc::new(Mutex::new(Instant::now())),
                tx_id_gen: Arc::new(TxIdGenerator::new()),
                visibility_manager: Arc::new(TxVisibilityManager::new()),
                node_id_gen: Arc::new(IdGenerator::new()),
                edge_id_gen: Arc::new(IdGenerator::new()),
                version_id_gen: Arc::new(IdGenerator::new()),
                default_durability: durability_mode,
                stats: Arc::new(Statistics::new()),
                persistence_config: config.persistence.clone(),
                persistence_manager: persistence_manager.clone(),
                persistence_tracker: persistence_tracker.clone(),
                persistence_thread_stopped: Arc::new(std::sync::atomic::AtomicBool::new(false)),
                persistence_thread_handle: None,
                encryption_manager: encryption_manager.clone(),
                _tempdir: None,
            };

            // Load indexes on startup if enabled
            if let Some(ref manager) = persistence_manager
                && config.persistence.load_on_startup
            {
                let loaded_lsn =
                    crate::storage::index_persistence::operations::load_indexes_startup(
                        manager,
                        &db.current,
                        &db.historical,
                        &db.node_id_gen,
                        &db.edge_id_gen,
                        &db.version_id_gen,
                    );

                // Initialize tracker LSNs from the loaded manifest
                if let Some(ref tracker) = persistence_tracker
                    && let Some(lsn) = loaded_lsn
                {
                    tracker.set_start_lsn(lsn);
                    tracker.update_last_persisted_counts(
                        db.current.node_count() as u64,
                        db.current.edge_count() as u64,
                    );
                    // Also initialize string count from current state
                    tracker.update_last_persisted_string_count(
                        crate::core::GLOBAL_INTERNER.len() as u64
                    );
                }

                // Replay WAL entries that occurred after the persisted snapshot
                // This ensures no data loss if the WAL is ahead of the indexes (e.g. crash before persist)
                let start_lsn = match loaded_lsn {
                    Some(lsn) => crate::storage::wal::LSN(lsn).next(),
                    None => {
                        // Safety check: if we have data but no LSN, replaying from initial is dangerous
                        // as it might overwrite existing data with old WAL entries or duplicate IDs.
                        if db.current.node_count() > 0 {
                            #[cfg(feature = "observability")]
                            tracing::error!(
                                "Database contains data ({} nodes) but no persistence LSN found. \
                             Skipping WAL replay to prevent potential data corruption. \
                             This may indicate a missing or corrupted manifest file.",
                                db.current.node_count()
                            );
                            #[cfg(not(feature = "observability"))]
                            eprintln!(
                                "ERROR: Database contains data ({} nodes) but no persistence LSN found. \
                             Skipping WAL replay to prevent potential data corruption.",
                                db.current.node_count()
                            );
                            // Skip replay by setting start_lsn to current WAL LSN (effectively no-op)
                            db.wal.current_lsn()
                        } else {
                            crate::storage::wal::LSN::initial()
                        }
                    }
                };

                // Capture initial version ID before replay
                let initial_version_id = db.version_id_gen.current();

                let mut historical_guard = db.historical.write();
                let (_final_lsn, max_node_id, max_edge_id, next_version_id) =
                    crate::storage::recovery::replay_wal_into_storage(
                        &db.wal,
                        &db.current,
                        &mut historical_guard,
                        start_lsn,
                        initial_version_id,
                    )?;
                drop(historical_guard);

                // Update ID generators to account for replayed entities.
                // This ensures that subsequent writes use IDs that don't collide with replayed data.
                if let Some(max_nid) = max_node_id {
                    db.node_id_gen.ensure_at_least(max_nid + 1);
                }
                if let Some(max_eid) = max_edge_id {
                    db.edge_id_gen.ensure_at_least(max_eid + 1);
                }
                db.version_id_gen.ensure_at_least(next_version_id);
            }

            // Start background persistence thread if enabled
            if let Some(ref tracker) = persistence_tracker
                && let Some(ref manager) = persistence_manager
            {
                let handle = spawn_background_persistence_thread(
                    Arc::clone(&db.current),
                    Arc::clone(&db.historical),
                    Arc::clone(&db.temporal_indexes),
                    Arc::clone(&db.wal),
                    Arc::clone(manager),
                    Arc::clone(tracker),
                    config.persistence.policies.clone(),
                    Arc::clone(&db.persistence_thread_stopped),
                );
                db.persistence_thread_handle = Some(handle);
            }

            wire_temporal_indexes(&db);

            // Initialize cold storage if enabled
            if enable_cold_storage && let Some(cold_storage_path) = cold_storage_path {
                // Create Redb cold storage backend, with optional encryption cipher
                let mut cold_storage = RedbColdStorage::new(&cold_storage_path, RedbConfig::new())?;

                if let Some(ref enc_mgr) = encryption_manager {
                    cold_storage = cold_storage.with_cipher(Arc::clone(enc_mgr.cold_cipher()));
                }

                let cold_storage = Arc::new(cold_storage);

                // Create tiered storage with warm cache configuration
                let tiered_config = TieredStorageConfig::default();
                let tiered_storage = TieredStorage::new(tiered_config, cold_storage);

                // Wire tiered storage to historical storage
                // Note: migration_age_threshold and max_hot_versions from config.historical
                // are used by HistoricalStorage's migration logic, not by TieredStorage
                db.historical
                    .write()
                    .set_tiered_storage(Arc::new(tiered_storage));
            }

            seed_startup_current_timestamp(&db)?;

            Ok(db)
        })();
        result.record_error_metric()
    }

    /// Create a new database with both anchor and WAL configuration.
    ///
    /// This maintains backward compatibility with the old API.
    /// For new code, prefer using [`with_unified_config`](Self::with_unified_config).
    ///
    /// # Errors
    ///
    /// Returns an error if WAL initialization fails (e.g., cannot create WAL directory).
    #[must_use = "this Result must be used; ignoring errors can lead to silent failures"]
    pub fn with_full_config(
        anchor_config: AnchorConfig,
        wal_config: crate::config::WalConfig,
    ) -> Result<Self> {
        let result = (|| {
            let durability_mode = wal_config.durability_mode;

            let wal_system_config = ConcurrentWalSystemConfig {
                wal_dir: wal_config.wal_dir,
                num_stripes: wal_config.num_stripes,
                stripe_capacity: wal_config.stripe_capacity,
                segment_size: wal_config.segment_size,
                segments_to_retain: wal_config.segments_to_retain,
                flush_interval_ms: flush_interval_from_durability(
                    durability_mode,
                    wal_config.flush_interval_ms,
                ),
                durability_mode,
                write_buffer_size: wal_config.write_buffer_size,
                wal_cipher: None,
            };

            let wal = Arc::new(ConcurrentWalSystem::new(wal_system_config)?);

            let db = AletheiaDB {
                current: Arc::new(CurrentStorage::new()),
                historical: Arc::new(RwLock::new(HistoricalStorage::with_config(anchor_config))),
                temporal_indexes: Arc::new(TemporalIndexes::new()),
                wal,
                current_timestamp: Arc::new(Mutex::new(time::now())),
                commit_clock_observed_at: Arc::new(Mutex::new(Instant::now())),
                tx_id_gen: Arc::new(TxIdGenerator::new()),
                visibility_manager: Arc::new(TxVisibilityManager::new()),
                node_id_gen: Arc::new(IdGenerator::new()),
                edge_id_gen: Arc::new(IdGenerator::new()),
                version_id_gen: Arc::new(IdGenerator::new()),
                default_durability: durability_mode,
                stats: Arc::new(Statistics::new()),
                persistence_config: crate::storage::index_persistence::PersistenceConfig::default(),
                persistence_manager: None,
                persistence_tracker: None,
                persistence_thread_stopped: Arc::new(std::sync::atomic::AtomicBool::new(false)),
                persistence_thread_handle: None,
                encryption_manager: None,
                _tempdir: None,
            };
            seed_startup_current_timestamp(&db)?;
            wire_temporal_indexes(&db);

            Ok(db)
        })();
        result.record_error_metric()
    }

    /// Get the default durability mode for this database.
    pub fn default_durability(&self) -> DurabilityMode {
        self.default_durability
    }

    /// Returns `true` if encryption at rest is enabled.
    pub fn is_encryption_enabled(&self) -> bool {
        self.encryption_manager.is_some()
    }

    /// Get a reference to the encryption manager, if encryption is enabled.
    pub fn encryption_manager(&self) -> Option<&Arc<crate::encryption::EncryptionManager>> {
        self.encryption_manager.as_ref()
    }
}

#[cfg(test)]
mod ephemeral_tests {
    use super::*;

    #[test]
    fn new_uses_a_unique_tempdir_per_call() {
        let db1 = AletheiaDB::new().expect("new should succeed");
        let db2 = AletheiaDB::new().expect("new should succeed");
        let dir1 = db1
            ._tempdir
            .as_ref()
            .expect("new should attach a tempdir")
            .path()
            .to_path_buf();
        let dir2 = db2
            ._tempdir
            .as_ref()
            .expect("new should attach a tempdir")
            .path()
            .to_path_buf();
        assert_ne!(dir1, dir2, "each new() must own a distinct tempdir");
        assert!(
            dir1.exists(),
            "tempdir must exist while the database is alive"
        );
        assert!(dir2.exists());
    }

    #[test]
    fn new_does_not_write_into_cwd() {
        let cwd = std::env::current_dir().expect("cwd");
        let db = AletheiaDB::new().expect("new should succeed");
        let wal_dir = db._tempdir.as_ref().expect("tempdir attached").path();
        assert!(
            !wal_dir.starts_with(&cwd) || wal_dir.starts_with(std::env::temp_dir()),
            "tempdir should live under the system temp root, not the cwd"
        );
    }

    #[test]
    fn tempdir_is_removed_when_db_drops() {
        let path = {
            let db = AletheiaDB::new().expect("new should succeed");
            db._tempdir.as_ref().unwrap().path().to_path_buf()
        };
        assert!(!path.exists(), "tempdir should be cleaned up on drop");
    }

    #[test]
    #[serial_test::serial]
    fn open_from_env_with_unset_var_is_ephemeral() {
        // SAFETY: env access is single-threaded inside this serial test.
        // remove_var is unsafe under Rust edition 2024.
        unsafe {
            std::env::remove_var(crate::config::DATA_DIR_ENV);
            std::env::remove_var(crate::config::CONFIG_ENV);
        }
        let db = AletheiaDB::open_from_env().expect("open_from_env should fall back to new()");
        assert!(
            db._tempdir.is_some(),
            "unset env must yield a tempdir-backed db"
        );
    }

    #[cfg(feature = "config-toml")]
    #[test]
    #[serial_test::serial]
    fn open_from_env_prefers_config_over_data_dir() {
        let scratch = tempfile::tempdir().expect("scratch tempdir");
        let config_path = scratch.path().join("config.toml");
        let toml_data_dir = scratch.path().join("toml-data");
        let env_data_dir = scratch.path().join("env-data");

        // Minimal valid TOML pointing the WAL inside `toml-data/wal`.
        // Use a TOML literal string (single quotes) so Windows backslashes
        // in the path don't trigger basic-string escape processing.
        std::fs::write(
            &config_path,
            format!(
                "[wal]\nwal_dir = '{}'\n",
                toml_data_dir.join("wal").display()
            ),
        )
        .unwrap();

        // SAFETY: serial test, single-threaded env access; required by edition 2024.
        unsafe {
            std::env::set_var(crate::config::CONFIG_ENV, &config_path);
            std::env::set_var(crate::config::DATA_DIR_ENV, &env_data_dir);
        }

        let db = AletheiaDB::open_from_env().expect("config TOML should load");
        assert!(
            db._tempdir.is_none(),
            "TOML-backed db is durable, not tempdir"
        );
        assert!(
            toml_data_dir.join("wal").exists(),
            "TOML's wal_dir must win over DATA_DIR"
        );
        assert!(
            !env_data_dir.exists(),
            "DATA_DIR should be ignored when CONFIG is set"
        );

        // SAFETY: see above.
        unsafe {
            std::env::remove_var(crate::config::CONFIG_ENV);
            std::env::remove_var(crate::config::DATA_DIR_ENV);
        }
    }
}
