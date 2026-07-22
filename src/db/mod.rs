//! Main AletheiaDB database API.
//!
//! This module provides the primary interface to the database, coordinating
//! between current storage (fast path) and historical storage (temporal path).

use crate::api::transaction::TxVisibilityManager;
use crate::core::id::{IdGenerator, TxIdGenerator};
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
/// Backup and restore operations.
///
/// Depends on the `backup` storage module (zstd/comfy-table) which is absent on
/// wasm32; the ephemeral wasm profile has no on-disk backup/restore.
#[cfg(not(target_arch = "wasm32"))]
pub mod backup;
/// Provenance hash chain integration: capture, rebuild, and verification API
/// (Issue #3351).
pub mod chain;
/// A `VersionSource` over historical storage for the provenance chain.
pub(crate) mod chain_source;
/// Push-changefeed subscription API (Issue #3375).
pub mod changefeed_sub;
/// Configuration and initialization.
pub mod config;
/// Uniqueness constraint builder.
pub mod constraint_builder;
/// GDPR crypto-shred: per-subject key axis + designation/erasure (Issue #3359).
///
/// Gated on `audit-export` (a default feature): the signed erasure attestation
/// reuses the audit Ed25519 signing key, so the module needs `crate::audit`.
#[cfg(feature = "audit-export")]
pub mod crypto_shred;
/// Crash-safe durable file write primitive (temp → fsync → rename → dir fsync).
#[cfg(not(target_arch = "wasm32"))]
pub(crate) mod durable_write;
/// Encrypted → plaintext migration engine — `encryption disable` (Issue #3616 PR4).
///
/// Disk/encryption migration engine (redb cold tier + index re-encryption);
/// absent on the wasm32 ephemeral profile.
#[cfg(not(target_arch = "wasm32"))]
pub mod encryption_disable;
/// Plaintext → encrypted-at-rest migration engine — `encryption enable` (Issue #3616 PR3).
///
/// Disk/encryption migration engine (redb cold tier + index persistence worker);
/// absent on the wasm32 ephemeral profile.
#[cfg(not(target_arch = "wasm32"))]
pub mod encryption_enable;
/// Durable encryption-state authority — `{data_dir}/encryption.state` (Issue #3616).
pub(crate) mod encryption_state;
/// Queryable bi-temporal extent of the dataset (Issue #3238).
pub mod extent;
/// Provenance-weighted retrieval fusion (Issue #3372).
#[cfg(feature = "semantic-retrieval-fusion")]
pub mod fusion;
/// GraphView implementation.
pub mod graph_view;
/// Fact-to-fact derivation lineage API (Issue #3371).
pub mod lineage;
/// Agent-scoped memory namespaces: registry + namespaced entity creation (Issue #3349).
pub mod namespace;
/// Namespace-scoped reads and traversal (Issue #3349, PR2).
pub mod namespace_query;
/// Basic graph operations (CRUD).
pub mod ops;
/// Point-in-time restore (PITR) to a transaction-time coordinate (Issue #3374).
///
/// Durability feature (WAL replay + backup restore) absent on the wasm32
/// ephemeral profile.
#[cfg(not(target_arch = "wasm32"))]
pub mod pitr;
/// Secondary property (equality) index: opt-in, node-only, current-state.
pub mod property_index;
/// Query builder and executor hooks.
pub mod query;
/// Index-layer key rotation orchestration (Issue #488).
///
/// Disk/encryption key-rotation engine (redb cold tier + index re-encryption);
/// absent on the wasm32 ephemeral profile. The shared crash-safe file-write
/// primitive lives in [`durable_write`], which is likewise a native-only
/// durability concern.
#[cfg(not(target_arch = "wasm32"))]
pub mod rotation;
/// Graph schema discovery (labels, edge types, property keys).
pub mod schema;

pub mod schema_constraint;
/// Unified vector similarity query builder.
pub mod similarity_query;
/// Named snapshots for reproducible reads (Issue #3370).
pub mod snapshot;
/// Database-wide statistics snapshot (Issue #3222).
pub mod stats;
/// Temporal query operations.
pub mod temporal;
#[cfg(test)]
/// Unit tests for the database API.
pub mod tests;
/// Transaction management.
pub mod transaction;
/// Trust propagation: computed confidence over derivation lineage (Issue #3382).
#[cfg(feature = "semantic-reasoning")]
pub mod trust;
/// Vector index operations.
pub mod vector;
/// Vector index builder pattern.
pub mod vector_builder;
/// Durable workflow-execution journal — schema convention + exactly-once step
/// recording (DBOS Phase 3a, `durable-execution` feature).
#[cfg(feature = "durable-execution")]
pub mod workflow;

