//! Main GallifreyDB database API.
//!
//! This module provides the primary interface to the database, coordinating
//! between current storage (fast path) and historical storage (temporal path).

use crate::api::transaction::{
    ReadTransaction, TxIdGenerator, TxVisibilityManager, WriteOps, WriteTransaction,
};
use crate::config::GallifreyDBConfig;
use crate::core::graph::{Edge, Node};
use crate::core::id::{EdgeId, IdGenerator, NodeId};
use crate::core::property::PropertyMap;
use crate::core::temporal::{Timestamp, time};
use crate::index::temporal::TemporalIndexes;
use crate::index::vector::hnsw::HnswConfig;
use crate::index::vector::temporal::{TemporalVectorConfig, VectorIndexObserver};
use crate::query::builder::state::Initial;
use crate::query::planner::Statistics;
use crate::query::{Query, QueryBuilder, QueryExecutor, QueryPlanner, QueryResults};
use crate::storage::current::CurrentStorage;
use crate::storage::historical::HistoricalStorage;
use crate::storage::index_persistence::operations::{
    load_vector_indexes, persist_temporal_index, persist_vector_indexes,
};
use crate::storage::index_persistence::tracker::PersistenceTracker;
use crate::storage::index_persistence::worker::spawn_background_persistence_thread;
use crate::storage::version::AnchorConfig;
use crate::storage::wal::concurrent_system::{ConcurrentWalSystem, ConcurrentWalSystemConfig};
use crate::storage::wal::{DurabilityMode, WriteOptions};
use crate::utils::error::{Result, StorageError};
use crate::utils::lock::{MutexExt, RwLockExt};
use parking_lot::RwLock;
use std::sync::{Arc, Mutex};

/// Main GallifreyDB database.
///
/// This is the primary entry point for interacting with the database.
/// It coordinates between current storage (for fast current-state queries)
/// and historical storage (for temporal queries).
///
/// # Durability Modes
///
/// GallifreyDB supports three durability modes for write transactions:
///
/// - **Synchronous** (default): Each commit waits for fsync. Maximum durability.
/// - **Async**: Commits return immediately, background thread syncs. Fast but risk of data loss.
/// - **GroupCommit**: Multiple commits share one fsync. ACID durability with high throughput.
///
/// Use [`with_wal_config`](Self::with_wal_config) to configure the default mode,
/// or [`write_with_options`](Self::write_with_options) for per-transaction overrides.
///
/// # Examples
///
/// ```ignore
/// use gallifreydb::{GallifreyDB, PropertyMapBuilder};
/// use gallifreydb::index::vector::{HnswConfig, DistanceMetric};
///
/// // 1. Initialize the database
/// let db = GallifreyDB::new().expect("Failed to open database");
///
/// // 2. Enable vector indexing (optional)
/// db.vector_index("embedding")
///     .hnsw(HnswConfig::new(384, DistanceMetric::Cosine))
///     .enable()?;
///
/// // 3. Create nodes
/// let alice_id = db.create_node(
///     "Person",
///     PropertyMapBuilder::new()
///         .insert("name", "Alice")
///         .insert("age", 30)
///         .build()
/// )?;
///
/// let bob_id = db.create_node(
///     "Person",
///     PropertyMapBuilder::new()
///         .insert("name", "Bob")
///         .insert_vector("embedding", &[0.1, 0.2, 0.3])
///         .build()
/// )?;
///
/// // 4. Create an edge
/// db.create_edge(
///     alice_id,
///     bob_id,
///     "KNOWS",
///     PropertyMapBuilder::new().insert("since", 2024).build()
/// )?;
///
/// // 5. Query
/// let alice = db.get_node(alice_id)?;
/// println!("Found: {:?}", alice.properties.get("name"));
/// ```
pub struct GallifreyDB {
    /// Current state storage (hot path) - Arc-wrapped for sharing across transactions
    current: Arc<CurrentStorage>,
    /// Historical version storage (temporal path) - RwLock-protected for concurrent reads
    historical: Arc<RwLock<HistoricalStorage>>,
    /// Temporal indexes for efficient time-based queries - Uses DashMap internally for fine-grained locking
    temporal_indexes: Arc<TemporalIndexes>,
    /// Concurrent Write-Ahead Log for durability - lock-free striped architecture
    wal: Arc<ConcurrentWalSystem>,
    /// Current logical timestamp for transaction time - Mutex-protected for thread-safe increment
    current_timestamp: Arc<Mutex<Timestamp>>,
    /// Transaction ID generator for MVCC
    tx_id_gen: Arc<TxIdGenerator>,
    /// Transaction visibility manager for Snapshot Isolation
    visibility_manager: Arc<TxVisibilityManager>,
    /// ID generators for nodes, edges, and versions (shared with transactions)
    node_id_gen: Arc<Mutex<IdGenerator>>,
    edge_id_gen: Arc<Mutex<IdGenerator>>,
    version_id_gen: Arc<Mutex<IdGenerator>>,
    /// Default durability mode for write transactions
    default_durability: DurabilityMode,
    /// Query optimization statistics - cached across queries for effective cost-based optimization
    stats: Arc<Statistics>,
    /// Index persistence configuration (stored for potential future use)
    #[allow(dead_code)]
    persistence_config: crate::storage::index_persistence::PersistenceConfig,
    /// Index persistence manager (if enabled)
    persistence_manager: Option<Arc<crate::storage::index_persistence::IndexPersistenceManager>>,
    /// Persistence mutation tracking
    persistence_tracker: Option<Arc<PersistenceTracker>>,
    /// Background persistence thread health flag - set to true if thread panics or stops
    persistence_thread_stopped: Arc<std::sync::atomic::AtomicBool>,
}

impl GallifreyDB {
    /// Create a new empty database with default configuration.
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
    /// use gallifreydb::{GallifreyDB, WalConfigBuilder, DurabilityMode};
    ///
    /// // High-throughput ACID mode with group commit
    /// let wal_config = WalConfigBuilder::new()
    ///     .durability_mode(DurabilityMode::group_commit(10, 200))
    ///     .build();
    /// let db = GallifreyDB::with_wal_config(wal_config)?;
    ///
    /// // Bulk loading mode with async durability
    /// let wal_config = WalConfigBuilder::new()
    ///     .durability_mode(DurabilityMode::async_mode(100))
    ///     .build();
    /// let db = GallifreyDB::with_wal_config(wal_config)?;
    /// ```
    pub fn with_wal_config(wal_config: crate::config::WalConfig) -> Result<Self> {
        Self::with_full_config(AnchorConfig::default(), wal_config)
    }

