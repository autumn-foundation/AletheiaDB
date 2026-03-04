use crate::api::transaction::TxVisibilityManager;
use crate::config::AletheiaDBConfig;
use crate::core::error::{Result, ResultExt};
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
        crate::core::error::Error::other(
            "failed to seed startup current_timestamp due to lock poisoning",
        )
    })?;
    *current_timestamp = startup_timestamp;
    Ok(())
}

impl AletheiaDB {
    /// Create a new empty database with default configuration.
    ///
    /// # Configuration
    ///
    /// This creates a **disk-based** database with:
    /// - **WAL directory**: `./aletheiadb/wal` (relative to current working directory)
    /// - **Durability**: Group Commit (ACID compliant)
    /// - **History**: Anchor interval 10
    ///
    /// To use a different path or in-memory storage (for testing), use [`with_unified_config`](Self::with_unified_config).
    ///
    /// # Errors
    ///
    /// Returns an error if WAL initialization fails (e.g., cannot create WAL directory).
    pub fn new() -> Result<Self> {
        Self::with_config(AnchorConfig::default())
    }

    /// Create a new database with custom anchor configuration.
    ///
    /// # Errors
    ///
    /// Returns an error if WAL initialization fails (e.g., cannot create WAL directory).
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
    pub fn with_unified_config(config: AletheiaDBConfig) -> Result<Self> {
        let result = (|| {
            let durability_mode = config.wal.durability_mode;

            // Create ConcurrentWalSystem config from unified WalConfig
            let wal_system_config = ConcurrentWalSystemConfig {
                wal_dir: config.wal.wal_dir,
                num_stripes: config.wal.num_stripes,
                stripe_capacity: config.wal.stripe_capacity,
                segment_size: config.wal.segment_size,
                segments_to_retain: config.wal.segments_to_retain,
                flush_interval_ms: match durability_mode {
                    DurabilityMode::Async { flush_interval_ms } => flush_interval_ms,
                    DurabilityMode::GroupCommit { max_delay_ms, .. } => max_delay_ms,
                    DurabilityMode::AsyncBatched { max_delay_ms, .. } => max_delay_ms,
                    _ => config.wal.flush_interval_ms, // Use config default
                },
                durability_mode,
                write_buffer_size: config.wal.write_buffer_size,
            };

            let wal = ConcurrentWalSystem::new(wal_system_config)?;
            let wal = Arc::new(wal);

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

            // Wire temporal indexes to historical storage for O(log n) version lookups (Issue #209)
            db.historical
                .write()
                .set_temporal_indexes(Arc::clone(&db.temporal_indexes));

            // Initialize and enable temporal adjacency index for efficient temporal pathfinding
            let temporal_adjacency_index = Arc::new(
                crate::index::temporal_adjacency::TemporalAdjacencyIndex::new(
                    crate::index::temporal_adjacency::TemporalAdjacencyConfig::default(),
                ),
            );
            db.historical
                .write()
                .set_temporal_adjacency_index(temporal_adjacency_index);

            // Initialize cold storage if enabled
            if enable_cold_storage && let Some(cold_storage_path) = cold_storage_path {
                // Create Redb cold storage backend
                let cold_storage =
                    Arc::new(RedbColdStorage::new(&cold_storage_path, RedbConfig::new())?);

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
    pub fn with_full_config(
        anchor_config: AnchorConfig,
        wal_config: crate::config::WalConfig,
    ) -> Result<Self> {
        let result = (|| {
            let durability_mode = wal_config.durability_mode;

            // Create ConcurrentWalSystem config from unified WalConfig
            let wal_system_config = ConcurrentWalSystemConfig {
                wal_dir: wal_config.wal_dir,
                num_stripes: wal_config.num_stripes,
                stripe_capacity: wal_config.stripe_capacity,
                segment_size: wal_config.segment_size,
                segments_to_retain: wal_config.segments_to_retain,
                flush_interval_ms: match durability_mode {
                    DurabilityMode::Async { flush_interval_ms } => flush_interval_ms,
                    DurabilityMode::GroupCommit { max_delay_ms, .. } => max_delay_ms,
                    DurabilityMode::AsyncBatched { max_delay_ms, .. } => max_delay_ms,
                    _ => wal_config.flush_interval_ms,
                },
                durability_mode,
                write_buffer_size: wal_config.write_buffer_size,
            };

            let wal = ConcurrentWalSystem::new(wal_system_config)?;
            let wal = Arc::new(wal);

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
            };
            seed_startup_current_timestamp(&db)?;

            // Wire temporal indexes to historical storage for O(log n) version lookups (Issue #209)
            db.historical
                .write()
                .set_temporal_indexes(Arc::clone(&db.temporal_indexes));

            // Initialize and enable temporal adjacency index for efficient temporal pathfinding
            let temporal_adjacency_index = Arc::new(
                crate::index::temporal_adjacency::TemporalAdjacencyIndex::new(
                    crate::index::temporal_adjacency::TemporalAdjacencyConfig::default(),
                ),
            );
            db.historical
                .write()
                .set_temporal_adjacency_index(temporal_adjacency_index);

            Ok(db)
        })();
        result.record_error_metric()
    }

    /// Get the default durability mode for this database.
    pub fn default_durability(&self) -> DurabilityMode {
        self.default_durability
    }
}