#[cfg(not(target_arch = "wasm32"))]
pub use crate::storage::backup::BackupSummary;
pub use constraint_builder::UniqueConstraintBuilder;
#[cfg(feature = "audit-export")]
pub use crypto_shred::{
    CryptoShredError, DesignationTarget, ErasureAttestation, ShredStatus, ShredStatusMap,
    SubjectId, SubjectKey,
};
pub use extent::{LabelExtent, TemporalExtent, TimeBounds};
#[cfg(feature = "semantic-retrieval-fusion")]
pub use fusion::{
    FusedHit, FusionBreakdown, FusionError, FusionPolicy, FusionPolicyBuilder, FusionPolicyError,
};
pub use lineage::{FactStatus, LineageView, LineageViewEntry};
pub use namespace::NamespaceInfo;
pub use namespace_query::NamespaceCount;
pub use ops::NodesAtTime;
#[cfg(not(target_arch = "wasm32"))]
pub use pitr::{PitrCoord, PitrPlan, PitrTarget};
pub use property_index::PropertyIndexBuilder;
pub use schema::{EdgeTypeSchema, GraphSchema, LabelSchema, SchemaInstant};
pub use similarity_query::{SimilarityQuery, SimilaritySource};
pub use snapshot::{NamedSnapshot, Snapshot};
pub use stats::{
    ColdStorageDetails, ColdStorageTierStats, CurrentStateStats, DatabaseStats,
    HistoricalDepthStats, TierAccessStats, WalStateStats,
};
pub use vector_builder::VectorIndexBuilder;
#[cfg(feature = "durable-execution")]
pub use workflow::{
    CreateRunSpec, StepExecError, StepOutcome, StepRecord, StepRecordSpec, StepStatus, StepValue,
    WorkflowError, WorkflowJournal, WorkflowJournalExt, WorkflowRun, WorkflowStatus,
};

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
/// Use [`with_unified_config`](Self::with_unified_config) to configure the database settings,
/// including durability modes. You can also use [`write_with_options`](Self::write_with_options)
/// for per-transaction overrides.
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
    // Lock ordering for write-path primitives:
    // 1. `current_timestamp`
    // 2. `wal`
    // 3. `historical`
    // 4. `temporal_indexes`
    // 5. `id generators`
    // 6. `outgoing`
    // 7. `incoming`
    //
    // Some entries are currently lock-free or internally sharded, but future
    // explicit locks must preserve this order to avoid deadlocks.
    /// Historical version storage (temporal path) - RwLock-protected for concurrent reads
    pub(crate) historical: Arc<RwLock<HistoricalStorage>>,
    /// Temporal indexes for efficient time-based queries - Uses DashMap internally for fine-grained locking
    pub(crate) temporal_indexes: Arc<TemporalIndexes>,
    /// Concurrent Write-Ahead Log for durability - lock-free striped architecture
    pub(crate) wal: Arc<ConcurrentWalSystem>,
    /// In-flight (durable-but-not-yet-applied) LSN tracker (lost-write persist
    /// race fix). Records the base LSN of every commit that is fsynced but not
    /// yet applied to current/historical storage. Index persistence stamps the
    /// manifest with `min(in_flight)` — the applied watermark — instead of the
    /// WAL allocation frontier, so replay always re-covers a durable-but-
    /// unapplied write instead of dropping it. Its internal mutex is a leaf
    /// (never held while acquiring another write-path primitive).
    pub(crate) in_flight: Arc<crate::api::transaction::write::InFlightLsns>,
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
    // Read only by the native-only background-persistence / encryption engines; on the
    // wasm32 ephemeral profile it is constructed but never read.
    #[cfg_attr(target_arch = "wasm32", allow(dead_code))]
    pub(crate) persistence_thread_stopped: Arc<std::sync::atomic::AtomicBool>,
    /// Background persistence thread handle (if enabled) - used to join thread on shutdown
    pub(crate) persistence_thread_handle: Option<std::thread::JoinHandle<()>>,
    /// Encryption manager (if encryption at rest is enabled)
    pub(crate) encryption_manager: Option<Arc<crate::encryption::EncryptionManager>>,
    /// Encryption configuration used at startup (retained for key rotation,
    /// Issue #488: re-sourcing the current MEK to guard against rotating to the
    /// same key). `None` when encryption is disabled.
    // Read only by the native-only key-rotation / encryption-enable-disable engines.
    #[cfg_attr(target_arch = "wasm32", allow(dead_code))]
    pub(crate) encryption_config: Option<crate::encryption::config::EncryptionConfig>,
    /// The AEAD algorithm from `config.encryption.algorithm`, retained REGARDLESS
    /// of whether encryption is currently enabled (Issue #3616 PR3). A plaintext
    /// database has no `encryption_config`, but `enable_encryption` must still write
    /// its at-rest bytes under the operator's configured algorithm (and pin the
    /// resolved concrete form into the authority) so the reopen — which builds its
    /// ciphers from `config.encryption.algorithm` — reads them back under the
    /// identical cipher. On the interrupted-enable resume path this also keeps the
    /// wrap passes in lockstep with the ciphers `open()` built from the same config
    /// algorithm.
    // Read only by the native-only encryption-enable/disable engines.
    #[cfg_attr(target_arch = "wasm32", allow(dead_code))]
    pub(crate) configured_encryption_algorithm: crate::encryption::factory::Algorithm,
    /// Uniqueness constraint registry (declarations + reservation index) plus
    /// property-type / required-key schema constraints (Issue #3378).
    pub(crate) constraint_registry: Arc<crate::core::constraint::ConstraintRegistry>,
    /// On-disk path for the schema-constraint sidecar (Issue #3378), or `None`
    /// for an ephemeral (in-memory) database. When `Some`, declaring/dropping
    /// schema constraints atomically rewrites this file, and it is loaded at
    /// startup. Lives under the data dir (the parent of the WAL dir), mirroring
    /// the provenance-chain sidecar's placement.
    pub(crate) schema_constraint_path: Option<std::path::PathBuf>,
    /// GDPR crypto-shred state (Issue #3359): the per-subject key registry, its
    /// durable paths, and the attestation signing key. In-memory-only for an
    /// ephemeral database (no index persistence). Loaded fail-closed at startup
    /// with breadcrumb crash-recovery.
    #[cfg(feature = "audit-export")]
    pub(crate) crypto_shred: Arc<crate::db::crypto_shred::CryptoShredState>,
    /// Fact-to-fact derivation lineage index (Issue #3371).
    ///
    /// Records that a fact version was derived from a set of source fact
    /// versions and answers upstream/downstream closure queries. Immutable,
    /// append-only, and independent of the graph/version stores so retracting
    /// or superseding a fact never disturbs lineage pointing at it. v1 is
    /// in-memory (does not survive restart) to keep the WAL format untouched
    /// (Issue #3413); see [`crate::core::lineage`].
    pub(crate) lineage: Arc<crate::core::lineage::LineageStore>,
    /// Push-changefeed broadcaster (Issue #3375).
    ///
    /// Fan-out hub for [`AletheiaDB::subscribe_changes`]. Each committed write transaction
    /// broadcasts its change set (byte-identical to what `list_changes` would return for
    /// that window) to matching subscribers, after the write is durable + applied + visible
    /// and outside every write-path lock. Best-effort at-least-once: the durable ground
    /// truth is `list_changes`, and a lagged/reconnecting/crash-surviving consumer resumes
    /// losslessly from its last cursor. Its internal locks are leaves (never held while
    /// acquiring a write-path primitive).
    pub(crate) changefeed: Arc<crate::core::changefeed_subscription::ChangefeedBroadcaster>,
    /// Named-snapshot registry for reproducible reads (Issue #3370).
    ///
    /// Maps a human-readable name to a pinned bi-temporal coordinate. Entirely
    /// off the data write path (creating a snapshot never touches
    /// current/historical storage, the WAL, or `current_timestamp`), so it
    /// adds zero overhead to `create_node`/`create_edge`. Durably persisted to
    /// a `{data_dir}/snapshots.json` sidecar when index persistence is enabled;
    /// in-memory-only for ephemeral databases. See [`crate::db::snapshot`].
    pub(crate) snapshots: Arc<snapshot::SnapshotRegistry>,
    /// Agent-scoped namespace registry (Issue #3349).
    ///
    /// Maps a namespace name to its `{description, created_at}` metadata so
    /// namespaces are creatable / listable / describable even when empty.
    /// Auto-registered on first write to an unknown namespace. Entirely off the
    /// data write path (a leaf, like [`snapshots`](Self::snapshots)). Durably
    /// persisted to a `{data_dir}/namespaces.json` sidecar when index
    /// persistence is enabled; in-memory-only for ephemeral databases. See
    /// [`crate::db::namespace`].
    pub(crate) namespaces: Arc<namespace::NamespaceRegistry>,
    /// Trust-propagation policy registry (Issue #3382).
    ///
    /// Maps the database default combinator + missing-confidence rule, plus any
    /// per-label overrides, used to compute a derived fact's confidence from its
    /// upstream lineage. Entirely off the data write path (a leaf, like
    /// [`snapshots`](Self::snapshots)). Durably persisted to a
    /// `{data_dir}/trust_policy.json` sidecar when index persistence is enabled;
    /// in-memory-only for ephemeral databases. Gated to the experimental
    /// `semantic-reasoning` cohort so it compiles out (zero cost) when the
    /// feature is off. See [`crate::experimental::reasoning::trust_propagation`].
    #[cfg(feature = "semantic-reasoning")]
    pub(crate) trust_policies:
        Arc<crate::experimental::reasoning::trust_propagation::TrustRegistry>,
    /// Knowledge half-life analytics cohort-statistics cache (Issue #3377,
    /// Fix-B). In-memory, recomputable, and **off the write path** (no
    /// write-time hook): it backs the sub-millisecond warm `fact_freshness`
    /// path by memoizing a cohort's `(VolatilityStats, KmCurve)` keyed on the
    /// full result determinant `(cohort, as_of, min_events, max_entities)`.
    /// `as_of == None` ("now") entries are invalidated lazily by comparing the
    /// WAL append generation captured at compute against the current one (an
    /// O(1) atomic read) — no write-path cost; `as_of == Some(_)` entries pin an
    /// immutable past tx snapshot and are cached indefinitely. Its internal
    /// mutex is a leaf (never held while acquiring a write-path primitive, and
    /// no storage read happens under it). Gated to `semantic-temporal`; the
    /// field compiles out entirely when the feature is off.
    #[cfg(feature = "semantic-temporal")]
    pub(crate) half_life_cache: Arc<crate::experimental::temporal::half_life::CohortStatsCache>,
    /// Temporal semantic drift-monitor registry (Issue #3367).
    ///
    /// Declarative rules that watch an embedding property's evolution against a
    /// threshold + window and materialize durable, bi-temporal `__drift_alarm`
    /// nodes when meaning moves. Entirely off the data write path (a leaf, like
    /// [`snapshots`](Self::snapshots)). Durably persisted to a
    /// `{data_dir}/drift_monitors.json` sidecar when index persistence is
    /// enabled; in-memory-only for ephemeral databases. Gated to the
    /// experimental `semantic-temporal` cohort so it is compiled out (zero cost)
    /// when the feature is off. See
    /// [`crate::experimental::temporal::drift_alarm`].
    #[cfg(feature = "semantic-temporal")]
    pub(crate) drift_monitors:
        Arc<crate::experimental::temporal::drift_alarm::DriftMonitorRegistry>,
    /// Opt-in tamper-evident provenance hash chain (Issue #3351).
    ///
    /// `None` unless `config.chain.enabled`. When present, each committed
    /// transaction's version refs are enqueued to a background sealer that
    /// folds them into a domain-separated SHA-256 hash chain over recorded
    /// history. Declared before `_tempdir` so it is dropped (flushing the
    /// sealer) before the tempdir is removed.
    pub(crate) chain: Option<Arc<crate::provenance_chain::ProvenanceChain>>,
    /// Engine-lane per-query resource-limits configuration (Issue #3368
    /// public API): server default + operator ceiling per dimension, consulted
    /// by [`execute_query`](crate::db::AletheiaDB::execute_query) to resolve
    /// each query's effective [`QueryResourceLimits`](crate::query::limits::QueryResourceLimits)
    /// from the query's own [`QueryLimitsOverride`](crate::query::limits::QueryLimitsOverride)
    /// (if any). Entirely off the write path (read-only, consulted per query).
    pub(crate) query_limits: crate::query::limits::EngineQueryLimitsConfig,
    /// Shared per-dimension counters of engine-lane resource-limit terminations
    /// and rejected overrides (Issue #3368 observability), surfaced via
    /// [`AletheiaDB::query_limit_counters`] and folded into
    /// [`stats`](crate::db::AletheiaDB::stats)'s `resource_limits` block.
    pub(crate) limit_counters: Arc<crate::query::limits::LimitCounters>,
    /// Backing tempdir for ephemeral databases created via [`AletheiaDB::new`].
    /// Declared last so it is dropped last (Rust drops struct fields in
    /// declaration order); this guarantees the WAL/persistence file handles
    /// above have already been closed before the directory is removed.
    pub(crate) _tempdir: Option<tempfile::TempDir>,
}

impl std::fmt::Debug for AletheiaDB {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let current_ts = self
            .current_timestamp
            .try_lock()
            .map(|ts| format!("{:?}", ts))
            .unwrap_or_else(|_| "<locked>".to_string());

        f.debug_struct("AletheiaDB")
            .field("current_timestamp", &current_ts)
            .field("default_durability", &self.default_durability)
            .field("persistence_enabled", &self.persistence_manager.is_some())
            .field("encryption_enabled", &self.encryption_manager.is_some())
            .field("stats", &self.stats)
            .finish_non_exhaustive()
    }
}

impl Drop for AletheiaDB {
    fn drop(&mut self) {
        // Flush and stop the provenance-chain sealer first so its final records
        // and head checkpoint are durable before storage handles are torn down
        // (Issue #3351). Idempotent with the chain's own Drop.
        if let Some(ref chain) = self.chain {
            chain.shutdown();
        }

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
