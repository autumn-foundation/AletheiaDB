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

    for node in current.iter_nodes() {
        if let Some(commit_ts) = node.metadata.commit_timestamp
            && commit_ts > max_timestamp
        {
            max_timestamp = commit_ts;
        }
    }

    for edge in current.iter_edges() {
        if let Some(commit_ts) = edge.metadata.commit_timestamp
            && commit_ts > max_timestamp
        {
            max_timestamp = commit_ts;
        }
    }

    let historical = historical.read();
    for node_version in historical.get_node_versions().values() {
        let tx_time = node_version.temporal.transaction_time();
        if tx_time.start() > max_timestamp {
            max_timestamp = tx_time.start();
        }
        // Issue #3387: restored versions carry CLOSED tx ends too. A closure
        // stamped by a superseding version that was since cold-migrated may
        // exceed every restored tx start; fold it in so the HLC seed stays
        // monotonic under clock skew.
        if !tx_time.is_current() && tx_time.end() > max_timestamp {
            max_timestamp = tx_time.end();
        }
    }

    for edge_version in historical.get_edge_versions().values() {
        let tx_time = edge_version.temporal.transaction_time();
        if tx_time.start() > max_timestamp {
            max_timestamp = tx_time.start();
        }
        if !tx_time.is_current() && tx_time.end() > max_timestamp {
            max_timestamp = tx_time.end();
        }
    }

    max_timestamp
}

pub(crate) fn seed_startup_current_timestamp(db: &AletheiaDB) -> Result<()> {
    let startup_timestamp = bootstrap_timestamp(&db.current, &db.historical);
    let mut current_timestamp = db.current_timestamp.lock().map_err(|_| {
        crate::core::error::Error::Storage(StorageError::LockPoisoned {
            resource: "current_timestamp".to_string(),
        })
    })?;
    *current_timestamp = startup_timestamp;
    Ok(())
}

/// Seed the WAL LSN allocator from durable state at startup (Issue #3420).
///
/// Scans the WAL directory for the maximum LSN present in existing segments
/// and moves the allocator to `max + 1` (never backwards). Must run after WAL
/// construction and **before any write is accepted**; otherwise a restarted
/// process starts allocating at LSN 1 again, producing duplicate LSNs across
/// segments and writes that land below the index manifest LSN — which the
/// next startup's differential replay then silently skips.
///
/// Seeding policy lives here (in the database startup path), not inside the
/// WAL constructor, so WAL-crate users keep full control over allocator state.
fn seed_lsn_allocator_from_segments(
    wal: &ConcurrentWalSystem,
    cipher: Option<&Arc<dyn crate::encryption::cipher::Cipher>>,
) -> Result<()> {
    let max_lsn = crate::storage::wal::segment_reader::max_lsn_in_dir(wal.wal_dir(), cipher)?;
    seed_lsn_allocator_from_max(wal, max_lsn);
    Ok(())
}

/// Move the allocator to `max_lsn + 1` (never backwards), given a max LSN that
/// the caller has already determined (Issue #3429).
///
/// Split out of [`seed_lsn_allocator_from_segments`] so the startup path can
/// seed from the max LSN of the single full WAL read it performs, instead of
/// scanning the segment directory a second time purely to compute it. See
/// [`seed_lsn_allocator_from_segments`] for the seeding-policy rationale.
fn seed_lsn_allocator_from_max(
    wal: &ConcurrentWalSystem,
    max_lsn: Option<crate::storage::wal::LSN>,
) {
    if let Some(max_lsn) = max_lsn {
        let next = crate::storage::wal::LSN(max_lsn.0.saturating_add(1));
        if next > wal.current_lsn() {
            wal.set_next_lsn(next);
        }
    }
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
    // Populate the temporal index from any versions already in historical storage
    // (loaded from index persistence + WAL replay). Without this, temporal
    // point-in-time queries would silently miss those versions.
    hist.rebuild_temporal_index_from_versions();
}

