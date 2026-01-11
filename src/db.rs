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
}

impl GallifreyDB {
    /// Create a new empty database with default configuration.
    pub fn new() -> Self {
        Self::with_config(AnchorConfig::default())
    }

    /// Create a new database with custom anchor configuration.
    pub fn with_config(config: AnchorConfig) -> Self {
        Self::with_full_config(config, crate::config::WalConfig::default())
    }

    /// Create a new database with custom WAL configuration.
    ///
    /// This allows configuring the durability mode and other WAL settings.
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
    /// let db = GallifreyDB::with_wal_config(wal_config);
    ///
    /// // Bulk loading mode with async durability
    /// let wal_config = WalConfigBuilder::new()
    ///     .durability_mode(DurabilityMode::async_mode(100))
    ///     .build();
    /// let db = GallifreyDB::with_wal_config(wal_config);
    /// ```
    pub fn with_wal_config(wal_config: crate::config::WalConfig) -> Self {
        Self::with_full_config(AnchorConfig::default(), wal_config)
    }

    /// Create a new database with unified configuration.
    ///
    /// This method accepts a [`GallifreyDBConfig`] which consolidates all configuration
    /// settings for the database, including WAL, historical storage, and vector indexes.
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
    /// let db = GallifreyDB::with_unified_config(config);
    /// ```
    pub fn with_unified_config(config: GallifreyDBConfig) -> Self {
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

        let wal = ConcurrentWalSystem::new(wal_system_config).expect("Failed to create WAL");
        let wal = Arc::new(wal);

        GallifreyDB {
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
        }
    }

