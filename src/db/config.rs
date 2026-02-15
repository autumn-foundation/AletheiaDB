use crate::api::transaction::{TxIdGenerator, TxVisibilityManager};
use crate::config::AletheiaDBConfig;
use crate::core::id::IdGenerator;
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
use crate::utils::error::Result;
use parking_lot::RwLock;
use std::sync::{Arc, Mutex};

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
        crate::utils::error::Error::other(
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
            crate::storage::index_persistence::operations::load_indexes_startup(
                manager,
                &db.current,
                &db.historical,
                &db.node_id_gen,
                &db.edge_id_gen,
                &db.version_id_gen,
            );
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
    }

    /// Get the default durability mode for this database.
    pub fn default_durability(&self) -> DurabilityMode {
        self.default_durability
    }

    /// Open an existing database from a checkpoint.
    ///
    /// This method loads the most recent checkpoint and restores the database state,
    /// including vector index configuration if it was enabled.
    ///
    /// # Arguments
    ///
    /// * `checkpoint_path` - Path to the checkpoint file to load
    ///
    /// # Returns
    ///
    /// Returns a `AletheiaDB` instance with restored configuration, or an error
    /// if the checkpoint cannot be loaded or the vector index cannot be restored.
    ///
    /// # Example
    ///
    /// ```ignore
    /// use aletheiadb::AletheiaDB;
    /// use std::path::Path;
    ///
    /// let db = AletheiaDB::open(Path::new("aletheiadb/checkpoints/latest.gfry"))?;
    /// ```
    pub fn open<P: AsRef<std::path::Path>>(checkpoint_path: P) -> Result<Self> {
        use crate::storage::persistence::Checkpoint;

        // Load checkpoint
        let checkpoint = Checkpoint::load(checkpoint_path.as_ref())?;

        // Create new database with default config
        let db = Self::new()?;

        // Restore vector index if it was enabled
        if let Some(ref vector_config) = checkpoint.metadata.vector_index_config
            && vector_config.enabled
        {
            db.current
                .enable_vector_index(&vector_config.property_name, vector_config.config.clone())?;
        }

        Ok(db)
    }
}