    /// Create a new database with unified configuration.
    ///
    /// This method accepts a [`GallifreyDBConfig`] which consolidates all configuration
    /// settings for the database, including WAL, historical storage, and vector indexes.
    ///
    /// # Errors
    ///
    /// Returns an error if WAL initialization fails (e.g., cannot create WAL directory).
    ///
    /// # Example
    ///
    /// ```ignore
    /// use gallifreydb::{GallifreyDB, config::GallifreyDBConfig, config::WalConfigBuilder};
    ///
    /// let config = GallifreyDBConfig::builder()
    ///     .wal(WalConfigBuilder::new()
    ///         .with_validated(32, 2048, 64 * 1024, 64 * 1024 * 1024, 10, 10).unwrap()
    ///         .build())
    ///     .build();
    ///
    /// let db = GallifreyDB::with_unified_config(config)?;
    /// ```
    pub fn with_unified_config(config: GallifreyDBConfig) -> Result<Self> {
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

        let db = GallifreyDB {
            current: Arc::new(CurrentStorage::new()),
            historical: Arc::new(RwLock::new(HistoricalStorage::from_unified_config(
                config.historical,
            ))),
            temporal_indexes: Arc::new(TemporalIndexes::new()),
            wal,
            current_timestamp: Arc::new(Mutex::new(time::now())),
            tx_id_gen: Arc::new(TxIdGenerator::new()),
            visibility_manager: Arc::new(TxVisibilityManager::new()),
            node_id_gen: Arc::new(Mutex::new(IdGenerator::new())),
            edge_id_gen: Arc::new(Mutex::new(IdGenerator::new())),
            version_id_gen: Arc::new(Mutex::new(IdGenerator::new())),
            default_durability: durability_mode,
            stats: Arc::new(Statistics::new()),
            persistence_config: config.persistence.clone(),
            persistence_manager: persistence_manager.clone(),
            persistence_tracker: persistence_tracker.clone(),
            persistence_thread_stopped: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        };

        // Load indexes on startup if enabled
        if let Some(ref manager) = persistence_manager
            && config.persistence.load_on_startup
        {
            // Try to load manifest and string interner, but don't fail if manifest doesn't exist yet
            // (manifest is only saved on shutdown, not during background persistence)
            match manager.load_manifest_and_strings() {
                Ok(_) => {}                      // Successfully loaded
                Err(e) if e.is_not_found() => {} // Expected on first run
                Err(e) => eprintln!("Warning: Failed to load manifest: {}", e),
            }

            // Try to restore graph data even if manifest loading failed
            let graph_path = manager.graph_path().join("adjacency.idx");
            if graph_path.exists() {
                use crate::api::transaction::types::TxId;
                use crate::core::GLOBAL_INTERNER;
                use crate::core::graph::{Edge, Node};
                use crate::core::id::{EdgeId, NodeId, VersionId};
                use crate::core::version::VersionMetadata;
                use crate::storage::index_persistence::graph::{
                    load_graph_index, restore_property_map,
                };

                match load_graph_index(&graph_path) {
                    Ok(graph_data) => {
                        let current_time = time::now();
                        let mut max_node_id = 0u64;
                        let mut max_edge_id = 0u64;

                        // Track restoration statistics
                        let total_nodes = graph_data.nodes.len();
                        let total_edges = graph_data.edges.len();
                        let mut nodes_loaded = 0usize;
                        let mut edges_loaded = 0usize;
                        let mut nodes_failed_label = 0usize;
                        let mut nodes_failed_properties = 0usize;
                        let mut nodes_failed_version = 0usize;
                        let mut edges_failed_label = 0usize;
                        let mut edges_failed_properties = 0usize;
                        let mut edges_failed_version = 0usize;

                        // Pre-calculate max IDs before inserting to avoid race conditions
                        let mut max_version_id = 0u64;
                        for persisted_node in &graph_data.nodes {
                            max_node_id = max_node_id.max(persisted_node.id);
                            max_version_id = max_version_id.max(persisted_node.version_id);
                        }
                        for persisted_edge in &graph_data.edges {
                            max_edge_id = max_edge_id.max(persisted_edge.id);
                            max_version_id = max_version_id.max(persisted_edge.version_id);
                        }

                        // Initialize ID generators BEFORE inserting entities to prevent collisions
                        if max_node_id > 0
                            && let Ok(mut node_gen) = db.node_id_gen.lock_or_err()
                        {
                            *node_gen = crate::core::id::IdGenerator::with_start(max_node_id + 1);
                        }
                        if max_edge_id > 0
                            && let Ok(mut edge_gen) = db.edge_id_gen.lock_or_err()
                        {
                            *edge_gen = crate::core::id::IdGenerator::with_start(max_edge_id + 1);
                        }
                        // Initialize version ID generator from max persisted version_id
                        if max_version_id > 0
                            && let Ok(mut version_gen) = db.version_id_gen.lock_or_err()
                        {
                            *version_gen =
                                crate::core::id::IdGenerator::with_start(max_version_id + 1);
                        }

                        // Restore nodes with explicit error tracking
                        for persisted_node in &graph_data.nodes {
                            // Validate label exists in string interner
                            let label_str = match GLOBAL_INTERNER.resolve(
                                crate::core::InternedString::from_raw(persisted_node.label_idx),
                            ) {
                                Some(s) => s,
                                None => {
                                    nodes_failed_label += 1;
                                    eprintln!(
                                        "Warning: Skipping node {}: label index {} not found in string interner",
                                        persisted_node.id, persisted_node.label_idx
                                    );
                                    continue;
                                }
                            };

                            // Restore properties
                            let properties = match restore_property_map(&persisted_node.properties)
                            {
                                Ok(p) => p,
                                Err(e) => {
                                    nodes_failed_properties += 1;
                                    eprintln!(
                                        "Warning: Skipping node {} (label '{}'): property restoration failed: {}",
                                        persisted_node.id, label_str, e
                                    );
                                    continue;
                                }
                            };

                            // Restore version ID from persisted data (CRITICAL for temporal provenance)
                            let version_id = match VersionId::new(persisted_node.version_id) {
                                Ok(v) => v,
                                Err(e) => {
                                    nodes_failed_version += 1;
                                    eprintln!(
                                        "Warning: Skipping node {} (label '{}'): invalid version ID {}: {}",
                                        persisted_node.id, label_str, persisted_node.version_id, e
                                    );
                                    continue;
                                }
                            };

                            let node = Node {
                                id: NodeId::new_unchecked(persisted_node.id),
                                label: crate::core::InternedString::from_raw(
                                    persisted_node.label_idx,
                                ),
                                properties,
                                current_version: version_id,
                                metadata: VersionMetadata {
                                    created_by_tx: TxId::new(0), // Restored from disk
                                    commit_timestamp: Some(current_time),
                                },
                            };

                            let _ = db.current.insert_node_direct(node, current_time);
                            nodes_loaded += 1;
                        }

                        // Restore edges with explicit error tracking
                        for persisted_edge in &graph_data.edges {
                            // Validate label exists in string interner
                            let label_str = match GLOBAL_INTERNER.resolve(
                                crate::core::InternedString::from_raw(persisted_edge.label_idx),
                            ) {
                                Some(s) => s,
                                None => {
                                    edges_failed_label += 1;
                                    eprintln!(
                                        "Warning: Skipping edge {}: label index {} not found in string interner",
                                        persisted_edge.id, persisted_edge.label_idx
                                    );
                                    continue;
                                }
                            };

                            // Restore properties
                            let properties = match restore_property_map(&persisted_edge.properties)
                            {
                                Ok(p) => p,
                                Err(e) => {
                                    edges_failed_properties += 1;
                                    eprintln!(
                                        "Warning: Skipping edge {} (label '{}'): property restoration failed: {}",
                                        persisted_edge.id, label_str, e
                                    );
                                    continue;
                                }
                            };

                            // Restore version ID from persisted data (CRITICAL for temporal provenance)
                            let version_id = match VersionId::new(persisted_edge.version_id) {
                                Ok(v) => v,
                                Err(e) => {
                                    edges_failed_version += 1;
                                    eprintln!(
                                        "Warning: Skipping edge {} (label '{}'): invalid version ID {}: {}",
                                        persisted_edge.id, label_str, persisted_edge.version_id, e
                                    );
                                    continue;
                                }
                            };

                            let edge = Edge {
                                id: EdgeId::new_unchecked(persisted_edge.id),
                                source: NodeId::new_unchecked(persisted_edge.source_id),
                                target: NodeId::new_unchecked(persisted_edge.target_id),
                                label: crate::core::InternedString::from_raw(
                                    persisted_edge.label_idx,
                                ),
                                properties,
                                current_version: version_id,
                                metadata: VersionMetadata {
                                    created_by_tx: TxId::new(0), // Restored from disk
                                    commit_timestamp: Some(current_time),
                                },
                            };

                            let _ = db.current.insert_edge_direct(edge);
                            edges_loaded += 1;
                        }

                        // Log restoration summary
                        let nodes_skipped = total_nodes - nodes_loaded;
                        let edges_skipped = total_edges - edges_loaded;

                        if nodes_skipped > 0 || edges_skipped > 0 {
                            eprintln!(
                                "Index restoration completed with data loss:\n\
                                 Nodes: {}/{} loaded ({} skipped - {} label errors, {} property errors, {} version errors)\n\
                                 Edges: {}/{} loaded ({} skipped - {} label errors, {} property errors, {} version errors)",
                                nodes_loaded,
                                total_nodes,
                                nodes_skipped,
                                nodes_failed_label,
                                nodes_failed_properties,
                                nodes_failed_version,
                                edges_loaded,
                                total_edges,
                                edges_skipped,
                                edges_failed_label,
                                edges_failed_properties,
                                edges_failed_version
                            );
                        } else if total_nodes > 0 || total_edges > 0 {
                            eprintln!(
                                "Index restoration completed successfully: {} nodes, {} edges loaded",
                                nodes_loaded, edges_loaded
                            );
                        }

                        // Import CSR adjacency structures if available, otherwise rebuild
                        if !graph_data.outgoing_offsets.is_empty()
                            && !graph_data.incoming_offsets.is_empty()
                        {
                            db.current.import_csr(
                                graph_data.outgoing_node_ids,
                                graph_data.outgoing_offsets,
                                graph_data.outgoing_neighbors,
                                graph_data.incoming_node_ids,
                                graph_data.incoming_offsets,
                                graph_data.incoming_neighbors,
                            );
                        } else {
                            // Fallback for older index files without CSR data
                            db.current.compact_adjacency();
                        }
                    }
                    Err(_e) => {
                        // Graph index loading failed - start with empty graph
                        // This is normal if no index files exist yet
                    }
                }
            }

            // Load temporal index (version history)
            let temporal_path = manager.temporal_path().join("versions.idx");
            if temporal_path.exists() {
                use crate::storage::index_persistence::temporal::{
                    load_temporal_index, restore_into_historical_storage,
                };

                match load_temporal_index(&temporal_path) {
                    Ok(temporal_data) => {
                        // Restore versions into historical storage
                        // Labels are now stored directly in the persisted entries
                        let mut historical_guard = db.historical.write();
                        match restore_into_historical_storage(&temporal_data, &mut historical_guard)
                        {
                            Ok(()) => {
                                eprintln!(
                                    "Temporal index restored: {} node versions, {} edge versions",
                                    temporal_data.node_versions.len(),
                                    temporal_data.edge_versions.len()
                                );
                            }
                            Err(e) => {
                                eprintln!("Warning: Failed to restore temporal versions: {}", e);
                            }
                        }
                        drop(historical_guard);
                    }
                    Err(e) => {
                        eprintln!("Warning: Failed to load temporal index: {}", e);
                    }
                }
            }

            // Load vector indexes
            if let Err(e) = load_vector_indexes(&db.current, manager) {
                eprintln!("Warning: Failed to load vector indexes: {}", e);
            }
        }

        // Start background persistence thread if enabled
        if let Some(ref tracker) = persistence_tracker
            && let Some(ref manager) = persistence_manager
        {
            spawn_background_persistence_thread(
                Arc::clone(&db.current),
                Arc::clone(&db.historical),
                Arc::clone(&db.temporal_indexes),
                Arc::clone(&db.wal),
                Arc::clone(manager),
                Arc::clone(tracker),
                config.persistence.policies.clone(),
                Arc::clone(&db.persistence_thread_stopped),
            );
        }

        // Wire temporal indexes to historical storage for O(log n) version lookups (Issue #209)
        db.historical
            .write()
            .set_temporal_indexes(Arc::clone(&db.temporal_indexes));

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

        let db = GallifreyDB {
            current: Arc::new(CurrentStorage::new()),
            historical: Arc::new(RwLock::new(HistoricalStorage::with_config(anchor_config))),
            temporal_indexes: Arc::new(TemporalIndexes::new()),
            wal,
            current_timestamp: Arc::new(Mutex::new(time::now())),
            tx_id_gen: Arc::new(TxIdGenerator::new()),
            visibility_manager: Arc::new(TxVisibilityManager::new()),
            node_id_gen: Arc::new(Mutex::new(IdGenerator::new())),
            edge_id_gen: Arc::new(Mutex::new(IdGenerator::new())),
            version_id_gen: Arc::new(Mutex::new(IdGenerator::new())),
            default_durability: durability_mode,
            stats: Arc::new(Statistics::new()),
            persistence_config: crate::storage::index_persistence::PersistenceConfig::default(),
            persistence_manager: None,
            persistence_tracker: None,
            persistence_thread_stopped: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        };

        // Wire temporal indexes to historical storage for O(log n) version lookups (Issue #209)
        db.historical
            .write()
            .set_temporal_indexes(Arc::clone(&db.temporal_indexes));

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
    /// Returns a `GallifreyDB` instance with restored configuration, or an error
    /// if the checkpoint cannot be loaded or the vector index cannot be restored.
    ///
    /// # Example
    ///
    /// ```ignore
    /// use gallifreydb::GallifreyDB;
    /// use std::path::Path;
    ///
    /// let db = GallifreyDB::open(Path::new("gallifreydb/checkpoints/latest.gfry"))?;
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

    /// Create a new read-only transaction.
    ///
    /// Read-only transactions are lightweight and have zero overhead:
    /// - No write buffer
    /// - No WAL logging
    /// - Snapshot-based reads for consistency
    /// - No commit overhead
    ///
    /// # Errors
    ///
    /// Returns an error if the timestamp lock is poisoned.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let tx = db.read_transaction()?;
    /// let node = tx.get_node(node_id)?;
    /// // No commit needed - transaction is read-only
    /// ```
    pub fn read_transaction(&self) -> Result<ReadTransaction> {
        let tx_id = self.tx_id_gen.next();
        let snapshot_timestamp = *self.current_timestamp.lock_or_err()?;

        // Register as active
        self.visibility_manager.register_active(tx_id);

        // Capture snapshot
        let snapshot = self.visibility_manager.capture_snapshot(snapshot_timestamp);

        Ok(ReadTransaction::new(
            tx_id,
            snapshot,
            Arc::clone(&self.current),
            Arc::clone(&self.visibility_manager),
            Arc::clone(&self.historical),
        ))
    }

    /// Execute a read-only operation in a transaction.
    ///
    /// This is a closure-based API that automatically manages the transaction lifecycle.
    /// The transaction is automatically cleaned up after the closure completes.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let name = db.read(|tx| {
    ///     let node = tx.get_node(node_id)?;
    ///     Ok(node.get_property("name").cloned())
    /// })?;
    /// ```
    pub fn read<F, T>(&self, f: F) -> Result<T>
    where
        F: FnOnce(&ReadTransaction) -> Result<T>,
    {
        let tx = self.read_transaction()?;
        f(&tx)
    }

    /// Create a new write transaction.
    ///
    /// Write transactions provide full ACID guarantees:
    /// - **Atomicity**: All-or-nothing commit via write buffering
    /// - **Consistency**: Referential integrity validation before commit
    /// - **Isolation**: Snapshot Isolation with write-write conflict detection
    /// - **Durability**: WAL with fsync for true durability
    ///
    /// # Errors
    ///
    /// Returns an error if the timestamp lock is poisoned.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let mut tx = db.write_transaction()?;
    /// let node_id = tx.create_node("Person", props)?;
    /// tx.create_edge(node_id, other, "KNOWS", edge_props)?;
    /// tx.commit()?;  // or tx.rollback()
    /// ```
    pub fn write_transaction(&self) -> Result<WriteTransaction> {
        let tx_id = self.tx_id_gen.next();

        // Capture snapshot timestamp using current wallclock time, ensuring it's
        // >= the last commit timestamp (monotonicity). This allows the transaction
        // to see all commits that happened before it started.
        let snapshot_timestamp = {
            let ts = self.current_timestamp.lock_or_err()?;
            std::cmp::max(crate::core::temporal::time::now(), *ts)
        };

        // Register as active
        self.visibility_manager.register_active(tx_id);

        // Capture snapshot
        let snapshot = self.visibility_manager.capture_snapshot(snapshot_timestamp);

        Ok(WriteTransaction::new(
            tx_id,
            snapshot,
            Arc::clone(&self.current),
            Arc::clone(&self.historical),
            Arc::clone(&self.temporal_indexes),
            Arc::clone(&self.wal),
            Arc::clone(&self.current_timestamp),
            Arc::clone(&self.visibility_manager),
            Arc::clone(&self.node_id_gen),
            Arc::clone(&self.edge_id_gen),
            Arc::clone(&self.version_id_gen),
        ))
    }

    /// Execute a write operation in a transaction.
    ///
    /// This is a closure-based API that automatically manages the transaction lifecycle.
    /// The transaction is automatically committed on Ok, or rolled back on Err.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let node_id = db.write(|tx| {
    ///     let id = tx.create_node("Person", props)?;
    ///     tx.create_edge(id, other, "KNOWS", edge_props)?;
    ///     Ok(id)
    /// })?;
    /// ```
    pub fn write<F, T>(&self, f: F) -> Result<T>
    where
        F: FnOnce(&mut WriteTransaction) -> Result<T>,
    {
        let mut tx = self.write_transaction()?;
        let result = f(&mut tx)?;

        // Track mutations for persistence before committing
        let has_node_writes = tx.has_node_writes();
        let has_edge_writes = tx.has_edge_writes();
        let has_vector_writes = tx.has_vector_writes();

        tx.commit()?; // Ignore commit timestamp for simple write()

        // Record mutations after successful commit
        if let Some(ref tracker) = self.persistence_tracker {
            if has_node_writes || has_edge_writes {
                tracker.record_graph_mutation();
                tracker.record_temporal_mutation();
                // String interner mutations happen with every node/edge (labels)
                tracker.record_string_mutation();
            }
            if has_vector_writes {
                tracker.record_vector_mutation();
            }
        }

        Ok(result)
    }

    /// Execute a write operation and return both the result and commit timestamp.
    ///
    /// This is useful for benchmarks and tests that need to query the database
    /// at the exact commit timestamp to verify temporal semantics.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let (node_id, commit_ts) = db.write_with_timestamp(|tx| {
    ///     tx.create_node("Person", properties)
    /// })?;
    ///
    /// // Query at exact commit timestamp
    /// let node = db.get_node_at_time(node_id, commit_ts, commit_ts)?;
    /// ```
    pub fn write_with_timestamp<F, T>(&self, f: F) -> Result<(T, Timestamp)>
    where
        F: FnOnce(&mut WriteTransaction) -> Result<T>,
    {
        let mut tx = self.write_transaction()?;
        let result = f(&mut tx)?;

        // Track mutations for persistence before committing
        let has_node_writes = tx.has_node_writes();
        let has_edge_writes = tx.has_edge_writes();
        let has_vector_writes = tx.has_vector_writes();

        let commit_ts = tx.commit_with_timestamp()?;

        // Record mutations after successful commit
        if let Some(ref tracker) = self.persistence_tracker {
            if has_node_writes || has_edge_writes {
                tracker.record_graph_mutation();
                tracker.record_temporal_mutation();
            }
            if has_vector_writes {
                tracker.record_vector_mutation();
            }
        }

        Ok((result, commit_ts))
    }

    /// Execute a write operation with custom durability options.
    ///
    /// This allows overriding the database's default durability mode for
    /// specific transactions. Useful for bulk loading (Async mode) or
    /// critical operations (Synchronous mode override).
    ///
    /// # Example
    ///
    /// ```ignore
    /// use gallifreydb::{GallifreyDB, WriteOptions, DurabilityMode};
    ///
    /// let db = GallifreyDB::new();
    ///
    /// // Use Async mode for bulk loading (faster but less durable)
    /// let options = WriteOptions::new()
    ///     .with_durability(DurabilityMode::async_mode(100));
    ///
    /// db.write_with_options(options, |tx| {
    ///     for item in bulk_data {
    ///         tx.create_node("Item", item.into())?;
    ///     }
    ///     Ok(())
    /// })?;
    /// ```
    pub fn write_with_options<F, T>(&self, options: WriteOptions, f: F) -> Result<T>
    where
        F: FnOnce(&mut WriteTransaction) -> Result<T>,
    {
        let mut tx = self.write_transaction_with_options(options)?;
        let result = f(&mut tx)?;

        // Track mutations for persistence before committing
        let has_node_writes = tx.has_node_writes();
        let has_edge_writes = tx.has_edge_writes();
        let has_vector_writes = tx.has_vector_writes();

        tx.commit()?; // Ignore commit timestamp for simple write_with_options()

        // Record mutations after successful commit
        if let Some(ref tracker) = self.persistence_tracker {
            if has_node_writes || has_edge_writes {
                tracker.record_graph_mutation();
                tracker.record_temporal_mutation();
                // String interner mutations happen with every node/edge (labels)
                tracker.record_string_mutation();
            }
            if has_vector_writes {
                tracker.record_vector_mutation();
            }
        }

        Ok(result)
    }

    /// Create a write transaction with custom durability options.
    ///
    /// This is the low-level API for creating transactions with specific
    /// durability settings. The transaction must be manually committed or
    /// rolled back.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let options = WriteOptions::new()
    ///     .with_durability(DurabilityMode::Synchronous);
    ///
    /// let mut tx = db.write_transaction_with_options(options)?;
    /// tx.create_node("Critical", props)?;
    /// tx.commit()?;
    /// ```
    pub fn write_transaction_with_options(
        &self,
        options: WriteOptions,
    ) -> Result<WriteTransaction> {
        let tx_id = self.tx_id_gen.next();

        // Capture snapshot timestamp using current wallclock time, ensuring it's
        // >= the last commit timestamp (monotonicity). This allows the transaction
        // to see all commits that happened before it started.
        let snapshot_timestamp = {
            let ts = self.current_timestamp.lock_or_err()?;
            std::cmp::max(crate::core::temporal::time::now(), *ts)
        };

        // Register as active
        self.visibility_manager.register_active(tx_id);

        // Capture snapshot
        let snapshot = self.visibility_manager.capture_snapshot(snapshot_timestamp);

        // Determine effective durability mode
        let durability = options.effective_durability(self.default_durability);

        Ok(WriteTransaction::new_with_durability(
            tx_id,
            snapshot,
            Arc::clone(&self.current),
            Arc::clone(&self.historical),
            Arc::clone(&self.temporal_indexes),
            Arc::clone(&self.wal),
            Arc::clone(&self.current_timestamp),
            Arc::clone(&self.visibility_manager),
            Arc::clone(&self.node_id_gen),
            Arc::clone(&self.edge_id_gen),
            Arc::clone(&self.version_id_gen),
            durability,
        ))
    }

    /// Create a node with the given label and properties.
    ///
    /// This is a convenience method that internally uses a write transaction.
    /// For multiple operations, prefer using `write()` or `write_transaction()`.
    ///
    /// # Example
    ///
    /// ```ignore
    /// use gallifreydb::PropertyMapBuilder;
    ///
    /// let node_id = db.create_node(
    ///     "Person",
    ///     PropertyMapBuilder::new()
    ///         .insert("name", "Alice")
    ///         .build()
    /// )?;
    /// ```
    pub fn create_node(&self, label: &str, properties: PropertyMap) -> Result<NodeId> {
        self.write(|tx| tx.create_node(label, properties))
    }

    /// Create an edge between two nodes.
    ///
    /// This is a convenience method that internally uses a write transaction.
    /// For multiple operations, prefer using `write()` or `write_transaction()`.
    ///
    /// # Example
    ///
    /// ```ignore
    /// use gallifreydb::PropertyMapBuilder;
    ///
    /// let edge_id = db.create_edge(
    ///     source_id,
    ///     target_id,
    ///     "KNOWS",
    ///     PropertyMapBuilder::new().insert("since", 2024).build()
    /// )?;
    /// ```
    pub fn create_edge(
        &self,
        source: NodeId,
        target: NodeId,
        label: &str,
        properties: PropertyMap,
    ) -> Result<EdgeId> {
        self.write(|tx| tx.create_edge(source, target, label, properties))
    }

    /// Get the current state of a node.
    ///
    /// This uses the fast path (current storage) for O(1) lookup.
    pub fn get_node(&self, node_id: NodeId) -> Result<Node> {
        self.current.get_node(node_id)
    }

    /// Get the current state of an edge.
    pub fn get_edge(&self, edge_id: EdgeId) -> Result<Edge> {
        self.current.get_edge(edge_id)
    }

    // ========================================================================
    // Zero-copy access methods (Issue #190)
    // ========================================================================

    /// Get the target node of an edge without cloning the entire edge.
    ///
    /// # Performance
    ///
    /// - **Zero-copy**: Only reads and returns the target NodeId (8 bytes)
    /// - **No allocation**: Does not clone Edge or PropertyMap
    #[inline]
    pub fn get_edge_target(&self, edge_id: EdgeId) -> Result<NodeId> {
        self.current.get_edge_target(edge_id)
    }

    /// Get the source node of an edge without cloning the entire edge.
    ///
    /// # Performance
    ///
    /// - **Zero-copy**: Only reads and returns the source NodeId (8 bytes)
    /// - **No allocation**: Does not clone Edge or PropertyMap
    #[inline]
    pub fn get_edge_source(&self, edge_id: EdgeId) -> Result<NodeId> {
        self.current.get_edge_source(edge_id)
    }

    /// Get outgoing edges from a node (current state).
    pub fn get_outgoing_edges(&self, node_id: NodeId) -> Vec<EdgeId> {
        self.current.get_outgoing_edges(node_id)
    }

    /// Get incoming edges to a node (current state).
    pub fn get_incoming_edges(&self, node_id: NodeId) -> Vec<EdgeId> {
        self.current.get_incoming_edges(node_id)
    }

    /// Get outgoing edges with a specific label (current state).
    pub fn get_outgoing_edges_with_label(&self, node_id: NodeId, label: &str) -> Vec<EdgeId> {
        self.current.get_outgoing_edges_with_label(node_id, label)
    }

    /// Get a node as it existed at a specific point in bi-temporal space.
    ///
    /// Uses the temporal index for O(log n) candidate lookup, then verifies
    /// visibility with historical storage (handles closed intervals from deletions).
    pub fn get_node_at_time(
        &self,
        node_id: NodeId,
        valid_time: Timestamp,
        transaction_time: Timestamp,
    ) -> Result<Node> {
        #[cfg(feature = "observability")]
        let _span = tracing::info_span!("get_node_at_time").entered();

        // Use temporal index for efficient O(log n) candidate lookup (Issue #194 fix)
        let version_ids =
            self.temporal_indexes
                .find_node_version_at_point(node_id, valid_time, transaction_time);

        // Early return if no candidates - avoid unnecessary lock acquisition
        if version_ids.is_empty() {
            return Err(StorageError::NodeNotFound(node_id).into());
        }

        let historical = self.historical.read_or_err()?;

        // IMPORTANT: Double-check visibility with historical storage.
        // This is intentional and NOT redundant: the temporal index stores intervals at
        // insertion time, but deletions close the valid_time interval in historical storage.
        // Without this check, deleted nodes would incorrectly appear as visible.
        for version_id in version_ids {
            if let Some(version) = historical.get_node_version(version_id)
                && version.temporal.is_visible_at(valid_time, transaction_time)
            {
                // Reconstruct properties
                let properties = historical.reconstruct_node_properties(version_id)?;

                // Build node from version
                return Ok(Node::new(
                    version.node_id,
                    version.label,
                    properties,
                    version.id,
                ));
            }
        }

        // No visible version found
        Err(StorageError::NodeNotFound(node_id).into())
    }

    /// Get an edge as it existed at a specific point in bi-temporal space.
    ///
    /// Uses the temporal index for O(log n) candidate lookup, then verifies
    /// visibility with historical storage (handles closed intervals from deletions).
    pub fn get_edge_at_time(
        &self,
        edge_id: EdgeId,
        valid_time: Timestamp,
        transaction_time: Timestamp,
    ) -> Result<Edge> {
        #[cfg(feature = "observability")]
        let _span = tracing::info_span!("get_edge_at_time").entered();

        // Use temporal index for efficient O(log n) candidate lookup (Issue #194 fix)
        let version_ids =
            self.temporal_indexes
                .find_edge_version_at_point(edge_id, valid_time, transaction_time);

        // Early return if no candidates - avoid unnecessary lock acquisition
        if version_ids.is_empty() {
            return Err(StorageError::EdgeNotFound(edge_id).into());
        }

        let historical = self.historical.read_or_err()?;

        // IMPORTANT: Double-check visibility with historical storage.
        // This is intentional and NOT redundant: the temporal index stores intervals at
        // insertion time, but deletions close the valid_time interval in historical storage.
        // Without this check, deleted edges would incorrectly appear as visible.
        for version_id in version_ids {
            if let Some(version) = historical.get_edge_version(version_id)
                && version.temporal.is_visible_at(valid_time, transaction_time)
            {
                let properties = historical.reconstruct_edge_properties(version_id)?;

                return Ok(Edge::new(
                    version.edge_id,
                    version.label,
                    version.source,
                    version.target,
                    properties,
                    version.id,
                ));
            }
        }

        // No visible version found
        Err(StorageError::EdgeNotFound(edge_id).into())
    }

    /// Get multiple nodes as they existed at a specific point in bi-temporal space.
    ///
    /// This is more efficient than calling `get_node_at_time` in a loop because it
    /// acquires the historical storage lock only once for all queries.
    ///
    /// **Note**: This implementation is similar to `get_edges_at_time()`. While the
    /// duplication could be eliminated with a generic helper function, we keep them
    /// separate for clarity and maintainability, as the type-specific operations
    /// (Node vs Edge construction) would require complex trait bounds.
    ///
    /// # Arguments
    ///
    /// * `node_ids` - Slice of node IDs to query
    /// * `valid_time` - The valid time to query
    /// * `transaction_time` - The transaction time to query
    ///
    /// # Returns
    ///
    /// A vector of (NodeId, `Option<Node>`) pairs, where None indicates the node
    /// did not exist at the specified time. Results are returned in the same
    /// order as the input node_ids.
    ///
    /// # Error Handling
    ///
    /// Unlike `get_node_at_time()` which returns an error when a node is not found,
    /// this batch API returns `None` for individual nodes that don't exist at the
    /// specified time. This allows partial results when querying multiple nodes.
    /// The method only returns `Err` for systemic failures (lock poisoning, storage
    /// corruption, property reconstruction failures).
    ///
    /// # Duplicate Handling
    ///
    /// If `node_ids` contains duplicate IDs, each will be processed independently
    /// and appear in the results. No deduplication is performed. The caller is
    /// responsible for deduplication if needed.
    ///
    /// # Performance Characteristics
    ///
    /// The current implementation holds a read lock on historical storage for the
    /// entire batch processing duration, including property reconstruction. This
    /// design prioritizes simplicity and correctness for the initial implementation.
    ///
    /// For very large batches (1000+ entities), consider:
    /// - Breaking the batch into smaller chunks
    /// - Using this method when temporal consistency across the batch is required
    ///
    /// Future optimization: A two-phase approach (gather version IDs, then reconstruct)
    /// could reduce lock hold time for better concurrency, at the cost of additional
    /// lock acquisitions.
    ///
    /// # Example
    ///
    /// ```ignore
    /// // Query 100 nodes at a historical point with single lock acquisition
    /// let node_ids = vec![node1, node2, /* ... */, node100];
    /// let results = db.get_nodes_at_time(&node_ids, valid_time, tx_time)?;
    ///
    /// for (node_id, node_opt) in results {
    ///     if let Some(node) = node_opt {
    ///         println!("Node {} existed with properties: {:?}", node_id, node.properties());
    ///     } else {
    ///         println!("Node {} did not exist at this time", node_id);
    ///     }
    /// }
    /// ```
    pub fn get_nodes_at_time(
        &self,
        node_ids: &[NodeId],
        valid_time: Timestamp,
        transaction_time: Timestamp,
    ) -> Result<Vec<(NodeId, Option<Node>)>> {
        #[cfg(feature = "observability")]
        let _span = tracing::info_span!("get_nodes_at_time").entered();

        // Single lock acquisition for all queries
        let historical = self.historical.read_or_err()?;

        // Process each node ID, propagating errors properly
        node_ids
            .iter()
            .map(|&node_id| {
                // Use temporal index for efficient O(log n) candidate lookup (Issue #194 fix)
                // Use iterator API to avoid Vec allocation when only first match is needed (Issue #197)
                let node = self
                    .temporal_indexes
                    .find_node_version_at_point_iter(node_id, valid_time, transaction_time)
                    .find_map(|version_id| {
                        match historical.get_node_version(version_id) {
                            Some(version)
                                if version.temporal.is_visible_at(valid_time, transaction_time) =>
                            {
                                // Reconstruct properties - return None on error to continue to next candidate
                                // Note: Batch queries treat reconstruction errors as "not found" to allow
                                // partial results. Errors are logged when observability is enabled.
                                match historical.reconstruct_node_properties(version_id) {
                                    Ok(properties) => Some(Node::new(
                                        version.node_id,
                                        version.label,
                                        properties,
                                        version.id,
                                    )),
                                    Err(_e) => {
                                        #[cfg(feature = "observability")]
                                        tracing::error!(
                                            version_id = %version_id,
                                            node_id = %node_id,
                                            error = %_e,
                                            "Property reconstruction failed in batch query"
                                        );
                                        None
                                    }
                                }
                            }
                            _ => None, // Version missing or not visible - continue to next candidate
                        }
                    });

                Ok((node_id, node))
            })
            .collect()
    }

    /// Get multiple edges as they existed at a specific point in bi-temporal space.
    ///
    /// This is more efficient than calling `get_edge_at_time` in a loop because it
    /// acquires the historical storage lock only once for all queries.
    ///
    /// # Arguments
    ///
    /// * `edge_ids` - Slice of edge IDs to query
    /// * `valid_time` - The valid time to query
    /// * `transaction_time` - The transaction time to query
    ///
    /// # Returns
    ///
    /// A vector of (EdgeId, `Option<Edge>`) pairs, where None indicates the edge
    /// did not exist at the specified time. Results are returned in the same
    /// order as the input edge_ids.
    ///
    /// # Error Handling
    ///
    /// Unlike `get_edge_at_time()` which returns an error when an edge is not found,
    /// this batch API returns `None` for individual edges that don't exist at the
    /// specified time. This allows partial results when querying multiple edges.
    /// The method only returns `Err` for systemic failures (lock poisoning, storage
    /// corruption, property reconstruction failures).
    ///
    /// # Duplicate Handling
    ///
    /// If `edge_ids` contains duplicate IDs, each will be processed independently
    /// and appear in the results. No deduplication is performed. The caller is
    /// responsible for deduplication if needed.
    ///
    /// # Performance Characteristics
    ///
    /// The current implementation holds a read lock on historical storage for the
    /// entire batch processing duration, including property reconstruction. This
    /// design prioritizes simplicity and correctness for the initial implementation.
    ///
    /// For very large batches (1000+ entities), consider:
    /// - Breaking the batch into smaller chunks
    /// - Using this method when temporal consistency across the batch is required
    ///
    /// Future optimization: A two-phase approach (gather version IDs, then reconstruct)
    /// could reduce lock hold time for better concurrency, at the cost of additional
    /// lock acquisitions.
    ///
    /// # Example
    ///
    /// ```ignore
    /// // Query multiple edges at a historical point with single lock acquisition
    /// let edge_ids = vec![edge1, edge2, edge3];
    /// let results = db.get_edges_at_time(&edge_ids, valid_time, tx_time)?;
    ///
    /// for (edge_id, edge_opt) in results {
    ///     if let Some(edge) = edge_opt {
    ///         println!("Edge {} existed: {} -> {}", edge_id, edge.source, edge.target);
    ///     } else {
    ///         println!("Edge {} did not exist at this time", edge_id);
    ///     }
    /// }
    /// ```
    pub fn get_edges_at_time(
        &self,
        edge_ids: &[EdgeId],
        valid_time: Timestamp,
        transaction_time: Timestamp,
    ) -> Result<Vec<(EdgeId, Option<Edge>)>> {
        #[cfg(feature = "observability")]
        let _span = tracing::info_span!("get_edges_at_time").entered();

        // Single lock acquisition for all queries
        let historical = self.historical.read_or_err()?;

        // Process each edge ID, propagating errors properly
        edge_ids
            .iter()
            .map(|&edge_id| {
                // Use temporal index for efficient O(log n) candidate lookup (Issue #194 fix)
                // Use iterator API to avoid Vec allocation when only first match is needed (Issue #197)
                let edge = self
                    .temporal_indexes
                    .find_edge_version_at_point_iter(edge_id, valid_time, transaction_time)
                    .find_map(|version_id| {
                        match historical.get_edge_version(version_id) {
                            Some(version)
                                if version.temporal.is_visible_at(valid_time, transaction_time) =>
                            {
                                // Reconstruct properties - return None on error to continue to next candidate
                                // Note: Batch queries treat reconstruction errors as "not found" to allow
                                // partial results. Errors are logged when observability is enabled.
                                match historical.reconstruct_edge_properties(version_id) {
                                    Ok(properties) => Some(Edge::new(
                                        version.edge_id,
                                        version.label,
                                        version.source,
                                        version.target,
                                        properties,
                                        version.id,
                                    )),
                                    Err(_e) => {
                                        #[cfg(feature = "observability")]
                                        tracing::error!(
                                            version_id = %version_id,
                                            edge_id = %edge_id,
                                            error = %_e,
                                            "Property reconstruction failed in batch query"
                                        );
                                        None
                                    }
                                }
                            }
                            _ => None, // Version missing or not visible - continue to next candidate
                        }
                    });

                Ok((edge_id, edge))
            })
            .collect()
    }

    // ========================================================================
    // Vector Indexing API (VS-030)
    // ========================================================================

    /// Enable vector indexing for a specific property.
    ///
    /// Once enabled, nodes with the specified property will be automatically
    /// indexed for similarity search. The property must contain vector values.
    ///
    /// # Arguments
    ///
    /// * `property_name` - Name of the property containing vectors
    /// * `config` - HNSW index configuration (dimensions, metric, etc.)
    ///
    /// # Example
    ///
    /// ```ignore
    /// use gallifreydb::index::vector::{HnswConfig, DistanceMetric};
    ///
    /// let config = HnswConfig::new(384, DistanceMetric::Cosine);
    /// db.enable_vector_index("embedding", config)?;
    /// ```
    ///
    /// # Errors
    ///
    /// Returns an error if vector indexing is already enabled.
    pub fn enable_vector_index(&self, property_name: &str, config: HnswConfig) -> Result<()> {
        #[cfg(feature = "observability")]
        let _span = tracing::info_span!("enable_vector_index").entered();
        self.current.enable_vector_index(property_name, config)
    }

    /// Check if vector indexing is enabled.
    pub fn is_vector_index_enabled(&self) -> bool {
        self.current.is_vector_index_enabled()
    }

    /// Check if vector indexing is enabled for a specific property.
    pub fn is_vector_index_enabled_for(&self, property_name: &str) -> bool {
        self.current.is_vector_index_enabled_for(property_name)
    }

    /// Enable temporal vector indexing for a specific property.
    ///
    /// Once enabled, vector changes will be tracked over time using snapshot-based
    /// indexing, enabling point-in-time vector queries and semantic drift tracking.
    /// This also integrates with the historical storage's observer pattern to create
    /// vector snapshots aligned with graph anchors.
    ///
    /// # Arguments
    ///
    /// * `property_name` - Name of the property containing vectors
    /// * `config` - Temporal vector index configuration
    ///
    /// # Example
    ///
    /// ```ignore
    /// use gallifreydb::index::vector::temporal::{TemporalVectorConfig, SnapshotStrategy};
    /// use gallifreydb::index::vector::HnswConfig;
    ///
    /// let hnsw_config = HnswConfig::new(384, DistanceMetric::Cosine);
    /// let temporal_config = TemporalVectorConfig::default_with_hnsw(hnsw_config);
    /// db.enable_temporal_vector_index("embedding", temporal_config)?;
    /// ```
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - Temporal vector indexing is already enabled
    /// - The historical storage lock is poisoned
    pub fn enable_temporal_vector_index(
        &self,
        property_name: &str,
        config: TemporalVectorConfig,
    ) -> Result<()> {
        // Resolve hnsw_config: use provided config or get from existing vector index
        let resolved_hnsw_config =
            if let Some(hnsw_config) = config.hnsw_config.clone() {
                // Config was provided explicitly
                hnsw_config
            } else if self.current.is_vector_index_enabled_for(property_name) {
                // No config provided, but vector index exists - use its config
                self.current.get_hnsw_config_for(property_name).ok_or_else(|| {
                crate::utils::error::Error::Vector(crate::utils::error::VectorError::IndexError(
                    format!(
                        "Vector index exists for '{}' but could not retrieve its configuration",
                        property_name
                    ),
                ))
            })?
            } else {
                // No config provided and no vector index exists - error
                return Err(crate::utils::error::Error::Vector(
                    crate::utils::error::VectorError::IndexError(
                        "HNSW configuration is required when no vector index exists. \
                     Use TemporalVectorConfig::default_with_hnsw() to provide one, \
                     or enable the vector index first with enable_vector_index()."
                            .to_string(),
                    ),
                ));
            };

        // Enable vector index if it doesn't exist yet
        if !self.current.is_vector_index_enabled_for(property_name) {
            self.enable_vector_index(property_name, resolved_hnsw_config.clone())?;
        }

        #[cfg(feature = "observability")]
        let _span = tracing::info_span!("enable_temporal_vector_index").entered();

        // Create a resolved config with the hnsw_config set
        let resolved_config = TemporalVectorConfig {
            hnsw_config: Some(resolved_hnsw_config),
            ..config
        };

        // Enable temporal vector index in current storage
        self.current
            .enable_temporal_vector_index(property_name, resolved_config)?;

        // Get the temporal vector index from current storage
        let temporal_index = self.current.get_temporal_vector_index().ok_or_else(|| {
            crate::utils::error::Error::Vector(crate::utils::error::VectorError::IndexError(
                "Temporal vector index not found after enabling".to_string(),
            ))
        })?;

        // Register pre-anchor hooks with historical storage (for strong consistency)
        // Both node and edge hooks perform the same action, so we create one and clone it
        let hook: crate::storage::historical::PreAnchorHook = {
            let index = Arc::clone(&temporal_index);
            Arc::new(move |_entity_type, _entity_id, timestamp, _properties| {
                index.create_snapshot_for_anchor(timestamp)
            })
        };

        let node_hook = Arc::clone(&hook);
        let edge_hook = hook;

        let mut historical = self.historical.write();

        historical.register_pre_node_anchor_hook(node_hook);
        historical.register_pre_edge_anchor_hook(edge_hook);

        // Create observer and register with historical storage (for extensibility)
        let observer = VectorIndexObserver::new(temporal_index);
        historical.add_observer(std::sync::Arc::new(observer));

        Ok(())
    }

    /// Check if temporal vector indexing is enabled.
    pub fn is_temporal_vector_index_enabled(&self) -> bool {
        self.current.is_temporal_vector_index_enabled()
    }

    /// List all property names that have temporal vector indexes enabled.
    ///
    /// Returns a vector of property names that have temporal vector indexing configured.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let db = GallifreyDB::new();
    /// // Enable temporal indexes for two properties
    /// db.vector_index("embedding1").hnsw(config1).temporal(temporal_config).enable()?;
    /// db.vector_index("embedding2").hnsw(config2).temporal(temporal_config).enable()?;
    ///
    /// let indexes = db.list_temporal_vector_indexes();
    /// assert!(indexes.contains(&"embedding1".to_string()));
    /// assert!(indexes.contains(&"embedding2".to_string()));
    /// ```
    pub fn list_temporal_vector_indexes(&self) -> Vec<String> {
        self.current.list_temporal_vector_indexes()
    }

    /// Create a builder for configuring a vector index on a property.
    ///
    /// This provides a fluent API for enabling vector indexes with optional
    /// temporal configuration. The builder pattern ensures proper configuration
    /// before enabling the index.
    ///
    /// # Arguments
    ///
    /// * `property_name` - The property name that will contain vector embeddings
    ///
    /// # Example
    ///
    /// ```ignore
    /// use gallifreydb::index::vector::{HnswConfig, DistanceMetric};
    ///
    /// // Basic vector index
    /// db.vector_index("embedding")
    ///     .hnsw(HnswConfig::new(384, DistanceMetric::Cosine))
    ///     .enable()?;
    ///
    /// // With temporal indexing for time-travel queries
    /// db.vector_index("embedding")
    ///     .hnsw(HnswConfig::new(384, DistanceMetric::Cosine))
    ///     .temporal(TemporalVectorConfig::default())
    ///     .enable()?;
    /// ```
    pub fn vector_index(&self, property_name: &str) -> VectorIndexBuilder<'_> {
        VectorIndexBuilder::new(self, property_name.to_string())
    }

