//! Main AletheiaDB database API.
//!
//! This module provides the primary interface to the database, coordinating
//! between current storage (fast path) and historical storage (temporal path).

use crate::api::transaction::{TxIdGenerator, TxVisibilityManager};
use crate::core::id::IdGenerator;
use crate::core::temporal::Timestamp;
use crate::index::temporal::TemporalIndexes;
use crate::query::planner::Statistics;
use crate::storage::current::CurrentStorage;
use crate::storage::historical::HistoricalStorage;
use crate::storage::index_persistence::tracker::PersistenceTracker;
use crate::storage::wal::DurabilityMode;
use crate::storage::wal::concurrent_system::ConcurrentWalSystem;
use parking_lot::RwLock;
use std::sync::{Arc, Mutex};
use std::time::Instant;

/// Administrative and maintenance operations.
pub mod admin;
/// Configuration and initialization.
pub mod config;
/// Basic graph operations (CRUD).
pub mod ops;
/// Query builder and executor hooks.
pub mod query;
/// Temporal query operations.
pub mod temporal;
#[cfg(test)]
/// Unit tests for the database API.
pub mod tests;
/// Transaction management.
pub mod transaction;
/// Vector index operations.
pub mod vector;
/// Vector index builder.
pub mod vector_builder;

/// Main AletheiaDB database.
///
/// This is the primary entry point for interacting with the database.
/// It coordinates between current storage (for fast current-state queries)
/// and historical storage (for temporal queries).
///
/// # Durability Modes
///
/// AletheiaDB supports three durability modes for write transactions:
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
/// ```rust,no_run
/// use aletheiadb::{AletheiaDB, PropertyMapBuilder};
/// use aletheiadb::index::vector::{HnswConfig, DistanceMetric};
///
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// // 1. Initialize the database
/// let db = AletheiaDB::new().expect("Failed to open database");
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
/// # Ok(())
/// # }
/// ```
pub struct AletheiaDB {
    /// Current state storage (hot path) - Arc-wrapped for sharing across transactions
    pub(crate) current: Arc<CurrentStorage>,
    /// Historical version storage (temporal path) - RwLock-protected for concurrent reads
    pub(crate) historical: Arc<RwLock<HistoricalStorage>>,
    /// Temporal indexes for efficient time-based queries - Uses DashMap internally for fine-grained locking
    pub(crate) temporal_indexes: Arc<TemporalIndexes>,
    /// Concurrent Write-Ahead Log for durability - lock-free striped architecture
    pub(crate) wal: Arc<ConcurrentWalSystem>,
    /// Current logical timestamp for transaction time - Mutex-protected for thread-safe increment
    pub(crate) current_timestamp: Arc<Mutex<Timestamp>>,
    /// Monotonic observation time for adaptive forward clock-skew limits.
    pub(crate) commit_clock_observed_at: Arc<Mutex<Instant>>,
    /// Transaction ID generator for MVCC
    pub(crate) tx_id_gen: Arc<TxIdGenerator>,
    /// Transaction visibility manager for Snapshot Isolation
    pub(crate) visibility_manager: Arc<TxVisibilityManager>,
    /// ID generators for nodes, edges, and versions (shared with transactions)
    /// IdGenerator uses AtomicU64 internally, so no external Mutex is needed.
    pub(crate) node_id_gen: Arc<IdGenerator>,
    pub(crate) edge_id_gen: Arc<IdGenerator>,
    pub(crate) version_id_gen: Arc<IdGenerator>,
    /// Default durability mode for write transactions
    pub(crate) default_durability: DurabilityMode,
    /// Query optimization statistics - cached across queries for effective cost-based optimization
    pub(crate) stats: Arc<Statistics>,
    /// Index persistence configuration (stored for potential future use)
    #[allow(dead_code)]
    pub(crate) persistence_config: crate::storage::index_persistence::PersistenceConfig,
    /// Index persistence manager (if enabled)
    pub(crate) persistence_manager:
        Option<Arc<crate::storage::index_persistence::IndexPersistenceManager>>,
    /// Persistence mutation tracking
    pub(crate) persistence_tracker: Option<Arc<PersistenceTracker>>,
    /// Background persistence thread health flag - set to true if thread panics or stops
    pub(crate) persistence_thread_stopped: Arc<std::sync::atomic::AtomicBool>,
    /// Background persistence thread handle (if enabled) - used to join thread on shutdown
    pub(crate) persistence_thread_handle: Option<std::thread::JoinHandle<()>>,
}

impl Drop for AletheiaDB {
    fn drop(&mut self) {
        // Signal shutdown to background persistence thread
        if let Some(ref tracker) = self.persistence_tracker {
            tracker.signal_shutdown();

            // Wait for the background thread to fully exit and release all resources
            // This ensures the thread drops its Arc references to TieredStorage/RedbColdStorage
            // before Drop returns, preventing file locking issues when reopening Redb.
            if let Some(handle) = self.persistence_thread_handle.take() {
                let _ = handle.join();
            }
        }
    }
}