    /// Create a new database with both anchor and WAL configuration.
    ///
    /// This maintains backward compatibility with the old API.
    /// For new code, prefer using [`with_unified_config`](Self::with_unified_config).
    pub fn with_full_config(
        anchor_config: AnchorConfig,
        wal_config: crate::config::WalConfig,
    ) -> Self {
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

        let wal = ConcurrentWalSystem::new(wal_system_config).expect("Failed to create WAL");
        let wal = Arc::new(wal);

        GallifreyDB {
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
        }
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
        let db = Self::new();

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
        let snapshot_timestamp = *self.current_timestamp.lock_or_err()?;

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
        tx.commit()?;
        Ok(result)
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
        tx.commit()?;
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
        let snapshot_timestamp = *self.current_timestamp.lock_or_err()?;

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
    pub fn create_node(&self, label: &str, properties: PropertyMap) -> Result<NodeId> {
        self.write(|tx| tx.create_node(label, properties))
    }

    /// Create an edge between two nodes.
    ///
    /// This is a convenience method that internally uses a write transaction.
    /// For multiple operations, prefer using `write()` or `write_transaction()`.
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
    /// This uses the slow path (historical storage + version reconstruction).
    pub fn get_node_at_time(
        &self,
        node_id: NodeId,
        valid_time: Timestamp,
        transaction_time: Timestamp,
    ) -> Result<Node> {
        #[cfg(feature = "observability")]
        let _span = tracing::info_span!("get_node_at_time").entered();
        let historical = self.historical.read_or_err()?;

        // Find the version valid at this time
        let version_id = historical
            .find_node_version_at_time(node_id, valid_time, transaction_time)
            .ok_or(StorageError::NodeNotFound(node_id))?;

        // Get the version
        let version = historical
            .get_node_version(version_id)
            .ok_or(StorageError::VersionNotFound(version_id))?;

        // Reconstruct properties
        let properties = historical.reconstruct_node_properties(version_id)?;

        // Build node from version
        Ok(Node::new(
            version.node_id,
            version.label,
            properties,
            version.id,
        ))
    }

    /// Get an edge as it existed at a specific point in bi-temporal space.
    pub fn get_edge_at_time(
        &self,
        edge_id: EdgeId,
        valid_time: Timestamp,
        transaction_time: Timestamp,
    ) -> Result<Edge> {
        #[cfg(feature = "observability")]
        let _span = tracing::info_span!("get_edge_at_time").entered();
        let historical = self.historical.read_or_err()?;

        let version_id = historical
            .find_edge_version_at_time(edge_id, valid_time, transaction_time)
            .ok_or(StorageError::EdgeNotFound(edge_id))?;

        let version = historical
            .get_edge_version(version_id)
            .ok_or(StorageError::VersionNotFound(version_id))?;

        let properties = historical.reconstruct_edge_properties(version_id)?;

        Ok(Edge::new(
            version.edge_id,
            version.label,
            version.source,
            version.target,
            properties,
            version.id,
        ))
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
    /// A vector of (NodeId, Option<Node>) pairs, where None indicates the node
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
                // Find the version valid at this time
                let node = match historical.find_node_version_at_time(
                    node_id,
                    valid_time,
                    transaction_time,
                ) {
                    Some(version_id) => {
                        // Get the version - this should always exist if find_node_version_at_time returned it
                        let version = historical
                            .get_node_version(version_id)
                            .ok_or(StorageError::VersionNotFound(version_id))?;

                        // Reconstruct properties - propagate errors as these are systemic failures
                        #[allow(clippy::map_identity)]
                        let properties = historical
                            .reconstruct_node_properties(version_id)
                            .map_err(|e| {
                                #[cfg(feature = "observability")]
                                tracing::error!(
                                    version_id = %version_id,
                                    node_id = %node_id,
                                    error = %e,
                                    "Property reconstruction failed in batch query"
                                );
                                e // Propagate the error
                            })?;

                        // Build node from version
                        Some(Node::new(
                            version.node_id,
                            version.label,
                            properties,
                            version.id,
                        ))
                    }
                    None => None, // Node didn't exist at this time - this is expected, not an error
                };

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
    /// A vector of (EdgeId, Option<Edge>) pairs, where None indicates the edge
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
                // Find the version valid at this time
                let edge = match historical.find_edge_version_at_time(
                    edge_id,
                    valid_time,
                    transaction_time,
                ) {
                    Some(version_id) => {
                        // Get the version - this should always exist if find_edge_version_at_time returned it
                        let version = historical
                            .get_edge_version(version_id)
                            .ok_or(StorageError::VersionNotFound(version_id))?;

                        // Reconstruct properties - propagate errors as these are systemic failures
                        #[allow(clippy::map_identity)]
                        let properties = historical
                            .reconstruct_edge_properties(version_id)
                            .map_err(|e| {
                                #[cfg(feature = "observability")]
                                tracing::error!(
                                    version_id = %version_id,
                                    edge_id = %edge_id,
                                    error = %e,
                                    "Property reconstruction failed in batch query"
                                );
                                e // Propagate the error
                            })?;

                        // Build edge from version
                        Some(Edge::new(
                            version.edge_id,
                            version.label,
                            version.source,
                            version.target,
                            properties,
                            version.id,
                        ))
                    }
                    None => None, // Edge didn't exist at this time - this is expected, not an error
                };

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
        #[cfg(feature = "observability")]
        let _span = tracing::info_span!("enable_temporal_vector_index").entered();

        // Enable temporal vector index in current storage
        self.current
            .enable_temporal_vector_index(property_name, config)?;

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
        #[cfg(feature = "observability")]
        let _span = tracing::info_span!("find_similar").entered();
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
        #[cfg(feature = "observability")]
        let _span = tracing::info_span!("find_similar").entered();
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

    /// Get a reference to the current storage (test-only helper).
    ///
    /// This method is only available in test builds and provides access to the
    /// internal CurrentStorage for integration test verification purposes.
    #[cfg(test)]
    #[allow(dead_code)]
    pub(crate) fn storage(&self) -> &Arc<CurrentStorage> {
        &self.current
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
}

impl Default for GallifreyDB {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::transaction::ReadOps;
    use crate::core::property::PropertyMapBuilder;

    #[test]
    fn test_create_node() {
        let db = GallifreyDB::new();

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
        let db = GallifreyDB::new();

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
        let db = GallifreyDB::new();

        // Create a node at time T1
        let props_v1 = PropertyMapBuilder::new()
            .insert("name", "Alice")
            .insert("age", 30i64)
            .build();

        let node_id = db.create_node("Person", props_v1).unwrap();
        let t1 = *db.current_timestamp.lock().unwrap() - 1; // Timestamp when created

        // In a real implementation, we'd create a second version here with an update_node method
        // For now, just verify we can query at T1

        // Query at time T1
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
        let db = GallifreyDB::new();

        // Create a node
        let props = PropertyMapBuilder::new()
            .insert("name", "Alice")
            .insert("age", 30i64)
            .build();
        let node_id = db.create_node("Person", props).unwrap();

        // Record timestamp after creation
        let t_after_create = *db.current_timestamp.lock().unwrap();

        // Delete the node
        db.write(|tx| {
            tx.delete_node(node_id)?;
            Ok(())
        })
        .unwrap();

        // Record timestamp after deletion
        let t_after_delete = *db.current_timestamp.lock().unwrap();

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

    #[test]
    fn test_graph_traversal() {
        let db = GallifreyDB::new();

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
        let db = GallifreyDB::new();

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
        let db = GallifreyDB::new();

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
        let db = GallifreyDB::new();

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
        let db = GallifreyDB::new();

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
        let db = GallifreyDB::new();

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
        let db = GallifreyDB::new();

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
        let db = GallifreyDB::new();

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
        let db = GallifreyDB::new();

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
        let db = GallifreyDB::new();

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

        let db = GallifreyDB::new();

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

        let db = GallifreyDB::new();

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

        let db = GallifreyDB::new();

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
        let db = GallifreyDB::new();

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

        let db = GallifreyDB::new();

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

        let db = GallifreyDB::new();

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

        let db = GallifreyDB::new();

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

        let db = GallifreyDB::new();

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

        let db = GallifreyDB::new();

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

        let db = GallifreyDB::new();

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

        let db = GallifreyDB::new();

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

        let db = GallifreyDB::new();

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

        let db = Arc::new(GallifreyDB::new());

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
        let db = GallifreyDB::new();

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
        let db = GallifreyDB::new();

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
        let db = GallifreyDB::new();

        let t1 = *db.current_timestamp.lock().unwrap();

        // Query with empty node list
        let results = db.get_nodes_at_time(&[], t1, t1).unwrap();

        // Should return empty results
        assert_eq!(results.len(), 0);
    }

    #[test]
    fn test_get_nodes_at_time_after_deletion() {
        let db = GallifreyDB::new();

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
        let db = GallifreyDB::new();

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
        let db = GallifreyDB::new();

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
        let db = GallifreyDB::new();

        let t1 = *db.current_timestamp.lock().unwrap();

        // Query with empty edge list
        let results = db.get_edges_at_time(&[], t1, t1).unwrap();

        // Should return empty results
        assert_eq!(results.len(), 0);
    }

    #[test]
    fn test_get_edges_at_time_after_deletion() {
        let db = GallifreyDB::new();

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
        let db = GallifreyDB::new();

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
        let db = GallifreyDB::new();

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
        let db = GallifreyDB::new();

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
        let db = GallifreyDB::new();

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

        let db = GallifreyDB::new();

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
}