    /// Check if a vector index is enabled for a specific property.
    ///
    /// # Arguments
    ///
    /// * `property_name` - The property name to check
    ///
    /// # Example
    ///
    /// ```ignore
    /// db.vector_index("embedding")
    ///     .hnsw(config)
    ///     .enable()?;
    ///
    /// assert!(db.has_vector_index("embedding"));
    /// assert!(!db.has_vector_index("other_property"));
    /// ```
    pub fn has_vector_index(&self, property_name: &str) -> bool {
        self.current.has_vector_index(property_name)
    }

    /// List all enabled vector indexes.
    ///
    /// Returns information about each configured vector index including
    /// the property name, dimensions, and distance metric.
    ///
    /// # Example
    ///
    /// ```ignore
    /// db.vector_index("title_embedding")
    ///     .hnsw(HnswConfig::new(384, DistanceMetric::Cosine))
    ///     .enable()?;
    ///
    /// db.vector_index("body_embedding")
    ///     .hnsw(HnswConfig::new(768, DistanceMetric::Euclidean))
    ///     .enable()?;
    ///
    /// let indexes = db.list_vector_indexes();
    /// assert_eq!(indexes.len(), 2);
    /// ```
    pub fn list_vector_indexes(&self) -> Vec<crate::storage::VectorIndexInfo> {
        self.current.list_vector_indexes()
    }

    /// Find k most similar nodes in a specific property's vector index.
    ///
    /// Use this method when you have multiple vector indexes and need to
    /// search a specific one. The query node's embedding from the specified
    /// property is used for the search.
    ///
    /// # Arguments
    ///
    /// * `property_name` - The indexed property to search
    /// * `query_node_id` - The node to find similar nodes for
    /// * `k` - Maximum number of results to return
    ///
    /// # Example
    ///
    /// ```ignore
    /// // Search title embeddings for similar nodes
    /// let similar = db.find_similar_in("title_embedding", node_id, 10)?;
    ///
    /// // Search body embeddings (different property, potentially different results)
    /// let similar_body = db.find_similar_in("body_embedding", node_id, 10)?;
    /// ```
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - No vector index is enabled for the specified property
    /// - Query node is not found
    /// - Query node does not have the specified vector property
    pub fn find_similar_in(
        &self,
        property_name: &str,
        query_node_id: NodeId,
        k: usize,
    ) -> Result<Vec<(NodeId, f32)>> {
        #[cfg(feature = "observability")]
        let _span = tracing::info_span!("find_similar_in").entered();
        self.current
            .find_similar_in(property_name, query_node_id, k)
    }

    /// Search a specific property's vector index with a raw embedding.
    ///
    /// Use this method when searching with embeddings that don't correspond to
    /// any existing node in the graph (e.g., query embeddings from external sources).
    ///
    /// # Arguments
    ///
    /// * `property_name` - The indexed property to search
    /// * `embedding` - The query embedding vector
    /// * `k` - Maximum number of results to return
    ///
    /// # Example
    ///
    /// ```ignore
    /// // Search with external embedding
    /// let query = embed_text("search query");
    /// let results = db.search_vectors_in("title_embedding", &query, 10)?;
    /// ```
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - No vector index is enabled for the specified property
    /// - Embedding dimensions don't match the index configuration
    pub fn search_vectors_in(
        &self,
        property_name: &str,
        embedding: &[f32],
        k: usize,
    ) -> Result<Vec<(NodeId, f32)>> {
        #[cfg(feature = "observability")]
        let _span = tracing::info_span!("search_vectors_in").entered();
        self.current.search_vectors_in(property_name, embedding, k)
    }

    /// Find k most similar nodes to a query node based on vector similarity.
    ///
    /// Returns a list of (NodeId, score) pairs sorted by similarity (highest first).
    /// The query node itself is excluded from results.
    ///
    /// # Arguments
    ///
    /// * `query_node_id` - The node to find similar nodes for
    /// * `k` - Maximum number of results to return
    ///
    /// # Example
    ///
    /// ```ignore
    /// // Find the 5 most similar documents to a given document
    /// let results = db.find_similar(doc_id, 5)?;
    /// for (node_id, score) in results {
    ///     println!("Similar node {} with score {}", node_id, score);
    /// }
    /// ```
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - Vector index is not enabled
    /// - Query node is not found
    /// - Query node does not have the indexed vector property
    pub fn find_similar(&self, query_node_id: NodeId, k: usize) -> Result<Vec<(NodeId, f32)>> {
        #[cfg(feature = "observability")]
        let _span = tracing::info_span!("find_similar").entered();
        self.current.find_similar(query_node_id, k)
    }

    /// Find k most similar nodes with a specific label.
    ///
    /// This is useful for finding similar nodes within a category, e.g.,
    /// "find similar documents" or "find similar users".
    ///
    /// # Arguments
    ///
    /// * `query_node_id` - The node to find similar nodes for
    /// * `label` - Only return nodes with this label
    /// * `k` - Maximum number of results to return
    ///
    /// # Example
    ///
    /// ```ignore
    /// // Find similar Person nodes only
    /// let similar_people = db.find_similar_with_label(person_id, "Person", 10)?;
    /// ```
    pub fn find_similar_with_label(
        &self,
        query_node_id: NodeId,
        label: &str,
        k: usize,
    ) -> Result<Vec<(NodeId, f32)>> {
        #[cfg(feature = "observability")]
        let _span = tracing::info_span!("find_similar_with_label").entered();
        self.current
            .find_similar_with_label(query_node_id, label, k)
    }

    /// Find k most similar nodes to a raw embedding vector.
    ///
    /// This is useful when searching with embeddings that don't correspond to any
    /// existing node in the graph, such as query embeddings from external sources
    /// or user input.
    ///
    /// # Arguments
    ///
    /// * `embedding` - The query embedding vector
    /// * `k` - Maximum number of results to return
    ///
    /// # Returns
    ///
    /// A list of (NodeId, similarity_score) pairs sorted by similarity (highest first).
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - Vector index is not enabled
    /// - Embedding dimensions don't match the indexed property
    ///
    /// # Example
    ///
    /// ```ignore
    /// // Search with an embedding from external source (e.g., user query)
    /// let query_embedding = get_embedding_from_llm("rust programming");
    /// let similar = db.find_similar_by_embedding(&query_embedding, 10)?;
    /// for (node_id, similarity) in similar {
    ///     println!("Node {:?} has similarity {}", node_id, similarity);
    /// }
    /// ```
    pub fn find_similar_by_embedding(
        &self,
        embedding: &[f32],
        k: usize,
    ) -> Result<Vec<(NodeId, f32)>> {
        #[cfg(feature = "observability")]
        let _span = tracing::info_span!("find_similar_by_embedding").entered();
        self.current.find_similar_by_embedding(embedding, k)
    }

    /// Find k most similar nodes with a specific label to a raw embedding vector.
    ///
    /// Like `find_similar_by_embedding()`, but filters results to only include
    /// nodes with the specified label.
    ///
    /// # Arguments
    ///
    /// * `embedding` - The query embedding vector
    /// * `label` - Only return nodes with this label
    /// * `k` - Maximum number of results to return
    ///
    /// # Returns
    ///
    /// A list of (NodeId, similarity_score) pairs sorted by similarity (highest first).
    /// All results have the specified label.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - Vector index is not enabled
    /// - Embedding dimensions don't match the indexed property
    ///
    /// # Example
    ///
    /// ```ignore
    /// // Find similar documents only
    /// let query_embedding = get_embedding_from_llm("rust programming");
    /// let similar_docs = db.find_similar_by_embedding_with_label(
    ///     &query_embedding,
    ///     "Document",
    ///     5
    /// )?;
    /// ```
    pub fn find_similar_by_embedding_with_label(
        &self,
        embedding: &[f32],
        label: &str,
        k: usize,
    ) -> Result<Vec<(NodeId, f32)>> {
        #[cfg(feature = "observability")]
        let _span = tracing::info_span!("find_similar_by_embedding_with_label").entered();
        #[cfg(feature = "observability")]
        let _span = tracing::info_span!("find_similar_by_embedding").entered();
        #[cfg(feature = "observability")]
        let _span = tracing::info_span!("find_similar").entered();
        self.current
            .find_similar_by_embedding_with_label(embedding, label, k)
    }

    /// Find k most similar nodes at a specific point in time.
    ///
    /// This method performs a temporal vector search, finding nodes with embeddings
    /// most similar to the query embedding as they existed at the specified timestamp.
    ///
    /// # Arguments
    ///
    /// * `embedding` - Query embedding vector to search for
    /// * `k` - Maximum number of results to return
    /// * `timestamp` - Point in time to query (in microseconds since epoch)
    ///
    /// # Returns
    ///
    /// A vector of (NodeId, similarity_score) tuples, sorted by similarity in descending order.
    ///
    /// # Errors
    ///
    /// - `Error::Vector(VectorError::IndexError)` if temporal vector index is not enabled
    /// - `Error::Vector(VectorError::*)` if the query embedding is invalid
    /// - `Error::Temporal(*)` if no snapshot exists at the given timestamp
    ///
    /// # Example
    ///
    /// ```ignore
    /// // Find documents similar to a query at a specific point in time
    /// let timestamp_2023 = 1672531200000000; // 2023-01-01 in microseconds
    /// let results = db.find_similar_as_of(&query_embedding, 10, timestamp_2023)?;
    /// ```
    pub fn find_similar_as_of(
        &self,
        embedding: &[f32],
        k: usize,
        timestamp: Timestamp,
    ) -> Result<Vec<(NodeId, f32)>> {
        #[cfg(feature = "observability")]
        let _span = tracing::info_span!("find_similar_as_of").entered();
        self.current.find_similar_as_of(embedding, k, timestamp)
    }

    /// Find similar vectors at a specific point in time for a specific property.
    ///
    /// This is the property-specific version of [`find_similar_as_of()`].
    /// It validates that the requested property matches the property for which
    /// the temporal vector index was enabled.
    ///
    /// # Arguments
    ///
    /// * `property_name` - The property containing the vector embeddings
    /// * `embedding` - Query vector to find similar vectors to
    /// * `k` - Number of results to return
    /// * `timestamp` - The point in time to query (in microseconds since epoch)
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - Temporal vector index is not enabled
    /// - The property name doesn't match the indexed property
    /// - Query embedding dimensions don't match
    /// - No snapshot exists at the given timestamp
    ///
    /// # Example
    ///
    /// ```ignore
    /// // Find documents similar to a query at a specific point in time
    /// let timestamp_2023 = 1672531200000000; // 2023-01-01 in microseconds
    /// let results = db.find_similar_as_of_in(
    ///     "content_embedding",
    ///     &query_embedding,
    ///     10,
    ///     timestamp_2023
    /// )?;
    /// ```
    pub fn find_similar_as_of_in(
        &self,
        property_name: &str,
        embedding: &[f32],
        k: usize,
        timestamp: Timestamp,
    ) -> Result<Vec<(NodeId, f32)>> {
        #[cfg(feature = "observability")]
        let _span =
            tracing::info_span!("find_similar_as_of_in", property = property_name).entered();
        self.current
            .find_similar_as_of_in(property_name, embedding, k, timestamp)
    }

    /// Track semantic drift for a node over time in a specific property's temporal index.
    ///
    /// This method tracks how a node's embedding has changed relative to a reference
    /// embedding over time. It validates that the requested property matches the
    /// property for which the temporal vector index was enabled.
    ///
    /// # Arguments
    ///
    /// * `property_name` - The property containing the vector embeddings
    /// * `node_id` - The node to track drift for
    /// * `reference_embedding` - Reference vector to measure drift against
    /// * `time_range` - The time range to search for drift
    ///
    /// # Returns
    ///
    /// A vector of (timestamp, drift_score) pairs showing how the node's embedding
    /// drifted from the reference at each snapshot time.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - Temporal vector index is not enabled
    /// - The property name doesn't match the indexed property
    /// - Reference embedding dimensions don't match
    ///
    /// # Example
    ///
    /// ```ignore
    /// use gallifreydb::core::temporal::TimeRange;
    ///
    /// // Track how a document's embedding changed from its original version
    /// let time_range = TimeRange::new(0.into(), i64::MAX.into()).unwrap();
    /// let drift = db.track_drift_in(
    ///     "content_embedding",
    ///     node_id,
    ///     &original_embedding,
    ///     time_range
    /// )?;
    ///
    /// for (timestamp, distance) in drift {
    ///     println!("At {}: drift = {:.3}", timestamp, distance);
    /// }
    /// ```
    pub fn track_drift_in(
        &self,
        property_name: &str,
        node_id: NodeId,
        reference_embedding: &[f32],
        time_range: crate::core::temporal::TimeRange,
    ) -> Result<Vec<(Timestamp, f32)>> {
        #[cfg(feature = "observability")]
        let _span =
            tracing::info_span!("track_drift_in", property = property_name, node = ?node_id)
                .entered();
        self.current
            .track_drift_in(property_name, node_id, reference_embedding, time_range)
    }

    /// Get the semantic evolution of a node's embedding over time in a specific property.
    ///
    /// Returns the actual embedding vectors at each snapshot timestamp, allowing
    /// you to see how the node's semantic representation changed over time.
    ///
    /// # Arguments
    ///
    /// * `property_name` - The property containing the vector embeddings
    /// * `node_id` - The node to get evolution for
    /// * `time_range` - The time range to query
    ///
    /// # Returns
    ///
    /// A vector of (timestamp, embedding) pairs showing the node's embedding
    /// at each snapshot time within the range.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - Temporal vector index is not enabled
    /// - The property name doesn't match the indexed property
    ///
    /// # Example
    ///
    /// ```ignore
    /// use gallifreydb::core::temporal::TimeRange;
    ///
    /// let time_range = TimeRange::new(0.into(), i64::MAX.into()).unwrap();
    /// let evolution = db.semantic_evolution_in("content_embedding", node_id, time_range)?;
    ///
    /// for (timestamp, embedding) in evolution {
    ///     println!("At {}: {} dimensions", timestamp, embedding.len());
    /// }
    /// ```
    pub fn semantic_evolution_in(
        &self,
        property_name: &str,
        node_id: NodeId,
        time_range: crate::core::temporal::TimeRange,
    ) -> Result<Vec<(Timestamp, std::sync::Arc<[f32]>)>> {
        #[cfg(feature = "observability")]
        let _span =
            tracing::info_span!("semantic_evolution_in", property = property_name, node = ?node_id)
                .entered();
        self.current
            .semantic_evolution_in(property_name, node_id, time_range)
    }

    /// Find all nodes with semantic drift above a threshold in a specific property.
    ///
    /// Scans all nodes in the temporal index and identifies those whose embeddings
    /// have changed by more than the specified threshold over the time range.
    ///
    /// # Arguments
    ///
    /// * `property_name` - The property containing the vector embeddings
    /// * `threshold` - Minimum drift distance to include in results
    /// * `time_range` - The time range to analyze
    /// * `metric` - The distance metric to use for drift calculation
    ///
    /// # Returns
    ///
    /// A vector of (node_id, drift_score) pairs for nodes exceeding the threshold,
    /// sorted by drift score in descending order.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - Temporal vector index is not enabled
    /// - The property name doesn't match the indexed property
    /// - Threshold is NaN or infinite
    ///
    /// # Example
    ///
    /// ```ignore
    /// use gallifreydb::core::temporal::TimeRange;
    /// use gallifreydb::index::vector::temporal::DriftMetric;
    ///
    /// let time_range = TimeRange::new(start_ts.into(), end_ts.into()).unwrap();
    /// let drifted = db.find_drift_in(
    ///     "content_embedding",
    ///     0.3,  // threshold
    ///     time_range,
    ///     DriftMetric::Cosine
    /// )?;
    ///
    /// for (node_id, drift) in drifted {
    ///     println!("Node {} drifted by {:.3}", node_id, drift);
    /// }
    /// ```
    pub fn find_drift_in(
        &self,
        property_name: &str,
        threshold: f32,
        time_range: crate::core::temporal::TimeRange,
        metric: crate::index::vector::temporal::DriftMetric,
    ) -> Result<Vec<(NodeId, f32)>> {
        #[cfg(feature = "observability")]
        let _span = tracing::info_span!(
            "find_drift_in",
            property = property_name,
            threshold = threshold
        )
        .entered();
        self.current
            .find_drift_in(property_name, threshold, time_range, metric)
    }