impl AletheiaDB {
    /// Create a new **ephemeral** in-memory database.
    ///
    /// The WAL is backed by a freshly created temporary directory that is
    /// removed automatically when the returned [`AletheiaDB`] is dropped. No
    /// state survives the process; nothing is loaded from prior runs.
    ///
    /// This is the right constructor for tests, scratch sessions, and quick
    /// experiments. For durable storage that persists across restarts, use
    /// [`Self::open`] — the one-line durable counterpart to this
    /// constructor. Power users needing full control can call
    /// [`Self::with_unified_config`] with a config built from
    /// [`crate::config::durable_config_for_data_dir`], or
    /// [`Self::open_from_env`] to honor the `ALETHEIADB_DATA_DIR`
    /// environment variable.
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
    ///    that path via [`Self::open`].
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
            return Self::open(path);
        }
        Self::new()
    }

    /// Open (or create) a **durable** database rooted at `path`.
    ///
    /// This is the one-line entry point for embedding a durable AletheiaDB:
    /// it creates the directory tree at `path` if absent, and opens an
    /// existing one otherwise, replaying any prior state so calls are
    /// idempotent across process restarts. Internally it is exactly
    /// [`Self::with_unified_config`] with a config built by
    /// [`crate::config::durable_config_for_data_dir`] — WAL + index
    /// persistence with `load_on_startup`, group-commit durability — so
    /// behavior stays in one canonical place and does not fork config
    /// defaults. For an ephemeral, tempdir-backed database, use
    /// [`Self::new`] instead.
    ///
    /// # Errors
    ///
    /// Returns an error if `path` is not writable, WAL initialization
    /// fails, or index loading fails. Never falls back to an ephemeral
    /// database on failure.
    ///
    /// # Example
    ///
    /// ```rust
    /// use aletheiadb::{AletheiaDB, PropertyMapBuilder};
    ///
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// let dir = tempfile::tempdir()?;
    ///
    /// let node_id = {
    ///     let db = AletheiaDB::open(dir.path())?;
    ///     db.create_node(
    ///         "Person",
    ///         PropertyMapBuilder::new().insert("name", "Alice").build(),
    ///     )?
    ///     // `db` drops here, persisting final state.
    /// };
    ///
    /// // Reopening the same path replays the prior state.
    /// let db = AletheiaDB::open(dir.path())?;
    /// let node = db.get_node(node_id)?;
    /// assert_eq!(
    ///     node.properties.get("name").and_then(|v| v.as_str()),
    ///     Some("Alice")
    /// );
    /// # Ok(())
    /// # }
    /// ```
    #[must_use = "this Result must be used; ignoring errors can lead to silent failures"]
    pub fn open(path: impl AsRef<std::path::Path>) -> Result<Self> {
        Self::with_unified_config(crate::config::durable_config_for_data_dir(
            path.as_ref().to_path_buf(),
        ))
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

            // Capture provenance-chain settings before `config.wal.wal_dir` is
            // moved into the WAL system config. The chain lives under the data
            // dir (the parent of the WAL dir) unless an explicit override is set.
            let chain_config = config.chain.clone();
            let chain_data_dir: std::path::PathBuf = config
                .wal
                .wal_dir
                .parent()
                .map(std::path::Path::to_path_buf)
                .unwrap_or_else(|| std::path::PathBuf::from("."));

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
                wal_cipher: wal_cipher.clone(),
                tolerate_torn_tail: config.wal.tolerate_torn_tail,
            };

            let wal = Arc::new(ConcurrentWalSystem::new(wal_system_config)?);

            // Issue #3429: decode the WAL exactly once here and reuse that
            // single decode for all three startup passes that historically each
            // re-read every segment: (1) the LSN-allocator seed below, (2) the
            // constraint declaration net-state, and (3) the differential
            // node/edge replay. `read_from` mirrors the recovery reader's cipher
            // behavior (plaintext today), so for an encrypted WAL its max LSN
            // would omit encrypted entries; the cipher-aware directory scan is
            // used as a targeted fallback below only when a cipher is
            // configured, preserving the #3420/#3430 encrypted seed exactly.
            //
            // Held (as `mut`, consumed by whichever replay branch runs) across
            // index load so no later pass re-reads the segment directory.
            let mut startup_wal_entries = wal.read_from(crate::storage::wal::LSN::initial())?;

            // Issue #3420: seed the LSN allocator past every LSN already durable
            // in existing WAL segments, BEFORE any write is accepted. Without
            // this, a restarted process re-allocates LSNs starting at 1,
            // breaking LSN total ordering across segments and placing new
            // writes below the index manifest LSN (so the next startup's
            // differential replay silently skips them).
            let seed_max_lsn = if wal_cipher.is_some() {
                // Encrypted segments are invisible to the cipher-less read
                // above; fall back to the cipher-aware directory scan so the
                // seed still accounts for encrypted entries (unchanged #3420
                // behavior for encrypted WALs; the constraint/replay passes
                // stay cipher-less exactly as before, per #3430).
                crate::storage::wal::segment_reader::max_lsn_in_dir(
                    wal.wal_dir(),
                    wal_cipher.as_ref(),
                )?
            } else {
                // `read_from` returns entries globally sorted by LSN
                // (segment_reader: `entries.sort_by_key(|e| e.lsn)`), so the
                // last element carries the max LSN in O(1). Empty WAL -> `None`
                // -> the seed is a no-op.
                startup_wal_entries.last().map(|e| e.lsn)
            };
            seed_lsn_allocator_from_max(&wal, seed_max_lsn);

            // Create persistence manager if enabled. When encryption is
            // enabled, thread the index cipher through so index files are
            // encrypted at rest (Issue #481); mirrors the WAL cipher wiring
            // above. Encryption disabled => None => plaintext, unchanged.
            let persistence_manager = if config.persistence.enabled {
                let index_cipher = encryption_manager
                    .as_ref()
                    .map(|mgr| Arc::clone(mgr.index_cipher()));
                Some(Arc::new(
                    crate::storage::index_persistence::IndexPersistenceManager::with_cipher(
                        &config.persistence.data_dir,
                        index_cipher,
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
                constraint_registry: Arc::new(crate::core::constraint::ConstraintRegistry::new()),
                lineage: Arc::new(crate::core::lineage::LineageStore::new()),
                chain: None,
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

                // Issue #3420: the manifest LSN is a second durability floor for
                // the allocator. Normally the segment scan above already seeded
                // the allocator higher, but if the WAL was truncated below the
                // manifest LSN (e.g. LSN-based truncation after cold-storage
                // migration), the segments alone under-seed it. The manifest
                // stores the next-to-allocate LSN captured at snapshot time
                // (see `IndexManifest::lsn`), so it is itself a valid "next".
                if let Some(lsn) = loaded_lsn {
                    let manifest_floor = crate::storage::wal::LSN(lsn);
                    if manifest_floor > db.wal.current_lsn() {
                        db.wal.set_next_lsn(manifest_floor);
                    }
                }

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

                // Replay constraint declarations from the full WAL history before the
                // differential node/edge replay.  Constraint WAL entries may predate
                // the snapshot LSN and would be skipped by the differential replay.
                //
                // Issue #3429: fold the constraint net-state out of the single
                // full read taken at startup (`startup_wal_entries` spans the
                // whole history from LSN 0), so this no longer re-reads the
                // segment directory. Below-manifest declarations are still
                // applied because the borrowed slice includes them.
                crate::storage::recovery::apply_constraint_declarations(
                    &startup_wal_entries,
                    &db.constraint_registry,
                );

                // Replay WAL entries that occurred after the persisted snapshot
                // This ensures no data loss if the WAL is ahead of the indexes (e.g. crash before persist)
                //
                // Issue #3419: the manifest LSN is the NEXT-to-allocate LSN
                // captured *before* the snapshot was taken (see
                // `IndexManifest::lsn`). Entries with LSN < manifest.lsn are
                // guaranteed to be in the snapshot; entries with LSN >=
                // manifest.lsn may or may not be. Replay therefore starts AT
                // the manifest LSN (inclusive) — the previous `.next()` here
                // skipped the first post-persist write entirely — and the
                // replay itself is idempotent for already-applied entries
                // (see the re-application guards in
                // `replay_wal_into_storage_with_constraints`).
                let start_lsn = match loaded_lsn {
                    Some(lsn) => crate::storage::wal::LSN(lsn),
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

                // Issue #3429: feed the differential replay the suffix of the
                // single startup read (entries with LSN >= start_lsn), rather
                // than re-reading the segment directory a third time.
                //
                // CORRECTNESS: `read_from` returns entries globally sorted by
                // LSN (segment_reader: `entries.sort_by_key(|e| e.lsn)`), so
                // `partition_point` finds the first index with LSN >= start_lsn
                // and `drain(..idx)` discards exactly the entries below it — a
                // contiguous, in-place split with no extra allocation. This is
                // byte-identical to what `read_from(start_lsn)` would have
                // produced (the same LSN-ordered tail the framing resolver
                // expects). If the sort is ever removed upstream, this
                // partition (and the O(1) seed above) breaks — keep them
                // together.
                let split = startup_wal_entries.partition_point(|entry| entry.lsn < start_lsn);
                startup_wal_entries.drain(..split);
                let replay_entries = std::mem::take(&mut startup_wal_entries);

                let mut historical_guard = db.historical.write();
                let (_final_lsn, max_node_id, max_edge_id, next_version_id) =
                    crate::storage::recovery::replay_entries_into_storage_with_constraints(
                        &db.wal,
                        replay_entries,
                        &db.current,
                        &mut historical_guard,
                        initial_version_id,
                        Some(&db.constraint_registry),
                    )?;
                drop(historical_guard);

                // Rebuild reservation index from currently-valid nodes for each declared constraint.
                for (label_str, property_str) in db.constraint_registry.list() {
                    if let (Some(label_id), Some(property_id)) = (
                        crate::core::interning::GLOBAL_INTERNER.get_id(&label_str),
                        crate::core::interning::GLOBAL_INTERNER.get_id(&property_str),
                    ) {
                        let nodes = db.current.get_nodes_by_label(&label_str);
                        db.constraint_registry
                            .rebuild_from_nodes(&nodes, label_id, property_id);
                    }
                }

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

            // When only the WAL is configured (no index persistence), replay the full
            // WAL from the beginning to restore node/edge data AND constraint declarations.
            if persistence_manager.is_none() {
                let initial_version_id = db.version_id_gen.current();

                // Issue #3429: replay directly from the single startup read
                // instead of re-decoding the segment directory. This branch
                // replays from LSN 0, so it consumes every entry; the inline
                // constraint arms in the replay loop recover constraint state
                // (no separate constraint pass is needed here). `std::mem::take`
                // moves the entries out of the (persistence-branch-shared)
                // binding; only one of the two branches runs.
                let replay_entries = std::mem::take(&mut startup_wal_entries);

                let mut historical_guard = db.historical.write();
                let (_final_lsn, max_node_id, max_edge_id, next_version_id) =
                    crate::storage::recovery::replay_entries_into_storage_with_constraints(
                        &db.wal,
                        replay_entries,
                        &db.current,
                        &mut historical_guard,
                        initial_version_id,
                        Some(&db.constraint_registry),
                    )?;
                drop(historical_guard);

                // Rebuild reservation index.
                for (label_str, property_str) in db.constraint_registry.list() {
                    if let (Some(label_id), Some(property_id)) = (
                        crate::core::interning::GLOBAL_INTERNER.get_id(&label_str),
                        crate::core::interning::GLOBAL_INTERNER.get_id(&property_str),
                    ) {
                        let nodes = db.current.get_nodes_by_label(&label_str);
                        db.constraint_registry
                            .rebuild_from_nodes(&nodes, label_id, property_id);
                    }
                }

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

                // Merge the cold tier's persisted extent bounds into the
                // temporal index so `temporal_extent` spans history migrated to
                // cold before this restart (Issue #3389). `wire_temporal_indexes`
                // above rebuilt the extent aggregate from the hot tier only;
                // absent/empty cold metadata leaves it untouched, and merging
                // only ever widens (never narrows) the reported extent.
                if let Some(bounds) = tiered_storage.cold_storage().get_temporal_extent_bounds()? {
                    db.temporal_indexes.merge_extent_bounds(
                        bounds.valid_earliest,
                        bounds.valid_latest,
                        bounds.tx_earliest,
                        bounds.tx_latest,
                    );
                }

                // Wire tiered storage to historical storage
                // Note: migration_age_threshold and max_hot_versions from config.historical
                // are used by HistoricalStorage's migration logic, not by TieredStorage
                db.historical
                    .write()
                    .set_tiered_storage(Arc::new(tiered_storage));
            }

            seed_startup_current_timestamp(&db)?;

            // Wire the opt-in provenance hash chain (Issue #3351). Constructed
            // after WAL replay + state restore so the genesis anchors the
            // post-recovery LSN and the tail rebuild can fold every restored
            // transaction. Nothing here runs (and no chain dir is created) when
            // the chain is disabled — the default — preserving byte-identical
            // behavior.
            if chain_config.enabled {
                let source: Arc<dyn crate::provenance_chain::VersionSource + Send + Sync> =
                    Arc::new(crate::db::chain_source::DbVersionSource::new(Arc::clone(
                        &db.historical,
                    )));
                let genesis_lsn = db.wal.current_lsn().0;
                let genesis_ts = time::now().wallclock();
                let chain = crate::provenance_chain::ProvenanceChain::open(
                    &chain_config,
                    &chain_data_dir,
                    genesis_lsn,
                    genesis_ts,
                    source,
                )
                .map_err(|e| {
                    crate::core::error::Error::Other(format!(
                        "provenance hash chain open failed: {e}"
                    ))
                })?;
                // Rebuild the unsealed tail from replayed history, then start the
                // background sealer for live commits.
                db.rebuild_chain_tail(&chain);
                chain.start();
                db.chain = Some(chain);
            }

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
                tolerate_torn_tail: wal_config.tolerate_torn_tail,
            };

            let wal = Arc::new(ConcurrentWalSystem::new(wal_system_config)?);

            // Issue #3420: seed the LSN allocator from existing WAL segments
            // before any write is accepted (see with_unified_config for details).
            // This construction path never configures a WAL cipher, matching
            // its (pre-existing) cipher-less read path.
            seed_lsn_allocator_from_segments(&wal, None)?;

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
                constraint_registry: Arc::new(crate::core::constraint::ConstraintRegistry::new()),
                lineage: Arc::new(crate::core::lineage::LineageStore::new()),
                chain: None,
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

    /// Covers config.rs line 435: the WAL+persistence incremental-replay path that
    /// runs `db.edge_id_gen.ensure_at_least(max_eid + 1)` when the incremental WAL
    /// (between the last snapshot LSN and the current WAL head) contains an edge.
    ///
    /// Achieving this requires a snapshot taken *before* the edge is written, while
    /// ensuring the normal background thread does not re-snapshot on shutdown (which
    /// would swallow the edge into the snapshot and leave the incremental WAL empty).
    /// We do this by manually calling `persist_all_indexes`, then stopping the
    /// background thread before creating the edge.
    #[test]
    fn wal_persistence_incremental_wal_edge_covers_edge_id_gen() {
        use crate::config::{AletheiaDBConfig, WalConfigBuilder};
        use crate::storage::index_persistence::PersistenceConfig;
        use crate::storage::index_persistence::operations::persist_all_indexes;
        use crate::storage::wal::DurabilityMode;
        use crate::{PropertyMapBuilder, WriteOps};
        use tempfile::tempdir;

        let scratch = tempdir().unwrap();
        let db_path = scratch.path().to_path_buf();

        let make_config = || {
            AletheiaDBConfig::builder()
                .wal(
                    WalConfigBuilder::new()
                        .wal_dir(db_path.join("wal"))
                        .durability_mode(DurabilityMode::Synchronous)
                        .build(),
                )
                .persistence(PersistenceConfig {
                    enabled: true,
                    data_dir: db_path.join("indexes"),
                    load_on_startup: true,
                    ..Default::default()
                })
                .build()
        };

        let (n1, n2) = {
            let mut db = AletheiaDB::with_unified_config(make_config()).unwrap();
            db.unique_constraint("P", "k").enable().unwrap();
            let n1 = db
                .create_node("P", PropertyMapBuilder::new().insert("k", "a").build())
                .unwrap();
            let n2 = db
                .create_node("P", PropertyMapBuilder::new().insert("k", "b").build())
                .unwrap();

            // Snapshot now so startup sees safe_lsn = current WAL head.
            // Use .expect() rather than if-let so LLVM does not generate uncovered
            // else-branches that inflate the Codecov patch-missing count.
            let tracker = db
                .persistence_tracker
                .as_ref()
                .expect("persistence configured");
            let manager = db
                .persistence_manager
                .as_ref()
                .expect("persistence configured");
            let _ = persist_all_indexes(
                &db.current,
                &db.historical,
                &db.temporal_indexes,
                &db.wal,
                manager,
                tracker,
            );

            // Stop the background thread before it can re-snapshot on its own.
            tracker.signal_shutdown();
            let handle = db
                .persistence_thread_handle
                .take()
                .expect("background thread running");
            let _ = handle.join();

            // persist_all_indexes records safe_lsn = wal.current_lsn(), the
            // NEXT-to-allocate LSN (call it L).  The edge below receives LSN=L
            // — the exact Issue #3419 boundary.  Startup replays from the
            // manifest LSN INCLUSIVE, so the very first post-persist write is
            // recovered without burning a throwaway LSN (the old workaround
            // that this test now guards against regressing).
            db.write(|tx| tx.create_edge(n1, n2, "R", PropertyMapBuilder::new().build()))
                .unwrap();
            (n1, n2)
        };
        let _ = (n1, n2);

        // Session 2: startup loads snapshot (n1+n2 only, no edge) then replays
        // incremental WAL from LSN L INCLUSIVE (the edge, Issue #3419) →
        // max_edge_id = Some(_) → the edge_id_gen bump fires.
        {
            let db = AletheiaDB::with_unified_config(make_config()).unwrap();
            assert_eq!(
                db.edge_count(),
                1,
                "edge must be recovered from incremental WAL"
            );
            db.create_node("P", PropertyMapBuilder::new().insert("k", "a").build())
                .expect_err("constraint must survive WAL+persistence restart");
        }
    }

    /// WAL-only recovery (no index persistence) bumps edge_id_gen past the
    /// highest edge ID seen in the WAL.  This exercises the
    /// `if persistence_manager.is_none()` branch at config.rs lines ~468-472:
    ///
    /// ```text
    /// if let Some(max_eid) = max_edge_id {
    ///     db.edge_id_gen.ensure_at_least(max_eid + 1);
    /// }
    /// ```
    #[test]
    fn wal_only_recovery_bumps_edge_id_gen() {
        use crate::WriteOps;
        use crate::config::{AletheiaDBConfig, WalConfigBuilder};
        use crate::storage::index_persistence::PersistenceConfig;
        use crate::storage::wal::DurabilityMode;
        use tempfile::tempdir;

        let scratch = tempdir().unwrap();
        let wal_dir = scratch.path().join("wal");

        let make_config = || {
            AletheiaDBConfig::builder()
                .wal(
                    WalConfigBuilder::new()
                        .wal_dir(wal_dir.clone())
                        .durability_mode(DurabilityMode::Synchronous)
                        .build(),
                )
                .persistence(PersistenceConfig {
                    enabled: false,
                    ..PersistenceConfig::default()
                })
                .build()
        };

        // Session 1: create two nodes and an edge; remember the edge ID.
        let recorded_edge_id = {
            let db = AletheiaDB::with_unified_config(make_config()).unwrap();
            let n1 = db
                .create_node("N", crate::PropertyMapBuilder::new().build())
                .unwrap();
            let n2 = db
                .create_node("N", crate::PropertyMapBuilder::new().build())
                .unwrap();
            let eid = db
                .write(|tx| tx.create_edge(n1, n2, "E", crate::PropertyMapBuilder::new().build()))
                .unwrap();
            eid.as_u64()
        };

        // Session 2: WAL-only replay — no persistence manager → enters the
        // `persistence_manager.is_none()` branch → max_edge_id is Some(_) →
        // edge_id_gen.ensure_at_least fires → new edge ID must be > recorded one.
        {
            let db = AletheiaDB::with_unified_config(make_config()).unwrap();
            assert_eq!(db.edge_count(), 1, "edge must be recovered from WAL replay");

            let n1 = db
                .create_node("N", crate::PropertyMapBuilder::new().build())
                .unwrap();
            let n2 = db
                .create_node("N", crate::PropertyMapBuilder::new().build())
                .unwrap();
            let new_eid = db
                .write(|tx| tx.create_edge(n1, n2, "E2", crate::PropertyMapBuilder::new().build()))
                .unwrap();
            assert!(
                new_eid.as_u64() > recorded_edge_id,
                "edge_id_gen must be bumped past the WAL-replayed max edge ID \
                 (got {}, expected > {})",
                new_eid.as_u64(),
                recorded_edge_id,
            );
        }
    }

    /// Issue #3387: the startup HLC seed must fold restored CLOSED
    /// transaction-time ends into the max, not just tx starts -- a closure
    /// stamped by a since-cold-migrated superseding version can exceed
    /// every restored tx start.
    #[test]
    fn bootstrap_timestamp_folds_closed_tx_ends() {
        use crate::core::GLOBAL_INTERNER;
        use crate::core::hlc::HybridTimestamp;
        use crate::core::id::{EdgeId, NodeId, VersionId};
        use crate::core::property::PropertyMapBuilder;
        use crate::storage::historical::HistoricalStorage;
        use parking_lot::RwLock;

        let current = CurrentStorage::new();
        let historical = RwLock::new(HistoricalStorage::new());

        let now = crate::core::temporal::time::now().wallclock();
        let node_start = HybridTimestamp::new(now + 3_600_000_000, 0).unwrap(); // now + 1h
        let node_end = HybridTimestamp::new(now + 7_200_000_000, 3).unwrap(); // now + 2h
        let edge_start = HybridTimestamp::new(now + 1_800_000_000, 0).unwrap();
        let edge_end = HybridTimestamp::new(now + 10_800_000_000, 5).unwrap(); // now + 3h (max)

        {
            let mut hist = historical.write();
            let label = GLOBAL_INTERNER.intern("BootstrapFold").unwrap();
            let node_id = NodeId::new(1).unwrap();
            let node_vid = VersionId::new(1).unwrap();
            hist.add_node_version(
                node_id,
                node_vid,
                node_start,
                node_start,
                label,
                PropertyMapBuilder::new().build(),
                false,
            )
            .unwrap();
            hist.close_node_version_transaction_time(node_vid, node_end)
                .unwrap();

            let edge_label = GLOBAL_INTERNER.intern("BOOTSTRAP_FOLD").unwrap();
            let edge_id = EdgeId::new(1).unwrap();
            let edge_vid = VersionId::new(2).unwrap();
            hist.add_edge_version(
                edge_id,
                edge_vid,
                edge_start,
                edge_start,
                edge_label,
                NodeId::new(1).unwrap(),
                NodeId::new(2).unwrap(),
                PropertyMapBuilder::new().build(),
                false,
            )
            .unwrap();
            hist.close_edge_version_transaction_time(edge_vid, edge_end)
                .unwrap();
        }

        let seed = bootstrap_timestamp(&current, &historical);
        assert_eq!(
            seed, edge_end,
            "seed must be the max over restored tx starts AND closed tx ends \
             (here the edge's closed end, incl. its logical component)"
        );
    }
}