    // ========================================================================
    // Hybrid Query Planner API (VS-060)
    // ========================================================================

    /// Create a new query builder for constructing hybrid queries.
    ///
    /// This is the entry point for the fluent query API that enables
    /// combining graph traversal, vector search, and temporal queries.
    ///
    /// # Example
    ///
    /// ```ignore
    /// // Graph + Vector: "Who does Alice know that's similar to Bob?"
    /// let results = db.query()
    ///     .start(alice_id)
    ///     .traverse("KNOWS")
    ///     .rank_by_similarity(&bob_embedding, 10)
    ///     .build();
    ///
    /// let results = db.execute_query(query)?;
    ///
    /// // Temporal + Vector: "What was similar in 2023?"
    /// let query = db.query()
    ///     .as_of(timestamp_2023, tx_time)
    ///     .find_similar(&embedding, 10)
    ///     .build();
    /// ```
    #[must_use]
    pub fn query(&self) -> QueryBuilder<Initial> {
        QueryBuilder::new()
    }

    /// Execute a query and return the results.
    ///
    /// This method plans and executes the query using the hybrid query planner.
    /// The planner applies optimization rules and chooses the best execution
    /// strategy based on cost estimation.
    ///
    /// # Arguments
    ///
    /// * `query` - The query to execute
    ///
    /// # Example
    ///
    /// ```ignore
    /// let query = db.query()
    ///     .start(alice_id)
    ///     .traverse("KNOWS")
    ///     .rank_by_similarity(&embedding, 10)
    ///     .build();
    ///
    /// let results = db.execute_query(query)?;
    /// for row in results {
    ///     println!("{:?}", row);
    /// }
    /// ```
    pub fn execute_query(&self, query: Query) -> Result<QueryResults> {
        #[cfg(feature = "observability")]
        let _span = tracing::info_span!("execute_query").entered();

        // Use cached statistics for cost-based optimization
        // Statistics are shared across all queries for this database instance
        let planner = QueryPlanner::new(Arc::clone(&self.stats), Arc::clone(&self.current));
        let physical_plan = planner.plan(query)?;

        // Execute the plan
        let executor = QueryExecutor::new(Arc::clone(&self.current), Arc::clone(&self.historical));

        executor.execute(physical_plan)
    }

    /// Traverse from a node and rank results by similarity to an embedding.
    ///
    /// This is a convenience method for a common hybrid query pattern:
    /// "Find nodes connected to X that are similar to Y."
    ///
    /// # Arguments
    ///
    /// * `source` - The starting node for traversal
    /// * `edge_label` - Edge type to traverse (e.g., "KNOWS")
    /// * `embedding` - Target embedding to rank by similarity
    /// * `k` - Maximum number of results to return
    ///
    /// # Example
    ///
    /// ```ignore
    /// // "Who does Alice know that's similar to Bob?"
    /// let results = db.traverse_and_rank(
    ///     alice_id,
    ///     "KNOWS",
    ///     &bob_embedding,
    ///     10
    /// )?;
    ///
    /// for row in results {
    ///     println!("Found: {:?}", row.node_id);
    /// }
    /// ```
    pub fn traverse_and_rank(
        &self,
        source: NodeId,
        edge_label: &str,
        embedding: &[f32],
        k: usize,
    ) -> Result<QueryResults> {
        #[cfg(feature = "observability")]
        let _span = tracing::info_span!("traverse_and_rank").entered();

        let query = self
            .query()
            .start(source)
            .traverse(edge_label)
            .rank_by_similarity(embedding, k)
            .build();

        self.execute_query(query)
    }

    /// Find similar nodes at a specific point in time.
    ///
    /// This is a convenience method for temporal vector queries:
    /// "What was similar to this embedding at time T?"
    ///
    /// # Arguments
    ///
    /// * `embedding` - Query embedding
    /// * `k` - Maximum number of results
    /// * `valid_time` - Valid time for the query
    /// * `transaction_time` - Transaction time for the query
    ///
    /// # Example
    ///
    /// ```ignore
    /// // "What concepts were similar to this in 2023?"
    /// let results = db.find_similar_at_time(
    ///     &query_embedding,
    ///     10,
    ///     timestamp_2023,
    ///     timestamp_2023
    /// )?;
    /// ```
    pub fn find_similar_at_time(
        &self,
        embedding: &[f32],
        k: usize,
        valid_time: Timestamp,
        transaction_time: Timestamp,
    ) -> Result<QueryResults> {
        #[cfg(feature = "observability")]
        let _span = tracing::info_span!("find_similar_at_time").entered();

        let query = self
            .query()
            .as_of(valid_time, transaction_time)
            .find_similar(embedding, k)
            .build();

        self.execute_query(query)
    }

    /// Execute a full hybrid query combining graph, vector, and temporal.
    ///
    /// This is a convenience method for the most complex query pattern:
    /// "Who did X know at time T that was similar to Y?"
    ///
    /// # Arguments
    ///
    /// * `source` - Starting node for traversal
    /// * `edge_label` - Edge type to traverse
    /// * `embedding` - Target embedding to rank by similarity
    /// * `k` - Maximum number of results
    /// * `valid_time` - Valid time for the query
    /// * `transaction_time` - Transaction time for the query
    ///
    /// # Example
    ///
    /// ```ignore
    /// // "Who did Alice know in 2023 that was similar to Bob?"
    /// let results = db.traverse_and_rank_at_time(
    ///     alice_id,
    ///     "KNOWS",
    ///     &bob_embedding,
    ///     10,
    ///     timestamp_2023,
    ///     timestamp_2023
    /// )?;
    /// ```
    pub fn traverse_and_rank_at_time(
        &self,
        source: NodeId,
        edge_label: &str,
        embedding: &[f32],
        k: usize,
        valid_time: Timestamp,
        transaction_time: Timestamp,
    ) -> Result<QueryResults> {
        #[cfg(feature = "observability")]
        let _span = tracing::info_span!("traverse_and_rank_at_time").entered();

        let query = self
            .query()
            .as_of(valid_time, transaction_time)
            .start(source)
            .traverse(edge_label)
            .rank_by_similarity(embedding, k)
            .build();

        self.execute_query(query)
    }

    /// Get the number of nodes in the current state.
    #[inline]
    pub fn node_count(&self) -> usize {
        self.current.node_count()
    }

    /// Get the number of edges in the current state.
    #[inline]
    pub fn edge_count(&self) -> usize {
        self.current.edge_count()
    }

    /// Get the out-degree of a node (current state).
    #[inline]
    pub fn out_degree(&self, node_id: NodeId) -> usize {
        self.current.out_degree(node_id)
    }

    /// Get the in-degree of a node (current state).
    #[inline]
    pub fn in_degree(&self, node_id: NodeId) -> usize {
        self.current.in_degree(node_id)
    }

    /// Get statistics about the historical storage.
    ///
    /// # Errors
    ///
    /// Returns an error if the historical storage lock is poisoned.
    pub fn historical_stats(&self) -> Result<crate::storage::historical::HistoricalStats> {
        Ok(self.historical.read_or_err()?.stats())
    }

    /// Persist all indexes to disk.
    ///
    /// This saves the current state of all indexes (graph, temporal, vector, strings)
    /// to disk in the configured persistence directory.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - Index persistence is not enabled in configuration
    /// - Writing index files fails due to I/O errors
    /// - Index serialization fails
    ///
    /// # Example
    ///
    /// ```ignore
    /// let db = GallifreyDB::new();
    /// // ... add data ...
    /// db.persist_indexes()?; // Save indexes to disk
    /// ```
    pub fn persist_indexes(&self) -> Result<()> {
        use crate::storage::index_persistence::formats::IndexManifest;
        use crate::storage::index_persistence::formats::{PersistedEdge, PersistedNode};
        use crate::storage::index_persistence::graph::{
            new_graph_index_data, persist_property_map, save_graph_index,
        };

        // Warn if background persistence thread has stopped
        if self
            .persistence_thread_stopped
            .load(std::sync::atomic::Ordering::Acquire)
        {
            eprintln!(
                "Warning: Background persistence thread has stopped. \
                 Automatic persistence is disabled. Manual persist_indexes() calls will still work."
            );
        }

        let manager =
            self.persistence_manager
                .as_ref()
                .ok_or_else(|| StorageError::InconsistentState {
                    reason: "Index persistence not enabled".to_string(),
                })?;

        // 1. Save string interner first (dependency for all others)
        manager.save_string_interner().map_err(|e| {
            StorageError::PersistenceError(format!("Failed to save string interner: {}", e))
        })?;

        // 2. Save graph index
        let mut graph_data = new_graph_index_data();

        // Export all nodes
        let all_nodes = self.current.all_nodes();
        for node in all_nodes {
            let label_idx = node.label.as_u32();
            let properties = persist_property_map(&node.properties).map_err(|e| {
                StorageError::PersistenceError(format!("Failed to persist node properties: {}", e))
            })?;

            graph_data.nodes.push(PersistedNode {
                id: node.id.as_u64(),
                label_idx,
                version_id: node.current_version.as_u64(),
                properties,
            });
        }

        // Export all edges
        let all_edges = self.current.all_edges();
        for edge in all_edges {
            let label_idx = edge.label.as_u32();
            let properties = persist_property_map(&edge.properties).map_err(|e| {
                StorageError::PersistenceError(format!("Failed to persist edge properties: {}", e))
            })?;

            graph_data.edges.push(PersistedEdge {
                id: edge.id.as_u64(),
                source_id: edge.source.as_u64(),
                target_id: edge.target.as_u64(),
                label_idx,
                version_id: edge.current_version.as_u64(),
                properties,
            });
        }

        graph_data.node_count = graph_data.nodes.len() as u64;
        graph_data.edge_count = graph_data.edges.len() as u64;

        save_graph_index(&graph_data, &manager.graph_path().join("adjacency.idx")).map_err(
            |e| StorageError::PersistenceError(format!("Failed to save graph index: {}", e)),
        )?;

        // 3. Save vector indexes
        if let Some(ref tracker) = self.persistence_tracker {
            persist_vector_indexes(&self.current, manager, Some(tracker))?;
        }

        // 4. Save temporal index (version history)
        if let Some(ref tracker) = self.persistence_tracker {
            persist_temporal_index(&self.historical, &self.temporal_indexes, manager, tracker)?;
        }

        // 5. Save manifest last with current WAL LSN
        // Note: This records the WAL position at persist time for future WAL replay coordination
        let current_lsn = self.wal.current_lsn().0;
        let manifest = IndexManifest::new(current_lsn);
        manager.save_manifest(&manifest).map_err(|e| {
            StorageError::PersistenceError(format!("Failed to save manifest: {}", e))
        })?;

        Ok(())
    }

    /// Get a reference to the current storage (test-only helper).
    ///
    /// This method is only available in test builds and provides access to the
    /// internal CurrentStorage for integration test verification purposes.
    #[cfg(test)]
    #[allow(dead_code)]
    pub(crate) fn storage(&self) -> &Arc<CurrentStorage> {
        &self.current
    }

    /// Get the current WAL LSN (test-only helper).
    ///
    /// This method provides access to the current WAL Log Sequence Number for
    /// test verification purposes. This is particularly useful for testing index
    /// persistence where LSN coordination with the WAL is critical for correctness.
    ///
    /// **Warning**: This method exposes internal implementation details and
    /// should only be used in tests.
    ///
    /// # Returns
    ///
    /// The current LSN from the WAL system.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let db = GallifreyDB::new();
    /// db.create_node("Person", properties)?;
    /// let lsn = db.__test_current_wal_lsn();
    /// assert!(lsn > 0); // LSN advances after operations
    /// ```
    #[doc(hidden)]
    pub fn __test_current_wal_lsn(&self) -> u64 {
        self.wal.current_lsn().0
    }

    /// Access the internal HistoricalStorage for testing purposes.
    ///
    /// This method provides access to the internal HistoricalStorage for
    /// integration test verification purposes. It is public to allow access from
    /// integration tests but is hidden from documentation and marked with
    /// `__test_` prefix to discourage production use.
    ///
    /// **Warning**: This method exposes internal implementation details and
    /// should only be used in tests.
    #[doc(hidden)]
    pub fn __test_historical_storage(&self) -> &Arc<RwLock<HistoricalStorage>> {
        &self.historical
    }

    /// Provide test-only access to temporal indexes for performance testing.
    ///
    /// This allows tests to verify that temporal indexes are populated correctly
    /// and can query them directly. This is marked as `#[doc(hidden)]` and
    /// should only be used in tests.
    #[doc(hidden)]
    pub fn __test_temporal_indexes(&self) -> &Arc<TemporalIndexes> {
        &self.temporal_indexes
    }

    /// Get adaptive over-fetch statistics for a label (test-only helper).
    ///
    /// Returns the current statistics (search_count, total_candidates, total_results)
    /// for the given label, or None if no searches have been performed yet.
    ///
    /// This is used for testing to verify that adaptive learning is working correctly.
    ///
    /// **Warning**: This method exposes internal implementation details and
    /// should only be used in tests.
    ///
    /// # Returns
    ///
    /// Some((search_count, total_candidates, total_results)) if statistics exist,
    /// None otherwise.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let db = GallifreyDB::new()?;
    /// db.enable_vector_index("embedding", config)?;
    /// // ... create nodes and perform searches ...
    /// let (count, candidates, results) = db.__test_get_filter_stats("Person").unwrap();
    /// assert_eq!(count, 10); // 10 searches performed
    /// ```
    #[doc(hidden)]
    pub fn __test_get_filter_stats(&self, label: &str) -> Option<(u64, u64, u64)> {
        self.current.get_filter_stats(label)
    }

    /// Get the query optimization statistics.
    ///
    /// Statistics are used for cost-based query optimization and are cached
    /// across queries for efficiency. The statistics are automatically refreshed
    /// when needed, but can be manually refreshed using [`refresh_statistics`](Self::refresh_statistics).
    ///
    /// # Returns
    ///
    /// A reference to the shared statistics object.
    pub fn statistics(&self) -> &Arc<Statistics> {
        &self.stats
    }

    /// Refresh query optimization statistics from current storage.
    ///
    /// This collects fresh statistics about node counts, edge counts, label
    /// cardinalities, and other metrics used for cost-based query optimization.
    /// Call this method after significant schema changes or data modifications
    /// to ensure the query planner has accurate information.
    ///
    /// Statistics are automatically refreshed lazily on first query, so this
    /// method is typically only needed for benchmarking or after bulk imports.
    ///
    /// # Example
    ///
    /// ```ignore
    /// // After bulk import
    /// for doc in documents {
    ///     db.create_node("Document", doc.properties)?;
    /// }
    ///
    /// // Refresh statistics for optimal query planning
    /// db.refresh_statistics();
    ///
    /// // Now queries will use accurate statistics
    /// let results = db.execute_query(query)?;
    /// ```
    pub fn refresh_statistics(&self) {
        // Collect statistics from current storage
        let node_count = self.current.node_count();
        let edge_count = self.current.edge_count();
        let vector_count = self.current.vector_count();

        // Collect label counts from current storage
        let label_counts = self.current.label_counts();

        // Calculate average delta chain length from historical storage
        // (using default estimate if historical storage is empty)
        // See issue #366: Calculate from historical storage instead of hardcoding
        let avg_delta_chain = 5.0;

        self.stats.refresh(
            node_count,
            edge_count,
            vector_count,
            label_counts,
            avg_delta_chain,
        );
    }

    /// Invalidate cached query optimization statistics.
    ///
    /// Call this after schema changes to force re-collection of statistics
    /// on the next query. The statistics will be lazily refreshed when needed.
    pub fn invalidate_statistics(&self) {
        self.stats.invalidate();
    }

    /// Compress the MVCC commit log to reduce memory usage (Issue #237).
    ///
    /// This applies epoch-based compression to the transaction visibility manager's
    /// commit log, converting sequential transaction ranges into compressed epochs.
    /// Achieves 10-100x memory reduction for typical workloads with sequential
    /// transaction patterns.
    ///
    /// # When to Call
    ///
    /// - Periodically during bulk imports (e.g., every 10K commits)
    /// - During idle periods
    /// - Before checkpointing
    ///
    /// # Performance
    ///
    /// O(N log N) where N is the number of uncompressed transactions.
    /// Relatively expensive, so should not be called on every commit.
    ///
    /// # Example
    ///
    /// ```ignore
    /// // After bulk import
    /// for i in 0..100000 {
    ///     db.create_node("Node", props)?;
    /// }
    ///
    /// // Compress commit log to free memory
    /// db.compress_commit_log();
    /// ```
    pub fn compress_commit_log(&self) {
        self.visibility_manager.compress_commit_log();
    }

    /// Get memory usage of the MVCC commit log in bytes.
    ///
    /// This reports the current memory footprint of the transaction commit log
    /// in the visibility manager. Useful for monitoring and triggering compression.
    ///
    /// # Returns
    ///
    /// Memory usage in bytes
    pub fn commit_log_memory_usage(&self) -> usize {
        self.visibility_manager.commit_log_memory_usage()
    }

    /// Get detailed compression statistics for the MVCC commit log (Issue #237).
    ///
    /// Returns statistics about:
    /// - Total transactions tracked
    /// - Number of compressed epochs
    /// - Number of exception entries
    /// - Compression ratio
    /// - Memory usage and savings
    ///
    /// # Returns
    ///
    /// Compression statistics
    ///
    /// # Example
    ///
    /// ```ignore
    /// let stats = db.get_compression_stats();
    /// println!("Compression ratio: {}x", stats.compression_ratio);
    /// println!("Memory saved: {} bytes", stats.memory_saved_bytes);
    /// ```
    pub fn get_compression_stats(&self) -> crate::api::transaction::visibility::CompressionStats {
        self.visibility_manager.get_compression_stats()
    }

    /// Check if commit log compression should be triggered based on memory threshold.
    ///
    /// This is a convenience method to help decide when to call `compress_commit_log()`.
    /// Returns `true` if the current commit log memory usage exceeds the threshold.
    ///
    /// # Arguments
    ///
    /// * `threshold_bytes` - Memory threshold in bytes
    ///
    /// # Returns
    ///
    /// `true` if compression is recommended, `false` otherwise
    ///
    /// # Example
    ///
    /// ```ignore
    /// // After bulk import, compress if using > 10MB
    /// if db.should_compress_commit_log(10 * 1024 * 1024) {
    ///     db.compress_commit_log();
    /// }
    /// ```
    pub fn should_compress_commit_log(&self, threshold_bytes: usize) -> bool {
        self.visibility_manager
            .should_compress_commit_log(threshold_bytes)
    }

    /// Check if compression should be triggered based on exception count.
    ///
    /// This is an alternative trigger mechanism that compresses when there are
    /// many uncompressed exceptions (indicating potential for compression).
    ///
    /// # Arguments
    ///
    /// * `threshold_exceptions` - Number of exceptions to trigger compression
    ///
    /// # Returns
    ///
    /// `true` if compression is recommended, `false` otherwise
    ///
    /// # Example
    ///
    /// ```ignore
    /// // Compress every 50K commits
    /// if db.should_compress_by_exception_count(50_000) {
    ///     db.compress_commit_log();
    /// }
    /// ```
    pub fn should_compress_by_exception_count(&self, threshold_exceptions: usize) -> bool {
        self.visibility_manager
            .should_compress_by_exception_count(threshold_exceptions)
    }
}

impl Drop for GallifreyDB {
    fn drop(&mut self) {
        // Signal shutdown to background persistence thread
        if let Some(ref tracker) = self.persistence_tracker {
            tracker.signal_shutdown();

            // Give the thread a moment to finish and persist on shutdown
            // The background thread will call persist_all_indexes() before exiting
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
    }
}

// Note: Default is intentionally NOT implemented for GallifreyDB because:
// 1. Construction can fail (WAL initialization may fail)
// 2. Users should explicitly call GallifreyDB::new()? to handle potential errors
// 3. This follows CODING_STANDARDS.md which prohibits .expect() in production code

/// Builder for configuring and enabling a vector index on a property.
///
/// This builder provides a fluent API for setting up vector indexes with
/// HNSW configuration and optional temporal indexing. The builder pattern
/// ensures that required configuration (HNSW) is provided before enabling.
///
/// # Example
///
/// ```ignore
/// use gallifreydb::index::vector::{HnswConfig, DistanceMetric};
///
/// // Create a basic vector index
/// db.vector_index("embedding")
///     .hnsw(HnswConfig::new(384, DistanceMetric::Cosine))
///     .enable()?;
///
/// // Create a vector index with temporal support
/// db.vector_index("embedding")
///     .hnsw(HnswConfig::new(384, DistanceMetric::Cosine))
///     .temporal(TemporalVectorConfig::default())
///     .enable()?;
/// ```
pub struct VectorIndexBuilder<'a> {
    db: &'a GallifreyDB,
    property_name: String,
    hnsw_config: Option<HnswConfig>,
    temporal_config: Option<TemporalVectorConfig>,
}

impl<'a> VectorIndexBuilder<'a> {
    /// Create a new builder for the specified property.
    fn new(db: &'a GallifreyDB, property_name: String) -> Self {
        VectorIndexBuilder {
            db,
            property_name,
            hnsw_config: None,
            temporal_config: None,
        }
    }

    /// Set the HNSW index configuration.
    ///
    /// This is required before calling `enable()`. The configuration specifies
    /// the vector dimensions, distance metric, and HNSW parameters.
    ///
    /// # Arguments
    ///
    /// * `config` - The HNSW index configuration
    ///
    /// # Example
    ///
    /// ```ignore
    /// db.vector_index("embedding")
    ///     .hnsw(HnswConfig::new(384, DistanceMetric::Cosine).with_capacity(10000))
    ///     .enable()?;
    /// ```
    pub fn hnsw(mut self, config: HnswConfig) -> Self {
        self.hnsw_config = Some(config);
        self
    }

    /// Enable temporal vector indexing.
    ///
    /// When temporal indexing is enabled, vector snapshots are created
    /// periodically to enable point-in-time vector queries and semantic
    /// drift tracking. This automatically enables the current index as well.
    ///
    /// # Arguments
    ///
    /// * `config` - The temporal vector index configuration
    ///
    /// # Example
    ///
    /// ```ignore
    /// use gallifreydb::index::vector::temporal::{TemporalVectorConfig, SnapshotStrategy};
    ///
    /// db.vector_index("embedding")
    ///     .hnsw(HnswConfig::new(384, DistanceMetric::Cosine))
    ///     .temporal(TemporalVectorConfig {
    ///         snapshot_strategy: SnapshotStrategy::TransactionInterval(100),
    ///         ..Default::default()
    ///     })
    ///     .enable()?;
    /// ```
    pub fn temporal(mut self, config: TemporalVectorConfig) -> Self {
        self.temporal_config = Some(config);
        self
    }

    /// Enable the vector index with the configured settings.
    ///
    /// This method validates the configuration and enables the index.
    /// If temporal configuration was provided, both the current and
    /// temporal indexes are enabled.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - HNSW configuration was not provided (call `.hnsw()` first)
    /// - A vector index already exists for this property
    /// - The temporal index configuration is invalid
    ///
    /// # Example
    ///
    /// ```ignore
    /// // This will fail - no HNSW config
    /// let result = db.vector_index("embedding").enable();
    /// assert!(result.is_err());
    ///
    /// // This will succeed
    /// db.vector_index("embedding")
    ///     .hnsw(HnswConfig::new(384, DistanceMetric::Cosine))
    ///     .enable()?;
    /// ```
    pub fn enable(self) -> Result<()> {
        let hnsw_config = self.hnsw_config.ok_or_else(|| {
            crate::utils::error::Error::Vector(crate::utils::error::VectorError::IndexError(
                "HNSW configuration is required. Call .hnsw() before .enable()".to_string(),
            ))
        })?;

        // If temporal config is provided, use enable_temporal_vector_index which
        // automatically enables the current index as well (fixing #386)
        if let Some(temporal_config) = self.temporal_config {
            // Merge HNSW config into temporal config
            let temporal_config_with_hnsw = TemporalVectorConfig {
                hnsw_config: Some(hnsw_config),
                ..temporal_config
            };
            self.db
                .enable_temporal_vector_index(&self.property_name, temporal_config_with_hnsw)
        } else {
            // Just enable the current index
            self.db
                .enable_vector_index(&self.property_name, hnsw_config)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::transaction::ReadOps;
    use crate::core::property::PropertyMapBuilder;

    #[test]
    fn test_create_node() {
        let db = GallifreyDB::new().unwrap();

        let props = PropertyMapBuilder::new()
            .insert("name", "Alice")
            .insert("age", 30i64)
            .build();

        let node_id = db.create_node("Person", props).unwrap();

        assert_eq!(db.node_count(), 1);

        let node = db.get_node(node_id).unwrap();
        assert_eq!(node.id, node_id);
        assert_eq!(
            node.get_property("name").and_then(|v| v.as_str()),
            Some("Alice")
        );
    }

    #[test]
    fn test_create_edge() {
        let db = GallifreyDB::new().unwrap();

        let alice = db
            .create_node("Person", PropertyMapBuilder::new().build())
            .unwrap();
        let bob = db
            .create_node("Person", PropertyMapBuilder::new().build())
            .unwrap();

        let edge_id = db
            .create_edge(
                alice,
                bob,
                "KNOWS",
                PropertyMapBuilder::new().insert("since", 2020i64).build(),
            )
            .unwrap();

        assert_eq!(db.edge_count(), 1);

        let edge = db.get_edge(edge_id).unwrap();
        assert_eq!(edge.source, alice);
        assert_eq!(edge.target, bob);
    }

    #[test]
    fn test_time_travel_query() {
        let db = GallifreyDB::new().unwrap();

        // Create a node at time T1
        let props_v1 = PropertyMapBuilder::new()
            .insert("name", "Alice")
            .insert("age", 30i64)
            .build();

        let node_id = db.create_node("Person", props_v1).unwrap();

        // Capture timestamp after creation (wallclock time)
        std::thread::sleep(std::time::Duration::from_micros(100));
        let t1 = crate::core::temporal::time::now();

        // In a real implementation, we'd create a second version here with an update_node method
        // For now, just verify we can query at T1

        // Query at time T1 (after node was created)
        let historical_node = db.get_node_at_time(node_id, t1, t1).unwrap();
        assert_eq!(
            historical_node.get_property("age").and_then(|v| v.as_int()),
            Some(30)
        );

        // Query current state
        let current_node = db.get_node(node_id).unwrap();
        assert_eq!(
            current_node.get_property("age").and_then(|v| v.as_int()),
            Some(30)
        );
    }

    #[test]
    fn test_time_travel_after_deletion() {
        let db = GallifreyDB::new().unwrap();

        // Create a node
        let props = PropertyMapBuilder::new()
            .insert("name", "Alice")
            .insert("age", 30i64)
            .build();
        let node_id = db.create_node("Person", props).unwrap();

        // Record timestamp after creation (wallclock time)
        std::thread::sleep(std::time::Duration::from_micros(100));
        let t_after_create = crate::core::temporal::time::now();

        // Delete the node
        std::thread::sleep(std::time::Duration::from_micros(100));
        db.write(|tx| {
            tx.delete_node(node_id)?;
            Ok(())
        })
        .unwrap();

        // Record timestamp after deletion (wallclock time)
        std::thread::sleep(std::time::Duration::from_micros(100));
        let t_after_delete = crate::core::temporal::time::now();

        // Query BEFORE creation - should fail (node didn't exist)
        // Note: We can't easily test this without more control over timestamps

        // Query AFTER deletion - should fail (node was deleted)
        // This is the critical test: time-travel query after deletion should NOT
        // return the deleted node's data
        let result = db.get_node_at_time(node_id, t_after_delete, t_after_delete);
        assert!(
            result.is_err(),
            "Expected NodeNotFound after deletion, but got: {:?}",
            result
        );

        // Query BEFORE deletion - should succeed (node existed)
        let result = db.get_node_at_time(node_id, t_after_create, t_after_create);
        assert!(
            result.is_ok(),
            "Expected to find node before deletion, but got: {:?}",
            result
        );
        let node = result.unwrap();
        assert_eq!(
            node.get_property("name").and_then(|v| v.as_str()),
            Some("Alice")
        );
    }

    /// Test that verifies temporal index + historical storage visibility check interaction.
    ///
    /// This is a critical integration test for Issue #194: the temporal index stores intervals
    /// at insertion time, but deletions close the valid_time in historical storage. The query
    /// path must verify visibility with historical storage to correctly reject deleted nodes.
    #[test]
    fn test_temporal_index_deletion_integration() {
        let db = GallifreyDB::new().unwrap();

        // Create a node
        let props = PropertyMapBuilder::new().insert("name", "TestNode").build();
        let node_id = db.create_node("TestLabel", props).unwrap();

        // Record timestamp after creation
        std::thread::sleep(std::time::Duration::from_micros(100));
        let t_after_create = crate::core::temporal::time::now();

        // Verify node is queryable after creation
        let result = db.get_node_at_time(node_id, t_after_create, t_after_create);
        assert!(
            result.is_ok(),
            "Node should be queryable after creation: {:?}",
            result
        );

        // Verify temporal index has the version
        let version_ids =
            db.temporal_indexes
                .find_node_version_at_point(node_id, t_after_create, t_after_create);
        assert!(
            !version_ids.is_empty(),
            "Temporal index should return candidates for existing node"
        );

        // Delete the node
        std::thread::sleep(std::time::Duration::from_micros(100));
        db.write(|tx| {
            tx.delete_node(node_id)?;
            Ok(())
        })
        .unwrap();

        // Record timestamp after deletion
        std::thread::sleep(std::time::Duration::from_micros(100));
        let t_after_delete = crate::core::temporal::time::now();

        // CRITICAL: Temporal index may still return the version (it stores insertion-time intervals)
        // but the query should correctly reject it via historical storage visibility check
        let version_ids_after =
            db.temporal_indexes
                .find_node_version_at_point(node_id, t_after_delete, t_after_delete);
        // Note: Temporal index might still return candidates - that's expected
        // The visibility check in get_node_at_time should filter them out

        // Query after deletion should fail
        let result = db.get_node_at_time(node_id, t_after_delete, t_after_delete);
        assert!(
            result.is_err(),
            "Query after deletion should fail. Temporal index returned {:?} candidates, \
             but historical visibility check should reject them. Got: {:?}",
            version_ids_after.len(),
            result
        );

        // Query at time BEFORE deletion should still work
        let result = db.get_node_at_time(node_id, t_after_create, t_after_create);
        assert!(
            result.is_ok(),
            "Query before deletion should succeed: {:?}",
            result
        );
    }

    #[test]
    fn test_graph_traversal() {
        let db = GallifreyDB::new().unwrap();

        let n0 = db
            .create_node("Person", PropertyMapBuilder::new().build())
            .unwrap();
        let n1 = db
            .create_node("Person", PropertyMapBuilder::new().build())
            .unwrap();
        let n2 = db
            .create_node("Person", PropertyMapBuilder::new().build())
            .unwrap();

        db.create_edge(n0, n1, "KNOWS", PropertyMapBuilder::new().build())
            .unwrap();
        db.create_edge(n0, n2, "KNOWS", PropertyMapBuilder::new().build())
            .unwrap();

        let outgoing = db.get_outgoing_edges(n0);
        assert_eq!(outgoing.len(), 2);

        let knows_edges = db.get_outgoing_edges_with_label(n0, "KNOWS");
        assert_eq!(knows_edges.len(), 2);
    }

    #[test]
    fn test_historical_stats() {
        let db = GallifreyDB::new().unwrap();

        db.create_node("Person", PropertyMapBuilder::new().build())
            .unwrap();
        db.create_node("Person", PropertyMapBuilder::new().build())
            .unwrap();

        let stats = db.historical_stats().unwrap();
        assert_eq!(stats.total_node_versions, 2);
        assert_eq!(stats.node_anchor_count, 2); // First versions are always anchors
    }

    // ==================== Transaction API Tests ====================

    #[test]
    fn test_closure_based_write_api() {
        let db = GallifreyDB::new().unwrap();

        // Use closure-based API for multiple operations
        let (node_id, edge_id) = db
            .write(|tx| {
                let n1 = tx.create_node(
                    "Person",
                    PropertyMapBuilder::new().insert("name", "Alice").build(),
                )?;
                let n2 = tx.create_node(
                    "Person",
                    PropertyMapBuilder::new().insert("name", "Bob").build(),
                )?;
                let e = tx.create_edge(
                    n1,
                    n2,
                    "KNOWS",
                    PropertyMapBuilder::new().insert("since", 2024i64).build(),
                )?;
                Ok((n1, e))
            })
            .unwrap();

        // Verify changes are visible
        assert_eq!(db.node_count(), 2);
        assert_eq!(db.edge_count(), 1);

        let node = db.get_node(node_id).unwrap();
        assert_eq!(
            node.get_property("name").and_then(|v| v.as_str()),
            Some("Alice")
        );

        let edge = db.get_edge(edge_id).unwrap();
        assert_eq!(edge.source, node_id);
    }

    #[test]
    fn test_closure_based_read_api() {
        let db = GallifreyDB::new().unwrap();

        let node_id = db
            .create_node(
                "Person",
                PropertyMapBuilder::new().insert("name", "Charlie").build(),
            )
            .unwrap();

        // Use closure-based read API
        let name = db
            .read(|tx| {
                let node = tx.get_node(node_id)?;
                Ok(node
                    .get_property("name")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string()))
            })
            .unwrap();

        assert_eq!(name, Some("Charlie".to_string()));
    }

    #[test]
    fn test_explicit_write_transaction() {
        let db = GallifreyDB::new().unwrap();

        let mut tx = db.write_transaction().unwrap();
        let n1 = tx
            .create_node(
                "Person",
                PropertyMapBuilder::new().insert("name", "David").build(),
            )
            .unwrap();
        let n2 = tx
            .create_node(
                "Person",
                PropertyMapBuilder::new().insert("name", "Eve").build(),
            )
            .unwrap();
        tx.create_edge(n1, n2, "KNOWS", PropertyMapBuilder::new().build())
            .unwrap();

        // Changes not visible before commit
        assert_eq!(db.node_count(), 0);

        // Commit
        tx.commit().unwrap();

        // Now visible
        assert_eq!(db.node_count(), 2);
        assert_eq!(db.edge_count(), 1);
    }

    #[test]
    fn test_explicit_read_transaction() {
        let db = GallifreyDB::new().unwrap();

        let node_id = db
            .create_node(
                "Person",
                PropertyMapBuilder::new().insert("age", 42i64).build(),
            )
            .unwrap();

        let tx = db.read_transaction().unwrap();
        let node = tx.get_node(node_id).unwrap();
        assert_eq!(node.get_property("age").and_then(|v| v.as_int()), Some(42));

        // Read transactions don't need commit
    }

    #[test]
    fn test_transaction_atomicity() {
        let db = GallifreyDB::new().unwrap();

        // Create a valid node first
        let valid_node = db
            .create_node("Person", PropertyMapBuilder::new().build())
            .unwrap();

        // Try to create multiple operations, one of which will fail
        let result = db.write(|tx| {
            tx.create_node("Person", PropertyMapBuilder::new().build())?;
            tx.create_node("Person", PropertyMapBuilder::new().build())?;
            // This should fail validation (non-existent target)
            tx.create_edge(
                valid_node,
                NodeId::new(9999).unwrap(),
                "KNOWS",
                PropertyMapBuilder::new().build(),
            )?;
            Ok(())
        });

        // Transaction should fail
        assert!(result.is_err());

        // No partial changes should be visible (atomicity)
        // We started with 1 node, should still have 1 node
        assert_eq!(db.node_count(), 1);
        assert_eq!(db.edge_count(), 0);
    }

    #[test]
    fn test_transaction_rollback_on_error() {
        let db = GallifreyDB::new().unwrap();

        // Closure returns an error - should auto-rollback
        let result: Result<()> = db.write(|tx| {
            tx.create_node("Person", PropertyMapBuilder::new().build())?;
            tx.create_node("Person", PropertyMapBuilder::new().build())?;
            // Manually return an error
            Err(crate::utils::error::Error::Storage(
                crate::utils::error::StorageError::InconsistentState {
                    reason: "test error".to_string(),
                },
            ))
        });

        assert!(result.is_err());

        // All changes rolled back
        assert_eq!(db.node_count(), 0);
    }

    #[test]
    fn test_multiple_transactions() {
        let db = GallifreyDB::new().unwrap();

        // Transaction 1
        let n1 = db
            .write(|tx| tx.create_node("Person", PropertyMapBuilder::new().build()))
            .unwrap();

        // Transaction 2
        let n2 = db
            .write(|tx| tx.create_node("Person", PropertyMapBuilder::new().build()))
            .unwrap();

        // Transaction 3
        db.write(|tx| tx.create_edge(n1, n2, "KNOWS", PropertyMapBuilder::new().build()))
            .unwrap();

        assert_eq!(db.node_count(), 2);
        assert_eq!(db.edge_count(), 1);
    }

    #[test]
    fn test_snapshot_isolation() {
        let db = GallifreyDB::new().unwrap();

        let node_id = db
            .create_node(
                "Person",
                PropertyMapBuilder::new().insert("version", 1i64).build(),
            )
            .unwrap();

        // Start a read transaction - captures snapshot
        let tx1 = db.read_transaction().unwrap();
        let node_v1 = tx1.get_node(node_id).unwrap();
        assert_eq!(
            node_v1.get_property("version").and_then(|v| v.as_int()),
            Some(1)
        );

        // Another write commits a change (creates a new node)
        let new_node_id = db
            .write(|tx| {
                tx.create_node(
                    "Person",
                    PropertyMapBuilder::new().insert("version", 2i64).build(),
                )
            })
            .unwrap();

        // Snapshot Isolation: tx1 should NOT see the new node
        // because it was created and committed after tx1's snapshot
        assert!(tx1.get_node(new_node_id).is_err());

        // Verify tx1 still sees the original node
        let node_v1_again = tx1.get_node(node_id).unwrap();
        assert_eq!(
            node_v1_again
                .get_property("version")
                .and_then(|v| v.as_int()),
            Some(1)
        );
    }

    // ==================== Vector Index API Tests ====================

    #[test]
    fn test_enable_vector_index() {
        use crate::index::vector::{DistanceMetric, HnswConfig};

        let db = GallifreyDB::new().unwrap();

        // Enable vector index
        let config = HnswConfig::new(3, DistanceMetric::Cosine).with_capacity(100);
        db.enable_vector_index("embedding", config).unwrap();

        // Trying to enable again should fail
        let config2 = HnswConfig::new(3, DistanceMetric::Cosine);
        assert!(db.enable_vector_index("embedding", config2).is_err());
    }

    #[test]
    fn test_find_similar_basic() {
        use crate::index::vector::{DistanceMetric, HnswConfig};

        let db = GallifreyDB::new().unwrap();

        // Enable vector index
        let config = HnswConfig::new(3, DistanceMetric::Cosine).with_capacity(100);
        db.enable_vector_index("embedding", config).unwrap();

        // Create nodes with vector embeddings
        let doc1 = db
            .create_node(
                "Document",
                PropertyMapBuilder::new()
                    .insert("title", "Rust Programming")
                    .insert_vector("embedding", &[1.0f32, 0.0, 0.0])
                    .build(),
            )
            .unwrap();

        let doc2 = db
            .create_node(
                "Document",
                PropertyMapBuilder::new()
                    .insert("title", "Rust Advanced")
                    .insert_vector("embedding", &[0.9f32, 0.1, 0.0])
                    .build(),
            )
            .unwrap();

        let doc3 = db
            .create_node(
                "Document",
                PropertyMapBuilder::new()
                    .insert("title", "Python Basics")
                    .insert_vector("embedding", &[0.0f32, 1.0, 0.0])
                    .build(),
            )
            .unwrap();

        // Find similar to doc1
        let similar = db.find_similar(doc1, 2).unwrap();

        // Should return 2 results (excluding doc1 itself)
        assert_eq!(similar.len(), 2);

        // doc2 should be most similar (both about Rust)
        assert_eq!(similar[0].0, doc2);
        assert!(similar[0].1 > 0.9); // High similarity

        // doc3 should be less similar
        assert_eq!(similar[1].0, doc3);
        assert!(similar[1].1 < 0.5); // Lower similarity
    }

    #[test]
    fn test_find_similar_with_label() {
        use crate::index::vector::{DistanceMetric, HnswConfig};

        let db = GallifreyDB::new().unwrap();

        // Enable vector index
        let config = HnswConfig::new(3, DistanceMetric::Cosine).with_capacity(100);
        db.enable_vector_index("embedding", config).unwrap();

        // Create Document nodes
        let doc1 = db
            .create_node(
                "Document",
                PropertyMapBuilder::new()
                    .insert_vector("embedding", &[1.0f32, 0.0, 0.0])
                    .build(),
            )
            .unwrap();

        let doc2 = db
            .create_node(
                "Document",
                PropertyMapBuilder::new()
                    .insert_vector("embedding", &[0.9f32, 0.1, 0.0])
                    .build(),
            )
            .unwrap();

        // Create Person nodes with similar embeddings
        let _person1 = db
            .create_node(
                "Person",
                PropertyMapBuilder::new()
                    .insert_vector("embedding", &[0.95f32, 0.05, 0.0])
                    .build(),
            )
            .unwrap();

        // Find similar Documents only (should exclude Person nodes)
        let similar = db.find_similar_with_label(doc1, "Document", 5).unwrap();

        // Should only return doc2 (not person1)
        assert_eq!(similar.len(), 1);
        assert_eq!(similar[0].0, doc2);
    }

    #[test]
    fn test_vector_index_not_enabled() {
        let db = GallifreyDB::new().unwrap();

        // Create node with vector
        let node_id = db
            .create_node(
                "Document",
                PropertyMapBuilder::new()
                    .insert_vector("embedding", &[1.0f32, 0.0, 0.0])
                    .build(),
            )
            .unwrap();

        // Try to search without enabling index - should fail
        assert!(db.find_similar(node_id, 5).is_err());
    }

    #[test]
    fn test_vector_index_with_euclidean_distance() {
        use crate::index::vector::{DistanceMetric, HnswConfig};

        let db = GallifreyDB::new().unwrap();

        // Enable vector index with Euclidean distance
        let config = HnswConfig::new(3, DistanceMetric::Euclidean).with_capacity(100);
        db.enable_vector_index("embedding", config).unwrap();

        // Create nodes with vector embeddings
        let doc1 = db
            .create_node(
                "Document",
                PropertyMapBuilder::new()
                    .insert_vector("embedding", &[0.0f32, 0.0, 0.0])
                    .build(),
            )
            .unwrap();

        let doc2 = db
            .create_node(
                "Document",
                PropertyMapBuilder::new()
                    .insert_vector("embedding", &[1.0f32, 0.0, 0.0])
                    .build(),
            )
            .unwrap();

        let doc3 = db
            .create_node(
                "Document",
                PropertyMapBuilder::new()
                    .insert_vector("embedding", &[10.0f32, 0.0, 0.0])
                    .build(),
            )
            .unwrap();

        // Find similar to doc1
        let similar = db.find_similar(doc1, 2).unwrap();

        assert_eq!(similar.len(), 2);

        // With Euclidean distance, doc2 (distance 1.0) should be closer than doc3 (distance 10.0)
        assert_eq!(similar[0].0, doc2);
        assert_eq!(similar[1].0, doc3);
    }

    #[test]
    fn test_vector_index_with_large_k() {
        use crate::index::vector::{DistanceMetric, HnswConfig};

        let db = GallifreyDB::new().unwrap();

        // Enable vector index
        let config = HnswConfig::new(3, DistanceMetric::Cosine).with_capacity(100);
        db.enable_vector_index("embedding", config).unwrap();

        // Create 5 nodes
        let mut node_ids = Vec::new();
        for i in 0..5 {
            let node_id = db
                .create_node(
                    "Document",
                    PropertyMapBuilder::new()
                        .insert_vector("embedding", &[i as f32, 0.0, 0.0])
                        .build(),
                )
                .unwrap();
            node_ids.push(node_id);
        }

        // Request k=10 (more than available)
        let similar = db.find_similar(node_ids[0], 10).unwrap();

        // Should return at most 4 results (5 total - 1 query node)
        assert!(similar.len() <= 4);
    }

    /// Regression test for VS-030 bug: nodes created via write transactions
    /// must be indexed for vector search.
    ///
    /// Prior to fix: insert_node_direct() only called indexes.insert_node(),
    /// skipping try_index_vector(). This meant all transaction-created nodes
    /// were missing from the HNSW index, causing find_similar to return empty results.
    #[test]
    fn test_transaction_nodes_are_indexed() {
        use crate::index::vector::{DistanceMetric, HnswConfig};

        let db = GallifreyDB::new().unwrap();

        // Enable vector index
        let config = HnswConfig::new(3, DistanceMetric::Cosine).with_capacity(100);
        db.enable_vector_index("embedding", config).unwrap();

        // Create nodes via write transaction (not convenience method)
        let (doc1, doc2, _doc3) = db
            .write(|tx| {
                let d1 = tx.create_node(
                    "Document",
                    PropertyMapBuilder::new()
                        .insert_vector("embedding", &[1.0f32, 0.0, 0.0])
                        .build(),
                )?;
                let d2 = tx.create_node(
                    "Document",
                    PropertyMapBuilder::new()
                        .insert_vector("embedding", &[0.9f32, 0.1, 0.0])
                        .build(),
                )?;
                let d3 = tx.create_node(
                    "Document",
                    PropertyMapBuilder::new()
                        .insert_vector("embedding", &[0.0f32, 1.0, 0.0])
                        .build(),
                )?;
                Ok((d1, d2, d3))
            })
            .unwrap();

        // CRITICAL: These nodes were created via transaction, not db.create_node()
        // Before the fix, insert_node_direct() didn't index vectors, so this would fail
        let similar = db.find_similar(doc1, 2).unwrap();

        // Should find doc2 and doc3
        assert_eq!(similar.len(), 2);
        assert_eq!(similar[0].0, doc2); // Most similar
        assert!(similar[0].1 > 0.9); // High similarity
    }

    #[test]
    fn test_find_similar_by_embedding() {
        use crate::index::vector::{DistanceMetric, HnswConfig};

        let db = GallifreyDB::new().unwrap();

        // Enable vector index
        let config = HnswConfig::new(3, DistanceMetric::Cosine).with_capacity(100);
        db.enable_vector_index("embedding", config).unwrap();

        // Create nodes with vector embeddings
        let doc1 = db
            .create_node(
                "Document",
                PropertyMapBuilder::new()
                    .insert("title", "Rust Programming")
                    .insert_vector("embedding", &[1.0f32, 0.0, 0.0])
                    .build(),
            )
            .unwrap();

        let doc2 = db
            .create_node(
                "Document",
                PropertyMapBuilder::new()
                    .insert("title", "Rust Advanced")
                    .insert_vector("embedding", &[0.9f32, 0.1, 0.0])
                    .build(),
            )
            .unwrap();

        let _doc3 = db
            .create_node(
                "Document",
                PropertyMapBuilder::new()
                    .insert("title", "Python Basics")
                    .insert_vector("embedding", &[0.0f32, 1.0, 0.0])
                    .build(),
            )
            .unwrap();

        // Search with an external query embedding (similar to doc1)
        let query_embedding = [0.95f32, 0.05, 0.0];
        let similar = db.find_similar_by_embedding(&query_embedding, 2).unwrap();

        // Should return doc1 first (most similar to query), then doc2
        assert_eq!(similar.len(), 2);
        assert_eq!(similar[0].0, doc1); // Most similar
        assert!(similar[0].1 > 0.99); // Very high similarity
        assert_eq!(similar[1].0, doc2);
    }

    #[test]
    fn test_find_similar_by_embedding_with_label() {
        use crate::index::vector::{DistanceMetric, HnswConfig};

        let db = GallifreyDB::new().unwrap();

        // Enable vector index
        let config = HnswConfig::new(3, DistanceMetric::Cosine).with_capacity(100);
        db.enable_vector_index("embedding", config).unwrap();

        // Create Document nodes
        let doc1 = db
            .create_node(
                "Document",
                PropertyMapBuilder::new()
                    .insert_vector("embedding", &[1.0f32, 0.0, 0.0])
                    .build(),
            )
            .unwrap();

        let doc2 = db
            .create_node(
                "Document",
                PropertyMapBuilder::new()
                    .insert_vector("embedding", &[0.9f32, 0.1, 0.0])
                    .build(),
            )
            .unwrap();

        // Create Person nodes with similar embeddings
        let _person1 = db
            .create_node(
                "Person",
                PropertyMapBuilder::new()
                    .insert_vector("embedding", &[0.95f32, 0.05, 0.0])
                    .build(),
            )
            .unwrap();

        // Search for Documents only with query embedding
        let query_embedding = [1.0f32, 0.0, 0.0];
        let similar = db
            .find_similar_by_embedding_with_label(&query_embedding, "Document", 5)
            .unwrap();

        // Should only return Documents (doc1 and doc2), not person1
        assert_eq!(similar.len(), 2);
        assert!(similar.iter().any(|(id, _)| *id == doc1));
        assert!(similar.iter().any(|(id, _)| *id == doc2));
    }

    #[test]
    fn test_find_similar_by_embedding_dimension_mismatch() {
        use crate::index::vector::{DistanceMetric, HnswConfig};

        let db = GallifreyDB::new().unwrap();

        // Enable vector index with 3 dimensions
        let config = HnswConfig::new(3, DistanceMetric::Cosine).with_capacity(100);
        db.enable_vector_index("embedding", config).unwrap();

        // Create a node
        db.create_node(
            "Document",
            PropertyMapBuilder::new()
                .insert_vector("embedding", &[1.0f32, 0.0, 0.0])
                .build(),
        )
        .unwrap();

        // Try to search with wrong dimensions (4 instead of 3)
        let wrong_embedding = [1.0f32, 0.0, 0.0, 0.0];
        let result = db.find_similar_by_embedding(&wrong_embedding, 5);

        // Should fail with dimension mismatch error
        assert!(result.is_err());
    }

    #[test]
    fn test_find_similar_empty_database() {
        use crate::index::vector::{DistanceMetric, HnswConfig};

        let db = GallifreyDB::new().unwrap();

        // Enable vector index but don't add any nodes
        let config = HnswConfig::new(3, DistanceMetric::Cosine).with_capacity(100);
        db.enable_vector_index("embedding", config).unwrap();

        // Search should return empty results, not error
        let query_embedding = [1.0f32, 0.0, 0.0];
        let results = db.find_similar_by_embedding(&query_embedding, 10).unwrap();

        assert_eq!(results.len(), 0);
    }

    #[test]
    fn test_find_similar_k_zero() {
        use crate::index::vector::{DistanceMetric, HnswConfig};

        let db = GallifreyDB::new().unwrap();

        // Enable vector index and add some nodes
        let config = HnswConfig::new(3, DistanceMetric::Cosine).with_capacity(100);
        db.enable_vector_index("embedding", config).unwrap();

        db.create_node(
            "Document",
            PropertyMapBuilder::new()
                .insert_vector("embedding", &[1.0f32, 0.0, 0.0])
                .build(),
        )
        .unwrap();

        // Search with k=0 should return empty results, not error
        let query_embedding = [1.0f32, 0.0, 0.0];
        let results = db.find_similar_by_embedding(&query_embedding, 0).unwrap();

        assert_eq!(results.len(), 0);
    }

    #[test]
    fn test_concurrent_vector_indexing() {
        use crate::index::vector::{DistanceMetric, HnswConfig};
        use std::sync::Arc;
        use std::thread;

        let db = Arc::new(GallifreyDB::new().unwrap());

        // Enable vector index
        let config = HnswConfig::new(4, DistanceMetric::Cosine).with_capacity(1000);
        db.enable_vector_index("embedding", config).unwrap();

        // Spawn multiple threads that create nodes with vectors concurrently
        let mut handles = vec![];
        for i in 0..10 {
            let db_clone = Arc::clone(&db);
            let handle = thread::spawn(move || {
                // Use non-zero vectors to avoid issues with cosine similarity
                let base = (i as f32 + 1.0) / 10.0;
                db_clone
                    .create_node(
                        "Document",
                        PropertyMapBuilder::new()
                            .insert_vector("embedding", &[base, base, base, base])
                            .build(),
                    )
                    .unwrap()
            });
            handles.push(handle);
        }

        // Wait for all threads to complete
        let node_ids: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();

        // Verify all nodes were indexed - search should return results
        // (Note: find_similar excludes the query node, so we check for OTHER nodes)
        for node_id in &node_ids {
            let results = db.find_similar(*node_id, 5).unwrap();
            // With 10 nodes and k=5, we should get at least 4 results (excluding query node)
            // HNSW is approximate, so we allow for slight variation
            assert!(
                results.len() >= 4,
                "Expected >=4 results for node {:?}, got {}",
                node_id,
                results.len()
            );
            // Verify results don't include the query node (it's excluded by design)
            assert!(
                results.iter().all(|(id, _)| *id != *node_id),
                "Query node {:?} should not appear in its own results",
                node_id
            );
            // Verify similarity scores are reasonable (between 0 and 1)
            for (_, score) in &results {
                assert!(
                    (0.0..=1.0).contains(score),
                    "Similarity score {} out of range",
                    score
                );
            }
        }

        // Verify total count
        assert_eq!(db.node_count(), 10);
    }

    // ==================== Batch Temporal Query Tests ====================

    #[test]
    fn test_get_nodes_at_time_basic() {
        let db = GallifreyDB::new().unwrap();

        // Create multiple nodes at time T1
        let props1 = PropertyMapBuilder::new()
            .insert("name", "Alice")
            .insert("age", 30i64)
            .build();
        let props2 = PropertyMapBuilder::new()
            .insert("name", "Bob")
            .insert("age", 25i64)
            .build();
        let props3 = PropertyMapBuilder::new()
            .insert("name", "Charlie")
            .insert("age", 35i64)
            .build();

        let node1 = db.create_node("Person", props1).unwrap();
        let node2 = db.create_node("Person", props2).unwrap();
        let node3 = db.create_node("Person", props3).unwrap();

        let t1 = *db.current_timestamp.lock().unwrap();

        // Query all three nodes at time T1 using batch API
        let node_ids = vec![node1, node2, node3];
        let results = db.get_nodes_at_time(&node_ids, t1, t1).unwrap();

        // Should return all three nodes
        assert_eq!(results.len(), 3);

        // Convert results to HashMap for easier verification
        let results_map: std::collections::HashMap<NodeId, Node> = results
            .into_iter()
            .map(|(id, node_opt)| (id, node_opt.expect("Node should exist")))
            .collect();

        assert_eq!(results_map.len(), 3);

        // Verify node1
        let n1 = results_map.get(&node1).unwrap();
        assert_eq!(n1.id, node1);
        assert_eq!(
            n1.get_property("name").and_then(|v| v.as_str()),
            Some("Alice")
        );

        // Verify node2
        let n2 = results_map.get(&node2).unwrap();
        assert_eq!(n2.id, node2);
        assert_eq!(
            n2.get_property("name").and_then(|v| v.as_str()),
            Some("Bob")
        );

        // Verify node3
        let n3 = results_map.get(&node3).unwrap();
        assert_eq!(n3.id, node3);
        assert_eq!(
            n3.get_property("name").and_then(|v| v.as_str()),
            Some("Charlie")
        );
    }

    #[test]
    fn test_get_nodes_at_time_mixed_results() {
        let db = GallifreyDB::new().unwrap();

        // Create two nodes
        let node1 = db
            .create_node(
                "Person",
                PropertyMapBuilder::new().insert("name", "Alice").build(),
            )
            .unwrap();
        let node2 = db
            .create_node(
                "Person",
                PropertyMapBuilder::new().insert("name", "Bob").build(),
            )
            .unwrap();

        let t1 = *db.current_timestamp.lock().unwrap();

        // Query including a non-existent node
        let non_existent = NodeId::new(9999).unwrap();
        let node_ids = vec![node1, non_existent, node2];
        let results = db.get_nodes_at_time(&node_ids, t1, t1).unwrap();

        // Should return 3 results, with one being None
        assert_eq!(results.len(), 3);

        // First result should be Some
        assert!(results[0].1.is_some());
        assert_eq!(results[0].0, node1);

        // Second result should be None (non-existent node)
        assert!(results[1].1.is_none());
        assert_eq!(results[1].0, non_existent);

        // Third result should be Some
        assert!(results[2].1.is_some());
        assert_eq!(results[2].0, node2);
    }

    #[test]
    fn test_get_nodes_at_time_empty_batch() {
        let db = GallifreyDB::new().unwrap();

        let t1 = *db.current_timestamp.lock().unwrap();

        // Query with empty node list
        let results = db.get_nodes_at_time(&[], t1, t1).unwrap();

        // Should return empty results
        assert_eq!(results.len(), 0);
    }

    #[test]
    fn test_get_nodes_at_time_after_deletion() {
        let db = GallifreyDB::new().unwrap();

        // Create nodes
        let node1 = db
            .create_node(
                "Person",
                PropertyMapBuilder::new().insert("name", "Alice").build(),
            )
            .unwrap();
        let node2 = db
            .create_node(
                "Person",
                PropertyMapBuilder::new().insert("name", "Bob").build(),
            )
            .unwrap();

        let t_after_create = *db.current_timestamp.lock().unwrap();

        // Delete node1
        db.write(|tx| {
            tx.delete_node(node1)?;
            Ok(())
        })
        .unwrap();

        let t_after_delete = *db.current_timestamp.lock().unwrap();

        // Query at time after deletion - node1 should not be found
        let node_ids = vec![node1, node2];
        let results = db
            .get_nodes_at_time(&node_ids, t_after_delete, t_after_delete)
            .unwrap();

        assert_eq!(results.len(), 2);
        assert!(results[0].1.is_none()); // node1 was deleted
        assert!(results[1].1.is_some()); // node2 still exists

        // Query at time before deletion - both should exist
        let results = db
            .get_nodes_at_time(&node_ids, t_after_create, t_after_create)
            .unwrap();

        assert_eq!(results.len(), 2);
        assert!(results[0].1.is_some()); // node1 existed
        assert!(results[1].1.is_some()); // node2 existed
    }

    #[test]
    fn test_get_edges_at_time_basic() {
        let db = GallifreyDB::new().unwrap();

        // Create nodes
        let alice = db
            .create_node("Person", PropertyMapBuilder::new().build())
            .unwrap();
        let bob = db
            .create_node("Person", PropertyMapBuilder::new().build())
            .unwrap();
        let charlie = db
            .create_node("Person", PropertyMapBuilder::new().build())
            .unwrap();

        // Create edges
        let edge1 = db
            .create_edge(
                alice,
                bob,
                "KNOWS",
                PropertyMapBuilder::new().insert("since", 2020i64).build(),
            )
            .unwrap();
        let edge2 = db
            .create_edge(
                bob,
                charlie,
                "KNOWS",
                PropertyMapBuilder::new().insert("since", 2021i64).build(),
            )
            .unwrap();
        let edge3 = db
            .create_edge(
                alice,
                charlie,
                "WORKS_WITH",
                PropertyMapBuilder::new().insert("since", 2022i64).build(),
            )
            .unwrap();

        let t1 = *db.current_timestamp.lock().unwrap();

        // Query all three edges at time T1 using batch API
        let edge_ids = vec![edge1, edge2, edge3];
        let results = db.get_edges_at_time(&edge_ids, t1, t1).unwrap();

        // Should return all three edges
        assert_eq!(results.len(), 3);

        // Convert results to HashMap for easier verification
        let results_map: std::collections::HashMap<EdgeId, Edge> = results
            .into_iter()
            .map(|(id, edge_opt)| (id, edge_opt.expect("Edge should exist")))
            .collect();

        assert_eq!(results_map.len(), 3);

        // Verify edge1
        let e1 = results_map.get(&edge1).unwrap();
        assert_eq!(e1.id, edge1);
        assert_eq!(e1.source, alice);
        assert_eq!(e1.target, bob);
        assert_eq!(
            e1.get_property("since").and_then(|v| v.as_int()),
            Some(2020)
        );

        // Verify edge2
        let e2 = results_map.get(&edge2).unwrap();
        assert_eq!(e2.id, edge2);
        assert_eq!(e2.source, bob);
        assert_eq!(e2.target, charlie);
        assert_eq!(
            e2.get_property("since").and_then(|v| v.as_int()),
            Some(2021)
        );

        // Verify edge3
        let e3 = results_map.get(&edge3).unwrap();
        assert_eq!(e3.id, edge3);
        assert_eq!(e3.source, alice);
        assert_eq!(e3.target, charlie);
        assert_eq!(
            e3.get_property("since").and_then(|v| v.as_int()),
            Some(2022)
        );
    }

    #[test]
    fn test_get_edges_at_time_mixed_results() {
        let db = GallifreyDB::new().unwrap();

        // Create nodes and edges
        let alice = db
            .create_node("Person", PropertyMapBuilder::new().build())
            .unwrap();
        let bob = db
            .create_node("Person", PropertyMapBuilder::new().build())
            .unwrap();

        let edge1 = db
            .create_edge(
                alice,
                bob,
                "KNOWS",
                PropertyMapBuilder::new().insert("since", 2020i64).build(),
            )
            .unwrap();

        let t1 = *db.current_timestamp.lock().unwrap();

        // Query including a non-existent edge
        let non_existent = EdgeId::new(9999).unwrap();
        let edge_ids = vec![edge1, non_existent];
        let results = db.get_edges_at_time(&edge_ids, t1, t1).unwrap();

        // Should return 2 results, with one being None
        assert_eq!(results.len(), 2);

        // First result should be Some
        assert!(results[0].1.is_some());
        assert_eq!(results[0].0, edge1);

        // Second result should be None (non-existent edge)
        assert!(results[1].1.is_none());
        assert_eq!(results[1].0, non_existent);
    }

    #[test]
    fn test_get_edges_at_time_empty_batch() {
        let db = GallifreyDB::new().unwrap();

        let t1 = *db.current_timestamp.lock().unwrap();

        // Query with empty edge list
        let results = db.get_edges_at_time(&[], t1, t1).unwrap();

        // Should return empty results
        assert_eq!(results.len(), 0);
    }

    #[test]
    fn test_get_edges_at_time_after_deletion() {
        let db = GallifreyDB::new().unwrap();

        // Create nodes and edges
        let alice = db
            .create_node("Person", PropertyMapBuilder::new().build())
            .unwrap();
        let bob = db
            .create_node("Person", PropertyMapBuilder::new().build())
            .unwrap();

        let edge1 = db
            .create_edge(alice, bob, "KNOWS", PropertyMapBuilder::new().build())
            .unwrap();
        let edge2 = db
            .create_edge(bob, alice, "WORKS_WITH", PropertyMapBuilder::new().build())
            .unwrap();

        let t_after_create = *db.current_timestamp.lock().unwrap();

        // Delete edge1
        db.write(|tx| {
            tx.delete_edge(edge1)?;
            Ok(())
        })
        .unwrap();

        let t_after_delete = *db.current_timestamp.lock().unwrap();

        // Query at time after deletion - edge1 should not be found
        let edge_ids = vec![edge1, edge2];
        let results = db
            .get_edges_at_time(&edge_ids, t_after_delete, t_after_delete)
            .unwrap();

        assert_eq!(results.len(), 2);
        assert!(results[0].1.is_none()); // edge1 was deleted
        assert!(results[1].1.is_some()); // edge2 still exists

        // Query at time before deletion - both should exist
        let results = db
            .get_edges_at_time(&edge_ids, t_after_create, t_after_create)
            .unwrap();

        assert_eq!(results.len(), 2);
        assert!(results[0].1.is_some()); // edge1 existed
        assert!(results[1].1.is_some()); // edge2 existed
    }

    #[test]
    fn test_get_nodes_at_time_large_batch() {
        let db = GallifreyDB::new().unwrap();

        // Create 100 nodes
        let node_ids: Vec<_> = (0..100)
            .map(|i| {
                db.create_node(
                    "Test",
                    PropertyMapBuilder::new().insert("index", i as i64).build(),
                )
                .unwrap()
            })
            .collect();

        let t1 = *db.current_timestamp.lock().unwrap();

        // Query all 100 at once
        let results = db.get_nodes_at_time(&node_ids, t1, t1).unwrap();

        // All should exist
        assert_eq!(results.len(), 100);
        assert!(results.iter().all(|(_, node)| node.is_some()));

        // Verify order is preserved
        for (i, (id, _)) in results.iter().enumerate() {
            assert_eq!(*id, node_ids[i]);
        }
    }

    #[test]
    fn test_get_nodes_at_time_duplicate_ids() {
        let db = GallifreyDB::new().unwrap();

        let node1 = db
            .create_node(
                "Test",
                PropertyMapBuilder::new().insert("name", "Alice").build(),
            )
            .unwrap();

        let t1 = *db.current_timestamp.lock().unwrap();

        // Query with duplicates
        let node_ids = vec![node1, node1, node1];
        let results = db.get_nodes_at_time(&node_ids, t1, t1).unwrap();

        // Should return 3 results (one per input, even if duplicate)
        assert_eq!(results.len(), 3);
        assert!(
            results
                .iter()
                .all(|(id, node)| { *id == node1 && node.is_some() })
        );
    }

    #[test]
    fn test_get_edges_at_time_large_batch() {
        let db = GallifreyDB::new().unwrap();

        // Create nodes
        let source = db
            .create_node("Node", PropertyMapBuilder::new().build())
            .unwrap();
        let target = db
            .create_node("Node", PropertyMapBuilder::new().build())
            .unwrap();

        // Create 100 edges
        let edge_ids: Vec<_> = (0..100)
            .map(|i| {
                db.create_edge(
                    source,
                    target,
                    "LINK",
                    PropertyMapBuilder::new().insert("index", i as i64).build(),
                )
                .unwrap()
            })
            .collect();

        let t1 = *db.current_timestamp.lock().unwrap();

        // Query all 100 at once
        let results = db.get_edges_at_time(&edge_ids, t1, t1).unwrap();

        // All should exist
        assert_eq!(results.len(), 100);
        assert!(results.iter().all(|(_, edge)| edge.is_some()));

        // Verify order is preserved
        for (i, (id, _)) in results.iter().enumerate() {
            assert_eq!(*id, edge_ids[i]);
        }
    }

    #[test]
    fn test_get_edges_at_time_duplicate_ids() {
        let db = GallifreyDB::new().unwrap();

        let source = db
            .create_node("Node", PropertyMapBuilder::new().build())
            .unwrap();
        let target = db
            .create_node("Node", PropertyMapBuilder::new().build())
            .unwrap();

        let edge1 = db
            .create_edge(source, target, "LINK", PropertyMapBuilder::new().build())
            .unwrap();

        let t1 = *db.current_timestamp.lock().unwrap();

        // Query with duplicates
        let edge_ids = vec![edge1, edge1, edge1];
        let results = db.get_edges_at_time(&edge_ids, t1, t1).unwrap();

        // Should return 3 results (one per input, even if duplicate)
        assert_eq!(results.len(), 3);
        assert!(
            results
                .iter()
                .all(|(id, edge)| { *id == edge1 && edge.is_some() })
        );
    }

    #[test]
    fn test_find_similar_with_missing_property() {
        use crate::index::vector::{DistanceMetric, HnswConfig};

        let db = GallifreyDB::new().unwrap();

        // Enable vector index on "embedding" property
        let config = HnswConfig::new(3, DistanceMetric::Cosine).with_capacity(100);
        db.enable_vector_index("embedding", config).unwrap();

        // Create some nodes with the indexed property
        let doc1 = db
            .create_node(
                "Document",
                PropertyMapBuilder::new()
                    .insert_vector("embedding", &[1.0f32, 0.0, 0.0])
                    .build(),
            )
            .unwrap();

        let _doc2 = db
            .create_node(
                "Document",
                PropertyMapBuilder::new()
                    .insert_vector("embedding", &[0.9f32, 0.1, 0.0])
                    .build(),
            )
            .unwrap();

        // Create a node WITHOUT the indexed property (should be ignored in searches)
        let _doc_no_vector = db
            .create_node(
                "Document",
                PropertyMapBuilder::new()
                    .insert("title", "No embedding")
                    .build(),
            )
            .unwrap();

        // Search should only find nodes with the property
        let results = db.find_similar(doc1, 5).unwrap();

        // Should find doc2 but not doc_no_vector
        assert_eq!(results.len(), 1); // Only doc2 (doc1 is excluded as query node)
    }

    // ========================================================================
    // Tests for Issue #389: VectorIndexBuilder pattern
    // ========================================================================

    /// Test basic builder pattern for enabling vector index.
    #[test]
    fn test_vector_index_builder_basic() {
        use crate::index::vector::DistanceMetric;

        let db = GallifreyDB::new().unwrap();

        // Builder pattern API
        db.vector_index("embedding")
            .hnsw(HnswConfig::new(4, DistanceMetric::Cosine).with_capacity(100))
            .enable()
            .expect("Should enable vector index via builder");

        // Verify it was enabled
        assert!(db.has_vector_index("embedding"));
    }

    /// Test builder pattern with multiple properties.
    #[test]
    fn test_vector_index_builder_multiple_properties() {
        use crate::index::vector::DistanceMetric;

        let db = GallifreyDB::new().unwrap();

        // Enable two indexes via builder
        db.vector_index("title_embedding")
            .hnsw(HnswConfig::new(4, DistanceMetric::Cosine).with_capacity(100))
            .enable()
            .unwrap();

        db.vector_index("body_embedding")
            .hnsw(HnswConfig::new(8, DistanceMetric::Euclidean).with_capacity(100))
            .enable()
            .unwrap();

        // Both should be enabled
        assert!(db.has_vector_index("title_embedding"));
        assert!(db.has_vector_index("body_embedding"));

        // Verify different configs
        let indexes = db.list_vector_indexes();
        assert_eq!(indexes.len(), 2);
    }

    /// Test builder pattern without calling hnsw() fails.
    #[test]
    fn test_vector_index_builder_missing_hnsw_fails() {
        let db = GallifreyDB::new().unwrap();

        // Calling enable() without hnsw() should fail
        let result = db.vector_index("embedding").enable();
        assert!(result.is_err(), "Should fail without HNSW config");
    }

    /// Test builder with temporal config auto-enables current index.
    ///
    /// This is the key DX improvement from #386: users shouldn't need to call
    /// both enable_vector_index() and enable_temporal_vector_index().
    #[test]
    fn test_vector_index_builder_with_temporal() {
        use crate::index::vector::DistanceMetric;
        use crate::index::vector::temporal::{
            RetentionPolicy, SnapshotStrategy, TemporalVectorConfig,
        };

        let db = GallifreyDB::new().unwrap();
        let hnsw_config = HnswConfig::new(4, DistanceMetric::Cosine).with_capacity(100);

        // Single call enables both current and temporal indexing
        db.vector_index("embedding")
            .hnsw(hnsw_config.clone())
            .temporal(TemporalVectorConfig {
                snapshot_strategy: SnapshotStrategy::TransactionInterval(10),
                retention_policy: RetentionPolicy::KeepN(100),
                max_snapshots: 100,
                full_snapshot_interval: 10,
                hnsw_config: Some(hnsw_config), // Will be overwritten by builder, but needed for struct completeness
            })
            .enable()
            .expect("Should enable both current and temporal index");

        // Current index should be auto-enabled
        assert!(db.has_vector_index("embedding"));

        // Temporal index should also be enabled
        assert!(db.is_temporal_vector_index_enabled());
    }

    /// Test builder creates working index that can be searched.
    #[test]
    fn test_vector_index_builder_functional() {
        use crate::index::vector::DistanceMetric;

        let db = GallifreyDB::new().unwrap();

        db.vector_index("embedding")
            .hnsw(HnswConfig::new(4, DistanceMetric::Cosine).with_capacity(100))
            .enable()
            .unwrap();

        // Create nodes with embeddings
        let v1 = vec![1.0f32, 0.0, 0.0, 0.0];
        let v2 = vec![0.9f32, 0.1, 0.0, 0.0];

        let node1 = db
            .create_node(
                "Doc",
                PropertyMapBuilder::new()
                    .insert_vector("embedding", &v1)
                    .build(),
            )
            .unwrap();
        let _node2 = db
            .create_node(
                "Doc",
                PropertyMapBuilder::new()
                    .insert_vector("embedding", &v2)
                    .build(),
            )
            .unwrap();

        // Search should work
        let results = db.find_similar(node1, 1).unwrap();
        assert_eq!(results.len(), 1);
    }

    /// Test re-enabling same property via builder fails.
    #[test]
    fn test_vector_index_builder_same_property_twice_fails() {
        use crate::index::vector::DistanceMetric;

        let db = GallifreyDB::new().unwrap();

        db.vector_index("embedding")
            .hnsw(HnswConfig::new(4, DistanceMetric::Cosine).with_capacity(100))
            .enable()
            .unwrap();

        // Second enable on same property should fail
        let result = db
            .vector_index("embedding")
            .hnsw(HnswConfig::new(4, DistanceMetric::Cosine).with_capacity(100))
            .enable();

        assert!(
            result.is_err(),
            "Should not allow re-enabling same property"
        );
    }

    // ========================================================================
    // Tests for Issue #389: Query API with explicit property specification
    // ========================================================================

    /// Test find_similar_in() with explicit property specification.
    #[test]
    fn test_find_similar_in_explicit_property() {
        use crate::index::vector::DistanceMetric;

        let db = GallifreyDB::new().unwrap();

        // Enable two different indexes
        db.vector_index("title_embedding")
            .hnsw(HnswConfig::new(4, DistanceMetric::Cosine).with_capacity(100))
            .enable()
            .unwrap();

        db.vector_index("body_embedding")
            .hnsw(HnswConfig::new(4, DistanceMetric::Cosine).with_capacity(100))
            .enable()
            .unwrap();

        // Create nodes with different embeddings for each property
        let title_v1 = vec![1.0f32, 0.0, 0.0, 0.0];
        let title_v2 = vec![0.9f32, 0.1, 0.0, 0.0];
        let body_v1 = vec![0.0f32, 1.0, 0.0, 0.0];
        let body_v2 = vec![0.0f32, 0.9, 0.1, 0.0];

        let node1 = db
            .create_node(
                "Doc",
                PropertyMapBuilder::new()
                    .insert_vector("title_embedding", &title_v1)
                    .insert_vector("body_embedding", &body_v1)
                    .build(),
            )
            .unwrap();

        let _node2 = db
            .create_node(
                "Doc",
                PropertyMapBuilder::new()
                    .insert_vector("title_embedding", &title_v2)
                    .insert_vector("body_embedding", &body_v2)
                    .build(),
            )
            .unwrap();

        // Search by title embedding
        let title_results = db.find_similar_in("title_embedding", node1, 1).unwrap();
        assert_eq!(title_results.len(), 1);

        // Search by body embedding
        let body_results = db.find_similar_in("body_embedding", node1, 1).unwrap();
        assert_eq!(body_results.len(), 1);
    }

    /// Test search_vectors_in() with explicit property specification.
    #[test]
    fn test_search_vectors_in_explicit_property() {
        use crate::index::vector::DistanceMetric;

        let db = GallifreyDB::new().unwrap();

        db.vector_index("embedding")
            .hnsw(HnswConfig::new(4, DistanceMetric::Cosine).with_capacity(100))
            .enable()
            .unwrap();

        // Create nodes
        let v1 = vec![1.0f32, 0.0, 0.0, 0.0];
        let v2 = vec![0.9f32, 0.1, 0.0, 0.0];

        db.create_node(
            "Doc",
            PropertyMapBuilder::new()
                .insert_vector("embedding", &v1)
                .build(),
        )
        .unwrap();

        db.create_node(
            "Doc",
            PropertyMapBuilder::new()
                .insert_vector("embedding", &v2)
                .build(),
        )
        .unwrap();

        // Search with raw embedding (not tied to a node)
        let query = vec![1.0f32, 0.0, 0.0, 0.0];
        let results = db.search_vectors_in("embedding", &query, 2).unwrap();
        assert_eq!(results.len(), 2);
    }

    /// Test find_similar_in() with non-existent property fails.
    #[test]
    fn test_find_similar_in_nonexistent_property_fails() {
        use crate::index::vector::DistanceMetric;

        let db = GallifreyDB::new().unwrap();

        db.vector_index("embedding")
            .hnsw(HnswConfig::new(4, DistanceMetric::Cosine).with_capacity(100))
            .enable()
            .unwrap();

        let v1 = vec![1.0f32, 0.0, 0.0, 0.0];
        let node1 = db
            .create_node(
                "Doc",
                PropertyMapBuilder::new()
                    .insert_vector("embedding", &v1)
                    .build(),
            )
            .unwrap();

        // Search with wrong property name should fail
        let result = db.find_similar_in("nonexistent", node1, 1);
        assert!(result.is_err());
    }

    // ========================================================================
    // Tests for Issue #389: Temporal property-specific queries
    // ========================================================================

    /// Test find_similar_as_of_in() with explicit property specification.
    #[test]
    fn test_find_similar_as_of_in_explicit_property() {
        use crate::index::vector::DistanceMetric;
        use crate::index::vector::temporal::{
            RetentionPolicy, SnapshotStrategy, TemporalVectorConfig,
        };

        let db = GallifreyDB::new().unwrap();
        let hnsw_config = HnswConfig::new(4, DistanceMetric::Cosine).with_capacity(100);

        // Enable temporal vector index for a specific property
        db.vector_index("content_embedding")
            .hnsw(hnsw_config.clone())
            .temporal(TemporalVectorConfig {
                snapshot_strategy: SnapshotStrategy::TransactionInterval(1), // Snapshot every tx
                retention_policy: RetentionPolicy::KeepN(100),
                max_snapshots: 100,
                full_snapshot_interval: 10,
                hnsw_config: Some(hnsw_config),
            })
            .enable()
            .expect("Should enable temporal index");

        // Create a node with an embedding
        let v1 = vec![1.0f32, 0.0, 0.0, 0.0];
        let _node1 = db
            .create_node(
                "Doc",
                PropertyMapBuilder::new()
                    .insert_vector("content_embedding", &v1)
                    .build(),
            )
            .unwrap();

        // Get a timestamp after the node was created
        let timestamp = crate::core::temporal::time::now();

        // Query with property-specific temporal search
        let query = vec![0.9f32, 0.1, 0.0, 0.0];
        let results = db
            .find_similar_as_of_in("content_embedding", &query, 10, timestamp)
            .unwrap();

        // Should find the node
        assert!(!results.is_empty());
    }

    /// Test find_similar_as_of_in() with wrong property fails.
    #[test]
    fn test_find_similar_as_of_in_wrong_property_fails() {
        use crate::index::vector::DistanceMetric;
        use crate::index::vector::temporal::{
            RetentionPolicy, SnapshotStrategy, TemporalVectorConfig,
        };

        let db = GallifreyDB::new().unwrap();
        let hnsw_config = HnswConfig::new(4, DistanceMetric::Cosine).with_capacity(100);

        // Enable temporal index for "embedding" property
        db.vector_index("embedding")
            .hnsw(hnsw_config.clone())
            .temporal(TemporalVectorConfig {
                snapshot_strategy: SnapshotStrategy::TransactionInterval(1),
                retention_policy: RetentionPolicy::KeepN(100),
                max_snapshots: 100,
                full_snapshot_interval: 10,
                hnsw_config: Some(hnsw_config),
            })
            .enable()
            .expect("Should enable temporal index");

        let timestamp = crate::core::temporal::time::now();
        let query = vec![1.0f32, 0.0, 0.0, 0.0];

        // Query with WRONG property name should fail
        let result = db.find_similar_as_of_in("wrong_property", &query, 10, timestamp);
        assert!(
            result.is_err(),
            "Should fail when property doesn't match temporal index"
        );
    }

    /// Test find_similar_as_of_in() when temporal index not enabled.
    #[test]
    fn test_find_similar_as_of_in_no_temporal_index() {
        use crate::index::vector::DistanceMetric;

        let db = GallifreyDB::new().unwrap();

        // Only enable regular HNSW index, not temporal
        db.vector_index("embedding")
            .hnsw(HnswConfig::new(4, DistanceMetric::Cosine).with_capacity(100))
            .enable()
            .unwrap();

        let timestamp = crate::core::temporal::time::now();
        let query = vec![1.0f32, 0.0, 0.0, 0.0];

        // Temporal query should fail when temporal index not enabled
        let result = db.find_similar_as_of_in("embedding", &query, 10, timestamp);
        assert!(
            result.is_err(),
            "Should fail when temporal index not enabled"
        );
    }

    /// Test track_drift_in() with explicit property name.
    #[test]
    fn test_track_drift_in_explicit_property() {
        use crate::core::temporal::TimeRange;
        use crate::index::vector::DistanceMetric;
        use crate::index::vector::temporal::{
            RetentionPolicy, SnapshotStrategy, TemporalVectorConfig,
        };

        let db = GallifreyDB::new().unwrap();
        let hnsw_config = HnswConfig::new(4, DistanceMetric::Cosine).with_capacity(100);

        // Enable temporal vector index for a specific property
        db.vector_index("content_embedding")
            .hnsw(hnsw_config.clone())
            .temporal(TemporalVectorConfig {
                snapshot_strategy: SnapshotStrategy::TransactionInterval(1), // Snapshot every tx
                retention_policy: RetentionPolicy::KeepN(100),
                max_snapshots: 100,
                full_snapshot_interval: 10,
                hnsw_config: Some(hnsw_config),
            })
            .enable()
            .expect("Should enable temporal index");

        // Create a node with an embedding
        let v1 = vec![1.0f32, 0.0, 0.0, 0.0];
        let node_id = db
            .create_node(
                "Doc",
                PropertyMapBuilder::new()
                    .insert_vector("content_embedding", &v1)
                    .build(),
            )
            .unwrap();

        // Track drift over time using property-specific method
        // Even with just one snapshot, the API should work (may return empty or single result)
        let reference = vec![1.0f32, 0.0, 0.0, 0.0];
        let time_range = TimeRange::new(0.into(), i64::MAX.into()).unwrap();
        let result = db.track_drift_in("content_embedding", node_id, &reference, time_range);

        // Should succeed (not error) - method exists and property validation passes
        assert!(
            result.is_ok(),
            "track_drift_in should succeed with correct property"
        );
    }

    /// Test track_drift_in() with wrong property fails.
    #[test]
    fn test_track_drift_in_wrong_property_fails() {
        use crate::core::temporal::TimeRange;
        use crate::index::vector::DistanceMetric;
        use crate::index::vector::temporal::{
            RetentionPolicy, SnapshotStrategy, TemporalVectorConfig,
        };

        let db = GallifreyDB::new().unwrap();
        let hnsw_config = HnswConfig::new(4, DistanceMetric::Cosine).with_capacity(100);

        // Enable temporal index for "embedding" property
        db.vector_index("embedding")
            .hnsw(hnsw_config.clone())
            .temporal(TemporalVectorConfig {
                snapshot_strategy: SnapshotStrategy::TransactionInterval(1),
                retention_policy: RetentionPolicy::KeepN(100),
                max_snapshots: 100,
                full_snapshot_interval: 10,
                hnsw_config: Some(hnsw_config),
            })
            .enable()
            .expect("Should enable temporal index");

        let node_id = NodeId::new(1).unwrap();
        let reference = vec![1.0f32, 0.0, 0.0, 0.0];
        let time_range = TimeRange::new(0.into(), i64::MAX.into()).unwrap();

        // Query with WRONG property name should fail
        let result = db.track_drift_in("wrong_property", node_id, &reference, time_range);
        assert!(
            result.is_err(),
            "Should fail when property doesn't match temporal index"
        );
    }

    /// Test track_drift_in() when temporal index not enabled.
    #[test]
    fn test_track_drift_in_no_temporal_index() {
        use crate::core::temporal::TimeRange;
        use crate::index::vector::DistanceMetric;

        let db = GallifreyDB::new().unwrap();

        // Only enable regular HNSW index, not temporal
        db.vector_index("embedding")
            .hnsw(HnswConfig::new(4, DistanceMetric::Cosine).with_capacity(100))
            .enable()
            .unwrap();

        let node_id = NodeId::new(1).unwrap();
        let reference = vec![1.0f32, 0.0, 0.0, 0.0];
        let time_range = TimeRange::new(0.into(), i64::MAX.into()).unwrap();

        // Temporal query should fail when temporal index not enabled
        let result = db.track_drift_in("embedding", node_id, &reference, time_range);
        assert!(
            result.is_err(),
            "Should fail when temporal index not enabled"
        );
    }

    /// Test semantic_evolution_in() with explicit property name.
    #[test]
    fn test_semantic_evolution_in_explicit_property() {
        use crate::core::temporal::TimeRange;
        use crate::index::vector::DistanceMetric;
        use crate::index::vector::temporal::{
            RetentionPolicy, SnapshotStrategy, TemporalVectorConfig,
        };

        let db = GallifreyDB::new().unwrap();
        let hnsw_config = HnswConfig::new(4, DistanceMetric::Cosine).with_capacity(100);

        db.vector_index("content_embedding")
            .hnsw(hnsw_config.clone())
            .temporal(TemporalVectorConfig {
                snapshot_strategy: SnapshotStrategy::TransactionInterval(1),
                retention_policy: RetentionPolicy::KeepN(100),
                max_snapshots: 100,
                full_snapshot_interval: 10,
                hnsw_config: Some(hnsw_config),
            })
            .enable()
            .expect("Should enable temporal index");

        let v1 = vec![1.0f32, 0.0, 0.0, 0.0];
        let node_id = db
            .create_node(
                "Doc",
                PropertyMapBuilder::new()
                    .insert_vector("content_embedding", &v1)
                    .build(),
            )
            .unwrap();

        let time_range = TimeRange::new(0.into(), i64::MAX.into()).unwrap();
        let result = db.semantic_evolution_in("content_embedding", node_id, time_range);

        assert!(result.is_ok(), "semantic_evolution_in should succeed");
    }

    /// Test semantic_evolution_in() with wrong property fails.
    #[test]
    fn test_semantic_evolution_in_wrong_property_fails() {
        use crate::core::temporal::TimeRange;
        use crate::index::vector::DistanceMetric;
        use crate::index::vector::temporal::{
            RetentionPolicy, SnapshotStrategy, TemporalVectorConfig,
        };

        let db = GallifreyDB::new().unwrap();
        let hnsw_config = HnswConfig::new(4, DistanceMetric::Cosine).with_capacity(100);

        db.vector_index("embedding")
            .hnsw(hnsw_config.clone())
            .temporal(TemporalVectorConfig {
                snapshot_strategy: SnapshotStrategy::TransactionInterval(1),
                retention_policy: RetentionPolicy::KeepN(100),
                max_snapshots: 100,
                full_snapshot_interval: 10,
                hnsw_config: Some(hnsw_config),
            })
            .enable()
            .expect("Should enable temporal index");

        let node_id = NodeId::new(1).unwrap();
        let time_range = TimeRange::new(0.into(), i64::MAX.into()).unwrap();

        let result = db.semantic_evolution_in("wrong_property", node_id, time_range);
        assert!(
            result.is_err(),
            "Should fail when property doesn't match temporal index"
        );
    }

    /// Test find_drift_in() with explicit property name.
    #[test]
    fn test_find_drift_in_explicit_property() {
        use crate::core::temporal::TimeRange;
        use crate::index::vector::DistanceMetric;
        use crate::index::vector::temporal::{
            DriftMetric, RetentionPolicy, SnapshotStrategy, TemporalVectorConfig,
        };

        let db = GallifreyDB::new().unwrap();
        let hnsw_config = HnswConfig::new(4, DistanceMetric::Cosine).with_capacity(100);

        db.vector_index("content_embedding")
            .hnsw(hnsw_config.clone())
            .temporal(TemporalVectorConfig {
                snapshot_strategy: SnapshotStrategy::TransactionInterval(1),
                retention_policy: RetentionPolicy::KeepN(100),
                max_snapshots: 100,
                full_snapshot_interval: 10,
                hnsw_config: Some(hnsw_config),
            })
            .enable()
            .expect("Should enable temporal index");

        let v1 = vec![1.0f32, 0.0, 0.0, 0.0];
        let _node_id = db
            .create_node(
                "Doc",
                PropertyMapBuilder::new()
                    .insert_vector("content_embedding", &v1)
                    .build(),
            )
            .unwrap();

        let time_range = TimeRange::new(0.into(), i64::MAX.into()).unwrap();
        let result = db.find_drift_in("content_embedding", 0.1, time_range, DriftMetric::Cosine);

        assert!(result.is_ok(), "find_drift_in should succeed");
    }

    /// Test find_drift_in() with wrong property fails.
    #[test]
    fn test_find_drift_in_wrong_property_fails() {
        use crate::core::temporal::TimeRange;
        use crate::index::vector::DistanceMetric;
        use crate::index::vector::temporal::{
            DriftMetric, RetentionPolicy, SnapshotStrategy, TemporalVectorConfig,
        };

        let db = GallifreyDB::new().unwrap();
        let hnsw_config = HnswConfig::new(4, DistanceMetric::Cosine).with_capacity(100);

        db.vector_index("embedding")
            .hnsw(hnsw_config.clone())
            .temporal(TemporalVectorConfig {
                snapshot_strategy: SnapshotStrategy::TransactionInterval(1),
                retention_policy: RetentionPolicy::KeepN(100),
                max_snapshots: 100,
                full_snapshot_interval: 10,
                hnsw_config: Some(hnsw_config),
            })
            .enable()
            .expect("Should enable temporal index");

        let time_range = TimeRange::new(0.into(), i64::MAX.into()).unwrap();

        let result = db.find_drift_in("wrong_property", 0.1, time_range, DriftMetric::Cosine);
        assert!(
            result.is_err(),
            "Should fail when property doesn't match temporal index"
        );
    }

    // ========================================================================
    // Multi-Property Temporal Index Tests (Issue #389 - Critical Fix)
    // ========================================================================

    /// Test that multiple temporal vector indexes can be enabled for different properties.
    /// This is the critical test for multi-property temporal support.
    #[test]
    fn test_multi_property_temporal_indexes() {
        use crate::core::temporal::TimeRange;
        use crate::index::vector::DistanceMetric;
        use crate::index::vector::temporal::{
            RetentionPolicy, SnapshotStrategy, TemporalVectorConfig,
        };

        let db = GallifreyDB::new().unwrap();

        // Enable temporal index for FIRST property
        let hnsw_config1 = HnswConfig::new(4, DistanceMetric::Cosine).with_capacity(100);
        db.vector_index("title_embedding")
            .hnsw(hnsw_config1.clone())
            .temporal(TemporalVectorConfig {
                snapshot_strategy: SnapshotStrategy::TransactionInterval(1),
                retention_policy: RetentionPolicy::KeepN(100),
                max_snapshots: 100,
                full_snapshot_interval: 10,
                hnsw_config: Some(hnsw_config1),
            })
            .enable()
            .expect("Should enable first temporal index");

        // Enable temporal index for SECOND property - should NOT overwrite first!
        let hnsw_config2 = HnswConfig::new(4, DistanceMetric::Cosine).with_capacity(100);
        db.vector_index("content_embedding")
            .hnsw(hnsw_config2.clone())
            .temporal(TemporalVectorConfig {
                snapshot_strategy: SnapshotStrategy::TransactionInterval(1),
                retention_policy: RetentionPolicy::KeepN(100),
                max_snapshots: 100,
                full_snapshot_interval: 10,
                hnsw_config: Some(hnsw_config2),
            })
            .enable()
            .expect("Should enable second temporal index");

        // Create a node with both embeddings
        let title_emb = vec![1.0f32, 0.0, 0.0, 0.0];
        let content_emb = vec![0.0f32, 1.0, 0.0, 0.0];
        let node_id = db
            .create_node(
                "Doc",
                PropertyMapBuilder::new()
                    .insert_vector("title_embedding", &title_emb)
                    .insert_vector("content_embedding", &content_emb)
                    .build(),
            )
            .unwrap();

        // Both temporal indexes should work independently
        let time_range = TimeRange::new(0.into(), i64::MAX.into()).unwrap();
        let query = vec![0.9f32, 0.1, 0.0, 0.0];

        // Query first property's temporal index
        let result1 = db.find_similar_as_of_in(
            "title_embedding",
            &query,
            10,
            crate::core::temporal::time::now(),
        );
        assert!(
            result1.is_ok(),
            "First temporal index should still work: {:?}",
            result1.err()
        );

        // Query second property's temporal index
        let result2 = db.find_similar_as_of_in(
            "content_embedding",
            &query,
            10,
            crate::core::temporal::time::now(),
        );
        assert!(
            result2.is_ok(),
            "Second temporal index should work: {:?}",
            result2.err()
        );

        // Both track_drift_in should work
        let reference = vec![1.0f32, 0.0, 0.0, 0.0];
        let drift1 = db.track_drift_in("title_embedding", node_id, &reference, time_range);
        assert!(
            drift1.is_ok(),
            "track_drift_in for first property should work"
        );

        let drift2 = db.track_drift_in("content_embedding", node_id, &reference, time_range);
        assert!(
            drift2.is_ok(),
            "track_drift_in for second property should work"
        );
    }

    /// Test that temporal queries on non-existent property fail gracefully.
    #[test]
    fn test_temporal_query_nonexistent_property() {
        use crate::index::vector::DistanceMetric;
        use crate::index::vector::temporal::{
            RetentionPolicy, SnapshotStrategy, TemporalVectorConfig,
        };

        let db = GallifreyDB::new().unwrap();

        // Enable temporal index for one property
        let hnsw_config = HnswConfig::new(4, DistanceMetric::Cosine).with_capacity(100);
        db.vector_index("embedding")
            .hnsw(hnsw_config.clone())
            .temporal(TemporalVectorConfig {
                snapshot_strategy: SnapshotStrategy::TransactionInterval(1),
                retention_policy: RetentionPolicy::KeepN(100),
                max_snapshots: 100,
                full_snapshot_interval: 10,
                hnsw_config: Some(hnsw_config),
            })
            .enable()
            .expect("Should enable temporal index");

        // Query a property that has NO temporal index enabled
        let query = vec![1.0f32, 0.0, 0.0, 0.0];
        let result = db.find_similar_as_of_in(
            "nonexistent_property",
            &query,
            10,
            crate::core::temporal::time::now(),
        );

        assert!(
            result.is_err(),
            "Should fail for property without temporal index"
        );
    }

    // ==================== Code Review Fix Tests ====================

    #[test]
    fn test_temporal_config_without_hnsw_config() {
        // Test that TemporalVectorConfig can be created without hnsw_config
        // when a vector index already exists
        use crate::index::vector::temporal::TemporalVectorConfig;
        use crate::index::vector::{DistanceMetric, HnswConfig};

        let db = GallifreyDB::new().unwrap();

        // Enable vector index first
        let hnsw_config = HnswConfig::new(4, DistanceMetric::Cosine).with_capacity(100);
        db.enable_vector_index("embedding", hnsw_config).unwrap();

        // Now enable temporal vector index WITHOUT providing hnsw_config
        // This should succeed because vector index already exists
        let temporal_config = TemporalVectorConfig::default_temporal_only();
        let result = db.enable_temporal_vector_index("embedding", temporal_config);
        assert!(
            result.is_ok(),
            "Should succeed when vector index exists and hnsw_config is None"
        );
    }

    #[test]
    fn test_temporal_config_without_hnsw_config_requires_existing_index() {
        // Test that TemporalVectorConfig without hnsw_config fails
        // when no vector index exists
        use crate::index::vector::temporal::TemporalVectorConfig;

        let db = GallifreyDB::new().unwrap();

        // Try to enable temporal vector index WITHOUT providing hnsw_config
        // AND without an existing vector index - this should fail
        let temporal_config = TemporalVectorConfig::default_temporal_only();
        let result = db.enable_temporal_vector_index("embedding", temporal_config);
        assert!(
            result.is_err(),
            "Should fail when no vector index exists and hnsw_config is None"
        );
    }

    #[test]
    fn test_temporal_config_default_temporal_only() {
        // Test the default_temporal_only constructor
        use crate::index::vector::temporal::{
            RetentionPolicy, SnapshotStrategy, TemporalVectorConfig,
        };

        let config = TemporalVectorConfig::default_temporal_only();

        assert!(config.hnsw_config.is_none(), "hnsw_config should be None");
        assert_eq!(
            config.snapshot_strategy,
            SnapshotStrategy::TransactionInterval(10)
        );
        assert_eq!(config.retention_policy, RetentionPolicy::KeepN(100));
        assert_eq!(config.max_snapshots, 100);
        assert_eq!(config.full_snapshot_interval, 10);
    }

    #[test]
    fn test_list_temporal_vector_indexes() {
        // Test the list_temporal_vector_indexes method
        use crate::index::vector::temporal::{
            RetentionPolicy, SnapshotStrategy, TemporalVectorConfig,
        };
        use crate::index::vector::{DistanceMetric, HnswConfig};

        let db = GallifreyDB::new().unwrap();

        // Initially empty
        assert!(db.list_temporal_vector_indexes().is_empty());

        // Enable temporal index for first property
        let hnsw_config1 = HnswConfig::new(4, DistanceMetric::Cosine).with_capacity(100);
        db.vector_index("embedding1")
            .hnsw(hnsw_config1)
            .temporal(TemporalVectorConfig {
                snapshot_strategy: SnapshotStrategy::TransactionInterval(10),
                retention_policy: RetentionPolicy::KeepN(100),
                max_snapshots: 100,
                full_snapshot_interval: 10,
                hnsw_config: None, // Will be resolved from builder
            })
            .enable()
            .expect("Should enable first temporal index");

        // Should have one index
        let indexes = db.list_temporal_vector_indexes();
        assert_eq!(indexes.len(), 1);
        assert!(indexes.contains(&"embedding1".to_string()));

        // Enable temporal index for second property
        let hnsw_config2 = HnswConfig::new(4, DistanceMetric::Cosine).with_capacity(100);
        db.vector_index("embedding2")
            .hnsw(hnsw_config2)
            .temporal(TemporalVectorConfig {
                snapshot_strategy: SnapshotStrategy::TransactionInterval(10),
                retention_policy: RetentionPolicy::KeepN(100),
                max_snapshots: 100,
                full_snapshot_interval: 10,
                hnsw_config: None,
            })
            .enable()
            .expect("Should enable second temporal index");

        // Should have two indexes
        let indexes = db.list_temporal_vector_indexes();
        assert_eq!(indexes.len(), 2);
        assert!(indexes.contains(&"embedding1".to_string()));
        assert!(indexes.contains(&"embedding2".to_string()));
    }

    #[test]
    fn test_concurrent_vector_operations_with_multiple_properties() {
        // Test concurrent operations across multiple vector properties
        use crate::index::vector::{DistanceMetric, HnswConfig};
        use std::sync::Arc;
        use std::thread;

        let db = Arc::new(GallifreyDB::new().unwrap());

        // Enable two vector indexes for different properties
        let hnsw_config1 = HnswConfig::new(4, DistanceMetric::Cosine).with_capacity(100);
        let hnsw_config2 = HnswConfig::new(4, DistanceMetric::Cosine).with_capacity(100);

        db.enable_vector_index("title_embedding", hnsw_config1)
            .unwrap();
        db.enable_vector_index("content_embedding", hnsw_config2)
            .unwrap();

        // Spawn multiple threads that create nodes with different embeddings concurrently
        let mut handles = vec![];
        for i in 0..10 {
            let db_clone = Arc::clone(&db);
            let handle = thread::spawn(move || {
                let base = (i as f32 + 1.0) / 10.0;
                let title_emb = vec![base, 0.0, 0.0, 0.0];
                let content_emb = vec![0.0, base, 0.0, 0.0];

                db_clone
                    .create_node(
                        "Document",
                        PropertyMapBuilder::new()
                            .insert("id", i as i64)
                            .insert_vector("title_embedding", &title_emb)
                            .insert_vector("content_embedding", &content_emb)
                            .build(),
                    )
                    .unwrap()
            });
            handles.push(handle);
        }

        // Wait for all threads to complete
        let node_ids: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();

        // Verify all nodes were created
        assert_eq!(db.node_count(), 10);

        // Verify both indexes are functioning
        // Search by title_embedding
        let title_query = vec![0.5f32, 0.0, 0.0, 0.0];
        let title_results = db
            .search_vectors_in("title_embedding", &title_query, 5)
            .unwrap();
        assert!(
            !title_results.is_empty(),
            "Should find results in title_embedding index"
        );

        // Search by content_embedding
        let content_query = vec![0.0f32, 0.5, 0.0, 0.0];
        let content_results = db
            .search_vectors_in("content_embedding", &content_query, 5)
            .unwrap();
        assert!(
            !content_results.is_empty(),
            "Should find results in content_embedding index"
        );

        // Verify both indexes have correct data (all nodes indexed in both)
        // Each node should be findable via both indexes
        let similar_by_title = db
            .search_vectors_in("title_embedding", &[0.5, 0.0, 0.0, 0.0], 10)
            .unwrap();
        let similar_by_content = db
            .search_vectors_in("content_embedding", &[0.0, 0.5, 0.0, 0.0], 10)
            .unwrap();

        // Both should have results from all nodes
        assert!(
            !similar_by_title.is_empty(),
            "Title index should have results"
        );
        assert!(
            !similar_by_content.is_empty(),
            "Content index should have results"
        );

        // Results should be distinct - title and content queries target different directions
        // So the top results from each should have different orderings
        let _ = node_ids; // Use node_ids to suppress unused warning
    }

    // ==================== Constructor Error Handling Tests ====================
    // These tests verify that database constructors return Result and properly
    // propagate WAL creation errors (Issue #343)

    #[test]
    fn test_new_returns_result() {
        // GallifreyDB::new() should return Result<Self> and succeed with default config
        let result = GallifreyDB::new();
        assert!(result.is_ok(), "new() should succeed with default config");
    }

    #[test]
    fn test_with_config_returns_result() {
        // GallifreyDB::with_config() should return Result<Self>
        let result = GallifreyDB::with_config(crate::storage::version::AnchorConfig::default());
        assert!(
            result.is_ok(),
            "with_config() should succeed with default config"
        );
    }

    #[test]
    fn test_with_wal_config_returns_result() {
        // GallifreyDB::with_wal_config() should return Result<Self>
        let wal_config = crate::config::WalConfig::default();
        let result = GallifreyDB::with_wal_config(wal_config);
        assert!(
            result.is_ok(),
            "with_wal_config() should succeed with default config"
        );
    }

    #[test]
    fn test_with_full_config_returns_result() {
        // GallifreyDB::with_full_config() should return Result<Self>
        let result = GallifreyDB::with_full_config(
            crate::storage::version::AnchorConfig::default(),
            crate::config::WalConfig::default(),
        );
        assert!(
            result.is_ok(),
            "with_full_config() should succeed with default config"
        );
    }

    #[test]
    fn test_with_unified_config_returns_result() {
        // GallifreyDB::with_unified_config() should return Result<Self>
        let config = crate::config::GallifreyDBConfig::default();
        let result = GallifreyDB::with_unified_config(config);
        assert!(
            result.is_ok(),
            "with_unified_config() should succeed with default config"
        );
    }

    #[cfg(unix)]
    #[test]
    fn test_wal_creation_failure_propagates_error() {
        // When WAL creation fails, the error should be propagated instead of panicking
        use std::path::PathBuf;

        // Use /dev/null/wal - /dev/null is a character device, not a directory,
        // so any attempt to create subdirectories under it will fail
        let invalid_wal_dir = PathBuf::from("/dev/null/wal");

        let wal_config = crate::config::WalConfigBuilder::new()
            .wal_dir(invalid_wal_dir)
            .build();

        let result = GallifreyDB::with_wal_config(wal_config);

        // Should return Err instead of panicking
        assert!(
            result.is_err(),
            "with_wal_config() should return Err when WAL directory cannot be created"
        );

        // Error should mention an I/O issue
        let err = result.err().expect("Expected an error");
        let err_msg = err.to_string().to_lowercase();
        assert!(
            err_msg.contains("i/o")
                || err_msg.contains("directory")
                || err_msg.contains("not a directory"),
            "Error message should indicate I/O issue, got: {}",
            err
        );
    }

    #[cfg(unix)]
    #[test]
    fn test_unified_config_wal_failure_propagates_error() {
        // When WAL creation fails in with_unified_config, the error should be propagated
        use std::path::PathBuf;

        // Use /dev/null/wal - /dev/null is a character device, not a directory,
        // so any attempt to create subdirectories under it will fail
        let invalid_wal_dir = PathBuf::from("/dev/null/wal");

        let config = crate::config::GallifreyDBConfigBuilder::new()
            .wal(
                crate::config::WalConfigBuilder::new()
                    .wal_dir(invalid_wal_dir)
                    .build(),
            )
            .build();

        let result = GallifreyDB::with_unified_config(config);

        // Should return Err instead of panicking
        assert!(
            result.is_err(),
            "with_unified_config() should return Err when WAL directory cannot be created"
        );
    }

    #[test]
    fn test_max_vector_properties_limit() {
        // Test that the maximum number of vector properties is enforced
        use crate::index::vector::{DistanceMetric, HnswConfig};
        use crate::storage::current::DEFAULT_MAX_VECTOR_PROPERTIES;

        let db = crate::GallifreyDB::new().unwrap();

        // Enable indexes up to the limit
        for i in 0..DEFAULT_MAX_VECTOR_PROPERTIES {
            let config = HnswConfig::new(4, DistanceMetric::Cosine);
            let result = db
                .vector_index(&format!("property_{}", i))
                .hnsw(config)
                .enable();
            assert!(
                result.is_ok(),
                "Should be able to enable index {} (limit is {})",
                i,
                DEFAULT_MAX_VECTOR_PROPERTIES
            );
        }

        // Verify we have exactly the limit number of indexes
        let indexes = db.list_vector_indexes();
        assert_eq!(indexes.len(), DEFAULT_MAX_VECTOR_PROPERTIES);

        // Attempting to add one more should fail
        let config = HnswConfig::new(4, DistanceMetric::Cosine);
        let result = db.vector_index("one_too_many").hnsw(config).enable();
        assert!(
            result.is_err(),
            "Should not be able to exceed the maximum vector property limit"
        );

        // Verify the error message is helpful
        let err = result.unwrap_err();
        let err_msg = err.to_string();
        assert!(
            err_msg.contains("Maximum") || err_msg.contains("maximum"),
            "Error message should mention the maximum limit"
        );
    }

    // ========================================================================
    // Phase 3: Simple Accessor and Getter Tests
    // ========================================================================

    #[test]
    fn test_gallifreydb_is_vector_index_enabled_for() {
        use crate::index::vector::{DistanceMetric, HnswConfig};

        let db = GallifreyDB::new().unwrap();

        // Initially no index should be enabled
        assert!(!db.is_vector_index_enabled());
        assert!(!db.is_vector_index_enabled_for("embedding"));
        assert!(!db.is_vector_index_enabled_for("vector"));

        // Enable index for "embedding"
        let config = HnswConfig::new(128, DistanceMetric::Cosine);
        db.enable_vector_index("embedding", config).unwrap();

        // Now should be enabled
        assert!(db.is_vector_index_enabled());
        assert!(db.is_vector_index_enabled_for("embedding"));
        assert!(!db.is_vector_index_enabled_for("vector")); // Still false for other property

        // Enable another index
        let config2 = HnswConfig::new(256, DistanceMetric::Euclidean);
        db.enable_vector_index("vector", config2).unwrap();

        assert!(db.is_vector_index_enabled_for("vector"));
    }

    #[test]
    fn test_gallifreydb_default_durability() {
        let db = GallifreyDB::new().unwrap();

        // Default durability should exist and be valid
        let _durability = db.default_durability();
        // Just verify we can call it without error
    }

    #[test]
    fn test_get_edge_source_and_target() {
        let db = GallifreyDB::new().unwrap();

        // Create nodes
        let alice = db
            .create_node(
                "Person",
                PropertyMapBuilder::new().insert("name", "Alice").build(),
            )
            .unwrap();

        let bob = db
            .create_node(
                "Person",
                PropertyMapBuilder::new().insert("name", "Bob").build(),
            )
            .unwrap();

        // Create edge from alice to bob
        let knows_edge = db
            .create_edge(alice, bob, "KNOWS", PropertyMapBuilder::new().build())
            .unwrap();

        // Verify get_edge_source and get_edge_target
        assert_eq!(db.get_edge_source(knows_edge).unwrap(), alice);
        assert_eq!(db.get_edge_target(knows_edge).unwrap(), bob);
    }
}

#[test]
fn test_load_indexes_coverage() {
    use crate::config::GallifreyDBConfig;
    use crate::core::property::PropertyMapBuilder;
    use crate::storage::index_persistence::{
        PersistenceConfig,
        formats::{IndexManifest, PersistedNode},
        graph::{new_graph_index_data, persist_property_map, save_graph_index},
    };
    use tempfile::tempdir;

    // 1. Setup persistence directory
    let dir = tempdir().unwrap();
    let data_dir = dir.path().to_path_buf();
    let manager = crate::storage::index_persistence::IndexPersistenceManager::new(&data_dir);
    manager.ensure_directories().unwrap();

    // 2. Save a dummy graph index
    let mut graph_data = new_graph_index_data();
    let props = PropertyMapBuilder::new().insert("name", "Test").build();
    let persisted_props = persist_property_map(&props).unwrap();

    // We need interned strings to be available
    crate::core::GLOBAL_INTERNER.intern("TestLabel").unwrap();

    graph_data.nodes.push(PersistedNode {
        id: 1,
        label_idx: crate::core::GLOBAL_INTERNER
            .intern("TestLabel")
            .unwrap()
            .as_u32(),
        version_id: 1,
        properties: persisted_props,
    });
    graph_data.node_count = 1;

    save_graph_index(&graph_data, &manager.graph_path().join("adjacency.idx")).unwrap();

    // Save manifest
    let manifest = IndexManifest::new(0);
    manager.save_manifest(&manifest).unwrap();
    manager.save_string_interner().unwrap();

    // 3. Create DB with load_on_startup = true
    let config = GallifreyDBConfig::builder()
        .persistence(PersistenceConfig {
            enabled: true,
            data_dir: data_dir.clone(),
            load_on_startup: true,
            ..Default::default()
        })
        .build();

    let db = GallifreyDB::with_unified_config(config).unwrap();

    // 4. Verify data loaded
    assert_eq!(db.node_count(), 1);
    let node = db.get_node(crate::core::NodeId::new(1).unwrap()).unwrap();
    let label_str = crate::core::GLOBAL_INTERNER.resolve(node.label).unwrap();
    assert_eq!(label_str.as_ref(), "TestLabel");
}

#[test]
fn test_load_indexes_corrupted_coverage() {
    use crate::config::GallifreyDBConfig;
    use crate::storage::index_persistence::PersistenceConfig;
    use tempfile::tempdir;

    let dir = tempdir().unwrap();
    let data_dir = dir.path().to_path_buf();
    let manager = crate::storage::index_persistence::IndexPersistenceManager::new(&data_dir);
    manager.ensure_directories().unwrap();

    // Create corrupted graph index
    std::fs::write(manager.graph_path().join("adjacency.idx"), b"CORRUPTED").unwrap();

    // Create DB with load_on_startup = true
    let config = GallifreyDBConfig::builder()
        .persistence(PersistenceConfig {
            enabled: true,
            data_dir: data_dir.clone(),
            load_on_startup: true,
            ..Default::default()
        })
        .build();

    // Should not panic, but log error and start empty
    let db = GallifreyDB::with_unified_config(config).unwrap();

    assert_eq!(db.node_count(), 0);
}
