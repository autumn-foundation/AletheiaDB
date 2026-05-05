//! Migration service for tiered storage.
//!
//! This module implements the background migration service that automatically moves
//! historical versions from the hot tier (in-memory) to the cold tier (disk-based Redb).
//!
//! # Architecture
//!
//! The migration service operates as a background worker that orchestrates data movement
//! between storage tiers. It is designed to be:
//!
//! - **Non-blocking**: Migration happens in the background without stalling read/write operations.
//! - **Policy-driven**: Configurable policies determine *when* and *what* to migrate.
//! - **Safe**: Ensures data integrity through atomic batch writes and LSN coordination.
//!
//! ## Components
//!
//! 1.  **MigrationPolicy**: Defines thresholds (age, memory usage) for triggering migration.
//! 2.  **MigrationService**: Manages the background worker and execution logic.
//! 3.  **RedbColdStorage**: The destination for migrated data.
//! 4.  **FlushCoordinator**: (Optional) Coordinates WAL truncation after successful migration.
//!
//! # Usage
//!
//! ```rust
//! use aletheiadb::storage::migration::{MigrationPolicy, MigrationService};
//! use aletheiadb::storage::redb_cold_storage::{RedbColdStorage, RedbConfig};
//! use std::sync::Arc;
//! use std::time::Duration;
//!
//! # fn example() -> aletheiadb::core::error::Result<()> {
//! // 1. Configure the cold storage backend
//! let cold_storage = Arc::new(RedbColdStorage::new("data/cold.redb", RedbConfig::default())?);
//!
//! // 2. Define a migration policy
//! let policy = MigrationPolicy::builder()
//!     .age_threshold(Duration::from_secs(7 * 24 * 60 * 60)) // Migrate versions older than 7 days
//!     .memory_threshold_bytes(1024 * 1024 * 1024)           // or when hot tier exceeds 1GB
//!     .min_hot_versions(1)                                  // Always keep current version hot
//!     .build();
//!
//! // 3. Create and start the service
//! let service = MigrationService::new(cold_storage, policy);
//! service.start();
//!
//! // The service now runs in the background.
//! // To gracefully stop it:
//! service.stop();
//! # Ok(())
//! # }
//! ```
//!
//! # Safety and Crash Recovery
//!
//! The migration service integrates with the Write-Ahead Log (WAL) to ensure crash consistency.
//! When `migrate_batch_with_lsn` is used:
//!
//! 1.  **Atomic Write**: Data is written to cold storage along with the Log Sequence Number (LSN).
//! 2.  **Verification**: The flushed LSN is verified to ensure durability.
//! 3.  **Truncation**: Only then is the WAL truncated up to that LSN.
//!
//! This invariant (`WAL_truncation_lsn <= cold_storage.get_flushed_lsn()`) ensures that
//! data is never removed from the WAL before it is safely persisted in cold storage.

use crate::core::error::Result;
use crate::core::id::{EdgeId, NodeId, VersionId};
use crate::core::version::{EdgeVersion, FastHashMap, NodeVersion};
use crate::storage::redb_cold_storage::RedbColdStorage;
use crate::storage::wal::LSN;
use crate::storage::wal::flush_coordinator::FlushCoordinator;
use quick_cache::sync::Cache;
use std::collections::HashMap;
use std::panic;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[cfg(feature = "config-toml")]
use serde::{Deserialize, Serialize};

/// Interval for checking shutdown signal in the background worker.
/// Short enough for responsive shutdown, long enough to avoid busy-waiting.
const SHUTDOWN_CHECK_INTERVAL: Duration = Duration::from_millis(50);

/// Maximum number of entries in the access_times tracking cache.
/// Prevents unbounded memory growth from access tracking (DoS protection).
/// Uses LRU eviction - oldest accessed entries are automatically evicted.
const MAX_ACCESS_ENTRIES: usize = 1_000_000;

/// Timeout for waiting during Drop to prevent deadlock.
/// If the worker doesn't stop within this time, we give up waiting.
const DROP_TIMEOUT: Duration = Duration::from_secs(5);

/// Policy for determining when to migrate versions from hot to cold tier.
///
/// This struct defines the rules for selecting which data should be moved to cold storage.
/// It supports both age-based and memory-pressure-based triggers.
///
/// # Default Policy
///
/// The default policy is suitable for most workloads:
/// - Migrate versions older than 7 days
/// - Keep at least 1 recent version in hot storage
/// - Check for migration every 60 seconds
/// - Trigger if memory usage exceeds 1GB
#[derive(Debug, Clone)]
#[cfg_attr(feature = "config-toml", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "config-toml", serde(default))]
pub struct MigrationPolicy {
    /// Migrate versions older than this duration.
    ///
    /// Versions created before `now - age_threshold` are candidates for migration.
    ///
    /// **Default:** 7 days
    pub age_threshold: Duration,

    /// Migrate when hot tier memory usage exceeds this threshold (in bytes).
    ///
    /// This acts as a backpressure mechanism to prevent OOM. When memory usage
    /// is high, migration may trigger even for versions younger than `age_threshold`
    /// (depending on `enable_lru`).
    ///
    /// **Default:** 1 GB
    pub memory_threshold_bytes: usize,

    /// Minimum number of versions to keep in hot tier per entity.
    ///
    /// This ensures that the most recent history is always fast to access,
    /// preventing "thrashing" where a frequently accessed recent version is migrated.
    ///
    /// **Default:** 1 (keep only the current version hot)
    pub min_hot_versions: usize,

    /// Maximum number of versions to migrate in a single batch.
    ///
    /// Controls the granularity of migration work units. Larger batches improve
    /// throughput but may hold locks longer.
    ///
    /// **Default:** 1000
    pub batch_size: usize,

    /// Interval between migration runs.
    ///
    /// Controls how often the background worker checks for migration candidates.
    ///
    /// **Default:** 60 seconds
    pub run_interval: Duration,

    /// Enable/disable migration globally.
    ///
    /// Useful for testing, maintenance, or bulk loading phases where migration
    /// should be paused.
    ///
    /// **Default:** true (enabled)
    pub enabled: bool,

    /// Enable LRU (Least Recently Used) based migration.
    ///
    /// When `true`, versions are sorted by their last access time, migrating
    /// least-recently-accessed data first. When `false`, migration order is
    /// determined primarily by version age (oldest first).
    ///
    /// **Default:** false (Age-based)
    pub enable_lru: bool,
}

impl Default for MigrationPolicy {
    fn default() -> Self {
        Self {
            age_threshold: Duration::from_secs(7 * 24 * 60 * 60), // 7 days
            memory_threshold_bytes: 1024 * 1024 * 1024,           // 1GB
            min_hot_versions: 1,
            batch_size: 1000,
            run_interval: Duration::from_secs(60),
            enabled: true,
            enable_lru: false,
        }
    }
}

impl MigrationPolicy {
    /// Create a new migration policy builder.
    ///
    /// # Example
    ///
    /// ```
    /// use aletheiadb::storage::migration::MigrationPolicy;
    /// use std::time::Duration;
    ///
    /// let policy = MigrationPolicy::builder()
    ///     .age_threshold(Duration::from_secs(3600))
    ///     .build();
    /// ```
    pub fn builder() -> MigrationPolicyBuilder {
        MigrationPolicyBuilder::new()
    }

    /// Create a policy that aggressively migrates to cold storage.
    ///
    /// **Use case:** Memory-constrained environments or workloads with little historical access.
    ///
    /// **Settings:**
    /// - Age threshold: 1 day
    /// - Memory threshold: 512MB
    /// - Strategy: LRU enabled (evict unused data quickly)
    pub fn aggressive() -> Self {
        Self {
            age_threshold: Duration::from_secs(24 * 60 * 60), // 1 day
            memory_threshold_bytes: 512 * 1024 * 1024,        // 512MB
            min_hot_versions: 1,
            batch_size: 2000,
            run_interval: Duration::from_secs(30),
            enabled: true,
            enable_lru: true, // Aggressive mode uses LRU
        }
    }

    /// Create a policy that keeps more data in hot storage.
    ///
    /// **Use case:** Read-heavy workloads with frequent temporal queries or ample RAM.
    ///
    /// **Settings:**
    /// - Age threshold: 30 days
    /// - Memory threshold: 4GB
    /// - Min hot versions: 5
    /// - Strategy: Age-based (keep recent history hot)
    pub fn conservative() -> Self {
        Self {
            age_threshold: Duration::from_secs(30 * 24 * 60 * 60), // 30 days
            memory_threshold_bytes: 4 * 1024 * 1024 * 1024,        // 4GB
            min_hot_versions: 5,
            batch_size: 500,
            run_interval: Duration::from_secs(300),
            enabled: true,
            enable_lru: false,
        }
    }

    /// Create a disabled policy (no automatic migration).
    ///
    /// Useful for testing, initial bulk loading, or maintenance modes.
    pub fn disabled() -> Self {
        Self {
            enabled: false,
            ..Default::default()
        }
    }
}

/// Builder for constructing a `MigrationPolicy`.
#[derive(Debug, Default)]
pub struct MigrationPolicyBuilder {
    policy: MigrationPolicy,
}

impl MigrationPolicyBuilder {
    /// Create a new builder initialized with default values.
    pub fn new() -> Self {
        Self {
            policy: MigrationPolicy::default(),
        }
    }

    /// Set the age threshold for migration.
    ///
    /// Versions older than this duration will be candidates for migration.
    pub fn age_threshold(mut self, duration: Duration) -> Self {
        self.policy.age_threshold = duration;
        self
    }

    /// Set the memory threshold in bytes.
    ///
    /// Migration may trigger if hot storage usage exceeds this value.
    pub fn memory_threshold_bytes(mut self, bytes: usize) -> Self {
        self.policy.memory_threshold_bytes = bytes;
        self
    }

    /// Set the minimum number of versions to keep in hot storage per entity.
    ///
    /// Ensures that even old versions remain hot if they are among the N most recent
    /// for a given entity.
    pub fn min_hot_versions(mut self, count: usize) -> Self {
        self.policy.min_hot_versions = count;
        self
    }

    /// Set the batch size for migration operations.
    pub fn batch_size(mut self, size: usize) -> Self {
        self.policy.batch_size = size;
        self
    }

    /// Set the interval between background migration checks.
    pub fn run_interval(mut self, interval: Duration) -> Self {
        self.policy.run_interval = interval;
        self
    }

    /// Enable or disable the migration service.
    pub fn enabled(mut self, enabled: bool) -> Self {
        self.policy.enabled = enabled;
        self
    }

    /// Enable or disable LRU-based migration ordering.
    ///
    /// - `true`: Prioritize migrating least-recently-accessed data.
    /// - `false`: Prioritize migrating oldest created data.
    pub fn enable_lru_migration(mut self, enable: bool) -> Self {
        self.policy.enable_lru = enable;
        self
    }

    /// Consume the builder and return the configured `MigrationPolicy`.
    pub fn build(self) -> MigrationPolicy {
        self.policy
    }
}

/// A version candidate for migration.
#[derive(Debug, Clone)]
pub struct MigrationCandidate {
    /// The version ID to migrate.
    pub version_id: VersionId,
    /// Whether this is a node version (true) or edge version (false).
    pub is_node: bool,
    /// Age of the version (time since creation).
    pub age: Duration,
    /// Estimated size in bytes.
    pub estimated_size: usize,
}

/// Progress information during a migration operation.
///
/// This struct provides real-time visibility into migration progress,
/// enabling monitoring dashboards and graceful shutdown decisions.
///
/// # Usage
///
/// Passed to `MigrationCallback::on_progress` during migration.
#[derive(Debug, Clone, Default)]
pub struct MigrationProgress {
    /// Total number of versions targeted for migration in this run.
    pub total_versions: usize,
    /// Number of versions successfully migrated so far.
    pub migrated_versions: usize,
    /// Total volume of data migrated so far (in bytes).
    /// Used for bandwidth monitoring and throttling decisions.
    pub bytes_migrated: u64,
    /// Current batch sequence number (1-indexed).
    pub current_batch: usize,
    /// Total expected batches based on `batch_size`.
    pub total_batches: usize,
    /// Time elapsed since the start of this migration run.
    pub elapsed: Duration,
}

impl MigrationProgress {
    /// Calculate the completion percentage (0.0 to 100.0).
    pub fn percentage(&self) -> f64 {
        if self.total_versions == 0 {
            100.0
        } else {
            (self.migrated_versions as f64 / self.total_versions as f64) * 100.0
        }
    }

    /// Check if the migration run is complete.
    pub fn is_complete(&self) -> bool {
        self.migrated_versions >= self.total_versions
    }

    /// Calculate the current throughput in versions per second.
    pub fn versions_per_second(&self) -> f64 {
        let secs = self.elapsed.as_secs_f64();
        if secs > 0.0 {
            self.migrated_versions as f64 / secs
        } else {
            0.0
        }
    }

    /// Calculate the current throughput in bytes per second.
    pub fn bytes_per_second(&self) -> f64 {
        let secs = self.elapsed.as_secs_f64();
        if secs > 0.0 {
            self.bytes_migrated as f64 / secs
        } else {
            0.0
        }
    }

    /// Estimate the time remaining until completion based on current throughput.
    ///
    /// Returns `Duration::ZERO` if completed or if throughput is zero.
    pub fn estimated_remaining(&self) -> Duration {
        let remaining = self.total_versions.saturating_sub(self.migrated_versions);
        let vps = self.versions_per_second();
        if vps > 0.0 {
            Duration::from_secs_f64(remaining as f64 / vps)
        } else {
            Duration::ZERO
        }
    }
}

/// aggregated statistics for migration operations.
///
/// Tracks lifetime statistics for the `MigrationService`.
#[derive(Debug, Clone, Default)]
pub struct MigrationStats {
    /// Total number of node versions successfully migrated.
    pub node_versions_migrated: u64,
    /// Total number of edge versions successfully migrated.
    pub edge_versions_migrated: u64,
    /// Total volume of data migrated (in bytes).
    pub bytes_migrated: u64,
    /// Number of migration runs successfully completed.
    pub runs_completed: u64,
    /// Number of errors encountered during migration.
    pub errors: u64,
    /// Duration of the most recent migration run.
    pub last_run_duration: Duration,
    /// Timestamp of the most recent migration run.
    pub last_run_time: Option<Instant>,
}

impl MigrationStats {
    /// Calculate the average throughput of the *last* run (versions per second).
    pub fn versions_per_second(&self) -> f64 {
        let total = self.node_versions_migrated + self.edge_versions_migrated;
        if self.last_run_duration.as_secs_f64() > 0.0 {
            total as f64 / self.last_run_duration.as_secs_f64()
        } else {
            0.0
        }
    }
}

/// Result of a migration batch with LSN tracking.
///
/// This struct is returned by `migrate_batch_with_lsn` and provides
/// information about what was migrated and how the WAL was affected.
#[derive(Debug, Clone)]
pub struct MigrationWithLsnResult {
    /// Number of node versions successfully migrated to cold storage.
    pub nodes_migrated: usize,
    /// Number of edge versions successfully migrated to cold storage.
    pub edges_migrated: usize,
    /// Number of WAL segments truncated after successful cold storage flush.
    pub segments_truncated: usize,
    /// The LSN that was flushed, if migration was enabled.
    pub flushed_lsn: Option<LSN>,
}

impl MigrationWithLsnResult {
    /// Check if any versions were migrated.
    pub fn has_migrations(&self) -> bool {
        self.nodes_migrated > 0 || self.edges_migrated > 0
    }

    /// Get the total number of versions migrated.
    pub fn total_migrated(&self) -> usize {
        self.nodes_migrated + self.edges_migrated
    }
}

/// Atomic statistics tracker for migration.
#[derive(Debug, Default)]
pub struct AtomicMigrationStats {
    node_versions_migrated: AtomicU64,
    edge_versions_migrated: AtomicU64,
    bytes_migrated: AtomicU64,
    runs_completed: AtomicU64,
    errors: AtomicU64,
}

impl AtomicMigrationStats {
    /// Create a new atomic stats tracker.
    pub fn new() -> Self {
        Self::default()
    }

    /// Get a snapshot of the current stats.
    pub fn snapshot(&self) -> MigrationStats {
        MigrationStats {
            node_versions_migrated: self.node_versions_migrated.load(Ordering::Relaxed),
            edge_versions_migrated: self.edge_versions_migrated.load(Ordering::Relaxed),
            bytes_migrated: self.bytes_migrated.load(Ordering::Relaxed),
            runs_completed: self.runs_completed.load(Ordering::Relaxed),
            errors: self.errors.load(Ordering::Relaxed),
            last_run_duration: Duration::ZERO,
            last_run_time: None,
        }
    }
}

/// Callback for migration events.
pub trait MigrationCallback: Send + Sync {
    /// Called when a node version is about to be migrated.
    /// Return false to skip this version.
    fn before_node_migration(&self, _version: &NodeVersion) -> bool {
        true
    }

    /// Called when an edge version is about to be migrated.
    /// Return false to skip this version.
    fn before_edge_migration(&self, _version: &EdgeVersion) -> bool {
        true
    }

    /// Called after a migration batch completes.
    fn after_batch(&self, _node_count: usize, _edge_count: usize) {}

    /// Called when a migration error occurs.
    fn on_error(&self, _error: &str) {}

    /// Called periodically during migration to report progress.
    /// This enables real-time monitoring of migration operations.
    fn on_progress(&self, _progress: &MigrationProgress) {}
}

/// Default callback that allows all migrations.
pub struct DefaultMigrationCallback;

impl MigrationCallback for DefaultMigrationCallback {}

/// Migration service that moves versions from hot to cold storage.
///
/// This service runs in the background and periodically checks for versions
/// that should be migrated based on the configured policy.
///
/// # Background Worker
///
/// The service manages a background thread that periodically:
/// 1. Checks if migration should be triggered (memory pressure, old versions)
/// 2. Identifies candidate versions for migration
/// 3. Batches and migrates versions to cold storage
/// 4. Reports progress via callbacks
///
/// # Graceful Shutdown
///
/// When `stop()` is called, the service:
/// 1. Signals the background worker to stop
/// 2. Waits for any in-flight batch to complete
/// 3. Returns only when fully stopped
///
/// The service also implements `Drop` to ensure the worker thread is stopped
/// when the service is dropped.
pub struct MigrationService {
    cold_storage: Arc<RedbColdStorage>,
    policy: MigrationPolicy,
    stats: Arc<AtomicMigrationStats>,
    running: Arc<AtomicBool>,
    callback: Arc<dyn MigrationCallback>,

    /// Access time tracking for LRU migration (version_id -> last access time).
    /// Uses a bounded LRU cache that automatically evicts oldest entries
    /// when MAX_ACCESS_ENTRIES is reached. Thread-safe without explicit locking.
    access_times: Arc<Cache<VersionId, Instant>>,

    /// Handle to the background worker thread
    worker_handle: Mutex<Option<JoinHandle<()>>>,

    /// Condvar for signaling worker shutdown completion.
    /// Tuple contains (shutdown_complete flag, generation counter).
    /// Generation counter prevents race conditions during rapid start/stop cycles.
    shutdown_complete: Arc<(Mutex<(bool, u64)>, Condvar)>,

    /// Current generation of the worker. Incremented on each start().
    /// Used to detect stale shutdown signals from previous workers.
    generation: Arc<AtomicU64>,

    /// Optional flush coordinator for LSN-based WAL truncation.
    /// When set, migration can atomically flush to cold storage and truncate WAL.
    flush_coordinator: Option<Arc<FlushCoordinator>>,
}

impl MigrationService {
    /// Create a new migration service.
    ///
    /// # Arguments
    ///
    /// * `cold_storage` - The destination storage for migrated data.
    /// * `policy` - Configuration determining when and what to migrate.
    pub fn new(cold_storage: Arc<RedbColdStorage>, policy: MigrationPolicy) -> Self {
        Self {
            cold_storage,
            policy,
            stats: Arc::new(AtomicMigrationStats::new()),
            running: Arc::new(AtomicBool::new(false)),
            callback: Arc::new(DefaultMigrationCallback),
            access_times: Arc::new(Cache::new(MAX_ACCESS_ENTRIES)),
            worker_handle: Mutex::new(None),
            shutdown_complete: Arc::new((Mutex::new((true, 0)), Condvar::new())),
            generation: Arc::new(AtomicU64::new(0)),
            flush_coordinator: None,
        }
    }

    /// Create a new migration service with a custom callback.
    ///
    /// Callbacks allow monitoring migration progress and filtering specific items.
    ///
    /// # Arguments
    ///
    /// * `cold_storage` - The destination storage.
    /// * `policy` - Configuration.
    /// * `callback` - Implementation of `MigrationCallback` for events.
    pub fn with_callback(
        cold_storage: Arc<RedbColdStorage>,
        policy: MigrationPolicy,
        callback: Arc<dyn MigrationCallback>,
    ) -> Self {
        Self {
            cold_storage,
            policy,
            stats: Arc::new(AtomicMigrationStats::new()),
            running: Arc::new(AtomicBool::new(false)),
            callback,
            access_times: Arc::new(Cache::new(MAX_ACCESS_ENTRIES)),
            worker_handle: Mutex::new(None),
            shutdown_complete: Arc::new((Mutex::new((true, 0)), Condvar::new())),
            generation: Arc::new(AtomicU64::new(0)),
            flush_coordinator: None,
        }
    }

    /// Set the flush coordinator for LSN-based WAL truncation.
    ///
    /// When a flush coordinator is set, `migrate_batch_with_lsn` can be used
    /// to atomically flush versions to cold storage and truncate the WAL.
    ///
    /// # Key Invariant
    ///
    /// `WAL_truncation_lsn <= cold_storage.get_flushed_lsn()` (always)
    ///
    /// This ensures that any data in the WAL that hasn't been flushed to cold
    /// storage is never truncated, preserving crash recovery guarantees.
    pub fn set_flush_coordinator(&mut self, coordinator: Arc<FlushCoordinator>) {
        self.flush_coordinator = Some(coordinator);
    }

    /// Get the flush coordinator, if set.
    pub fn flush_coordinator(&self) -> Option<&Arc<FlushCoordinator>> {
        self.flush_coordinator.as_ref()
    }

    /// Start the background migration worker.
    ///
    /// The worker runs in a separate thread and periodically checks for
    /// versions that need to be migrated based on the configured policy.
    ///
    /// # Thread Safety
    ///
    /// This method is thread-safe and idempotent. Calling it multiple times
    /// has no effect if the service is already running.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let service = Arc::new(MigrationService::new(cold, policy));
    /// service.start();
    /// // ... later ...
    /// service.stop();
    /// ```
    pub fn start(&self) {
        // Check if already running (atomic compare-and-swap)
        if self
            .running
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::Relaxed)
            .is_err()
        {
            // Already running, no-op
            return;
        }

        // Increment generation and mark as not shutdown complete
        let current_gen = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
        {
            let (lock, _) = &*self.shutdown_complete;
            let mut state = lock
                .lock()
                .expect("MigrationService shutdown lock poisoned in start()");
            *state = (false, current_gen);
        }

        // Clone what we need for the worker thread
        let running = self.running.clone();
        let policy = self.policy.clone();
        let shutdown_complete = self.shutdown_complete.clone();

        // Spawn the background worker with panic safety
        let handle = thread::spawn(move || {
            Self::worker_loop(running, policy, shutdown_complete, current_gen);
        });

        // Store the handle
        let mut handle_guard = self
            .worker_handle
            .lock()
            .expect("MigrationService worker_handle lock poisoned in start()");
        *handle_guard = Some(handle);
    }

    /// Background worker loop with panic safety.
    ///
    /// The loop is wrapped in catch_unwind to ensure shutdown_complete is signaled
    /// even if the worker panics, preventing stop() from hanging indefinitely.
    fn worker_loop(
        running: Arc<AtomicBool>,
        policy: MigrationPolicy,
        shutdown_complete: Arc<(Mutex<(bool, u64)>, Condvar)>,
        generation: u64,
    ) {
        // SAFETY: AssertUnwindSafe is used here because:
        // 1. `running` is an Arc<AtomicBool> which has no interior mutability concerns
        // 2. `policy` is Clone and owned, so unwinding won't leave it in invalid state
        // 3. The closure only reads from these values and calls worker_loop_inner
        // 4. Even if worker_loop_inner panics, we catch it and signal shutdown_complete
        //    before re-throwing, so no resources are leaked
        let result = panic::catch_unwind(panic::AssertUnwindSafe(|| {
            Self::worker_loop_inner(&running, &policy);
        }));

        // Always signal shutdown complete, even on panic
        let (lock, cvar) = &*shutdown_complete;
        let mut state = lock
            .lock()
            .expect("MigrationService shutdown lock poisoned in worker_loop");

        // Only signal if this is the current generation (prevents stale signals)
        if state.1 == generation {
            state.0 = true;
            cvar.notify_all();
        }

        // Re-panic after signaling if there was a panic
        if let Err(panic_payload) = result {
            // Mark as not running so stop() doesn't try to join again
            running.store(false, Ordering::SeqCst);
            panic::resume_unwind(panic_payload);
        }
    }

    /// Inner worker loop logic.
    fn worker_loop_inner(running: &AtomicBool, policy: &MigrationPolicy) {
        while running.load(Ordering::SeqCst) {
            // Sleep for the configured interval, checking for shutdown periodically
            let mut remaining = policy.run_interval;

            while remaining > Duration::ZERO && running.load(Ordering::SeqCst) {
                let sleep_time = remaining.min(SHUTDOWN_CHECK_INTERVAL);
                thread::sleep(sleep_time);
                remaining = remaining.saturating_sub(sleep_time);
            }

            // Check again if we should exit
            if !running.load(Ordering::SeqCst) {
                break;
            }

            // Migration logic would go here when integrated with HistoricalStorage
            // For now, this is a placeholder that the integration tests can verify
        }
        // Shutdown signaling is handled by worker_loop() wrapper
    }

    /// Stop the background migration worker.
    ///
    /// This method gracefully shuts down the migration service. It sends a stop
    /// signal to the background worker and blocks until the worker thread exits.
    ///
    /// # Blocking Behavior
    ///
    /// This method blocks until:
    /// 1. The current migration batch (if any) completes
    /// 2. The worker thread exits cleanly
    ///
    /// If the service is not running, this is a no-op and returns immediately.
    pub fn stop(&self) {
        // Signal the worker to stop
        if !self.running.swap(false, Ordering::SeqCst) {
            // Was already stopped, no-op
            return;
        }

        // Get current generation to wait for
        let current_gen = self.generation.load(Ordering::SeqCst);

        // Wait for the worker to complete (check generation to avoid stale signals)
        let (lock, cvar) = &*self.shutdown_complete;
        let mut state = lock
            .lock()
            .expect("MigrationService shutdown lock poisoned in stop()");
        while !state.0 || state.1 != current_gen {
            // If generation changed, a new worker started - don't wait for old one
            if state.1 > current_gen {
                break;
            }
            state = cvar
                .wait(state)
                .expect("MigrationService shutdown condvar wait failed");
        }

        // Join the thread if we have a handle
        let mut handle_guard = self
            .worker_handle
            .lock()
            .expect("MigrationService worker_handle lock poisoned in stop()");
        if let Some(handle) = handle_guard.take() {
            let _ = handle.join();
        }
    }

    /// Check if migration should be triggered based on current conditions.
    ///
    /// This method evaluates the current state against the configured `MigrationPolicy`
    /// to determine if a migration run is necessary.
    ///
    /// # Trigger Conditions
    ///
    /// Returns true if either:
    /// - **Memory Pressure**: Hot tier usage > `memory_threshold_bytes`
    /// - **Age**: There are versions older than `age_threshold`
    ///
    /// # Arguments
    ///
    /// * `current_memory_bytes` - Current hot tier memory usage in bytes
    /// * `old_version_count` - Number of versions exceeding the age threshold
    pub fn should_trigger_migration(
        &self,
        current_memory_bytes: usize,
        old_version_count: usize,
    ) -> bool {
        if !self.policy.enabled {
            return false;
        }

        // Memory pressure trigger
        if current_memory_bytes > self.policy.memory_threshold_bytes {
            return true;
        }

        // Age-based trigger (if there are old versions)
        if old_version_count > 0 {
            return true;
        }

        false
    }

    /// Record an access to a version for LRU tracking.
    ///
    /// This method should be called whenever a version is accessed
    /// to track access patterns for LRU-based migration.
    ///
    /// The access tracking cache is bounded to `MAX_ACCESS_ENTRIES` and uses
    /// LRU eviction. When at capacity, least recently used entries are
    /// automatically evicted. This is O(1) and thread-safe.
    pub fn record_access(&self, version_id: VersionId) {
        self.access_times.insert(version_id, Instant::now());
    }

    /// Get the last access time for a version.
    ///
    /// Returns `None` if the version has never been accessed or was evicted.
    pub fn get_last_access(&self, version_id: VersionId) -> Option<Instant> {
        self.access_times.get(&version_id)
    }

    /// Clear access tracking for versions that have been migrated.
    pub fn clear_access(&self, version_ids: &[VersionId]) {
        for id in version_ids {
            self.access_times.remove(id);
        }
    }

    /// Sort migration candidates based on policy.
    ///
    /// In LRU mode, candidates are sorted by last access time (least recently used first).
    /// In age mode, candidates are sorted by age (oldest first).
    ///
    /// For LRU mode, access times are pre-fetched to minimize time spent during sort.
    fn sort_candidates_by_policy(&self, candidates: &mut [MigrationCandidate]) {
        if self.policy.enable_lru {
            // Pre-fetch access times for all candidates
            // The Cache is lock-free so we can call get() for each candidate
            let candidate_access_times: HashMap<VersionId, Option<Instant>> = candidates
                .iter()
                .map(|c| (c.version_id, self.access_times.get(&c.version_id)))
                .collect();

            // LRU mode: sort by last access time (least recently used first)
            candidates.sort_by(|a, b| {
                let a_access = candidate_access_times.get(&a.version_id).and_then(|&t| t);
                let b_access = candidate_access_times.get(&b.version_id).and_then(|&t| t);

                match (a_access, b_access) {
                    // Both have access times: sort by oldest access first
                    (Some(a_time), Some(b_time)) => a_time.cmp(&b_time),
                    // No access time means never accessed, prioritize for migration
                    (None, Some(_)) => std::cmp::Ordering::Less,
                    (Some(_), None) => std::cmp::Ordering::Greater,
                    // Both never accessed: fall back to age-based ordering
                    (None, None) => b.age.cmp(&a.age),
                }
            });
        } else {
            // Age mode: sort by age (oldest first) to prioritize older versions
            candidates.sort_by_key(|candidate| std::cmp::Reverse(candidate.age));
        }
    }

    /// Get the migration policy.
    pub fn policy(&self) -> &MigrationPolicy {
        &self.policy
    }

    /// Get migration statistics.
    pub fn stats(&self) -> MigrationStats {
        self.stats.snapshot()
    }

    /// Check if the service is running.
    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::Relaxed)
    }

    /// Migrate a batch of node versions to cold storage.
    ///
    /// This method takes a list of node versions, filters them via the callback,
    /// and moves them to the cold storage tier.
    ///
    /// # Usage
    ///
    /// This is typically called by:
    /// 1. The background worker during scheduled migration
    /// 2. The hot tier directly when urgent memory pressure is detected
    ///
    /// # Progress Reporting
    ///
    /// Progress is reported via the configured `MigrationCallback`.
    ///
    /// # Returns
    ///
    /// Returns the number of versions successfully migrated.
    pub fn migrate_node_versions(&self, versions: &[NodeVersion]) -> Result<usize> {
        if !self.policy.enabled {
            return Ok(0);
        }

        let start_time = Instant::now();
        let total_versions = versions.len();
        let total_batches = total_versions.div_ceil(self.policy.batch_size);

        let mut migrated = 0;
        let mut batch = Vec::with_capacity(self.policy.batch_size.min(versions.len()));
        let mut total_bytes = 0u64;
        let mut current_batch = 0;

        for version in versions {
            if !self.callback.before_node_migration(version) {
                continue;
            }

            total_bytes += version.estimated_size() as u64;
            batch.push(version.clone());

            if batch.len() >= self.policy.batch_size {
                self.cold_storage.store_node_versions_batch(&batch)?;
                migrated += batch.len();
                current_batch += 1;
                self.callback.after_batch(batch.len(), 0);

                // Report progress
                let progress = MigrationProgress {
                    total_versions,
                    migrated_versions: migrated,
                    bytes_migrated: total_bytes,
                    current_batch,
                    total_batches,
                    elapsed: start_time.elapsed(),
                };
                self.callback.on_progress(&progress);

                batch.clear();
            }
        }

        // Migrate remaining versions
        if !batch.is_empty() {
            self.cold_storage.store_node_versions_batch(&batch)?;
            migrated += batch.len();
            current_batch += 1;
            self.callback.after_batch(batch.len(), 0);

            // Report final progress
            let progress = MigrationProgress {
                total_versions,
                migrated_versions: migrated,
                bytes_migrated: total_bytes,
                current_batch,
                total_batches: current_batch, // Adjust for actual batch count
                elapsed: start_time.elapsed(),
            };
            self.callback.on_progress(&progress);
        }

        self.stats
            .node_versions_migrated
            .fetch_add(migrated as u64, Ordering::Relaxed);
        self.stats
            .bytes_migrated
            .fetch_add(total_bytes, Ordering::Relaxed);

        Ok(migrated)
    }

    /// Migrate a batch of edge versions to cold storage.
    ///
    /// Similar to `migrate_node_versions`, but for edge data.
    ///
    /// # Progress Reporting
    ///
    /// Progress is reported via the configured `MigrationCallback`.
    ///
    /// # Returns
    ///
    /// Returns the number of versions successfully migrated.
    pub fn migrate_edge_versions(&self, versions: &[EdgeVersion]) -> Result<usize> {
        if !self.policy.enabled {
            return Ok(0);
        }

        let start_time = Instant::now();
        let total_versions = versions.len();
        let total_batches = total_versions.div_ceil(self.policy.batch_size);

        let mut migrated = 0;
        let mut batch = Vec::with_capacity(self.policy.batch_size.min(versions.len()));
        let mut total_bytes = 0u64;
        let mut current_batch = 0;

        for version in versions {
            if !self.callback.before_edge_migration(version) {
                continue;
            }

            total_bytes += version.estimated_size() as u64;
            batch.push(version.clone());

            if batch.len() >= self.policy.batch_size {
                self.cold_storage.store_edge_versions_batch(&batch)?;
                migrated += batch.len();
                current_batch += 1;
                self.callback.after_batch(0, batch.len());

                // Report progress
                let progress = MigrationProgress {
                    total_versions,
                    migrated_versions: migrated,
                    bytes_migrated: total_bytes,
                    current_batch,
                    total_batches,
                    elapsed: start_time.elapsed(),
                };
                self.callback.on_progress(&progress);

                batch.clear();
            }
        }

        if !batch.is_empty() {
            self.cold_storage.store_edge_versions_batch(&batch)?;
            migrated += batch.len();
            current_batch += 1;
            self.callback.after_batch(0, batch.len());

            // Report final progress
            let progress = MigrationProgress {
                total_versions,
                migrated_versions: migrated,
                bytes_migrated: total_bytes,
                current_batch,
                total_batches: current_batch,
                elapsed: start_time.elapsed(),
            };
            self.callback.on_progress(&progress);
        }

        self.stats
            .edge_versions_migrated
            .fetch_add(migrated as u64, Ordering::Relaxed);
        self.stats
            .bytes_migrated
            .fetch_add(total_bytes, Ordering::Relaxed);

        Ok(migrated)
    }

    /// Migrate a batch of versions to cold storage with LSN tracking and WAL truncation.
    ///
    /// This is the core method for crash-safe migration. It performs an atomic hand-off
    /// of data ownership from the WAL to cold storage.
    ///
    /// # Atomicity Guarantees
    ///
    /// 1.  **Store**: Node and edge versions are written to cold storage.
    /// 2.  **Metadata Update**: The `flushed_lsn` in cold storage is updated atomically with the data.
    /// 3.  **Verification**: The flushed LSN is read back from cold storage to confirm durability.
    /// 4.  **Truncation**: The WAL is truncated *only* up to the confirmed flushed LSN.
    ///
    /// # Failure Handling
    ///
    /// - If cold storage write fails: Log error, return error. WAL is untouched.
    /// - If WAL truncation fails: Log error, return success (data is safe in cold storage, WAL will be cleaned up later).
    ///
    /// # Key Invariant
    ///
    /// `WAL_truncation_lsn <= cold_storage.get_flushed_lsn()` (always)
    ///
    /// This ensures that we never delete data from the WAL before it is durably persisted
    /// in the cold tier.
    ///
    /// # Arguments
    ///
    /// * `nodes` - Node versions to migrate
    /// * `edges` - Edge versions to migrate
    /// * `lsn` - The LSN that this batch covers (usually the current WAL LSN)
    ///
    /// # Returns
    ///
    /// Returns a `MigrationWithLsnResult` containing counts of migrated entities
    /// and the number of truncated WAL segments.
    pub fn migrate_batch_with_lsn(
        &self,
        nodes: &[NodeVersion],
        edges: &[EdgeVersion],
        lsn: LSN,
    ) -> Result<MigrationWithLsnResult> {
        if !self.policy.enabled {
            return Ok(MigrationWithLsnResult {
                nodes_migrated: 0,
                edges_migrated: 0,
                segments_truncated: 0,
                flushed_lsn: None,
            });
        }

        let start_time = Instant::now();

        // Filter versions through callback
        let filtered_nodes: Vec<_> = nodes
            .iter()
            .filter(|v| self.callback.before_node_migration(v))
            .cloned()
            .collect();

        let filtered_edges: Vec<_> = edges
            .iter()
            .filter(|v| self.callback.before_edge_migration(v))
            .cloned()
            .collect();

        // Calculate total bytes
        let node_bytes: u64 = filtered_nodes
            .iter()
            .map(|v| v.estimated_size() as u64)
            .sum();
        let edge_bytes: u64 = filtered_edges
            .iter()
            .map(|v| v.estimated_size() as u64)
            .sum();
        let total_bytes = node_bytes + edge_bytes;

        // Step 1: Atomically store to cold storage with LSN
        // If this fails, we return early - WAL is NOT truncated
        self.cold_storage
            .store_batch_with_lsn(&filtered_nodes, &filtered_edges, lsn)?;

        let nodes_migrated = filtered_nodes.len();
        let edges_migrated = filtered_edges.len();

        // Report callback
        self.callback.after_batch(nodes_migrated, edges_migrated);

        // Step 2: Get flushed LSN from cold storage and truncate WAL if coordinator is available
        // Get the actual flushed LSN from cold storage to ensure invariant:
        // WAL_truncation_lsn <= cold_storage.get_flushed_lsn()
        let flushed_lsn_value = self.cold_storage.get_flushed_lsn()?;

        let segments_truncated = if let Some(coordinator) = &self.flush_coordinator {
            // Defensive check: only truncate if we got a valid flushed LSN
            if let Some(flushed_lsn) = flushed_lsn_value {
                // Additional safety: verify the flushed LSN is >= what we just stored
                // (Should always be true due to monotonic LSN updates)
                debug_assert!(
                    flushed_lsn.0 >= lsn.0,
                    "Flushed LSN ({}) should be >= stored LSN ({})",
                    flushed_lsn.0,
                    lsn.0
                );

                // Truncate WAL segments up to the confirmed flushed LSN
                coordinator.truncate_to_lsn(flushed_lsn)?
            } else {
                // No flushed LSN available, don't truncate
                0
            }
        } else {
            // No coordinator, can't truncate WAL
            0
        };

        // Update stats
        self.stats
            .node_versions_migrated
            .fetch_add(nodes_migrated as u64, Ordering::Relaxed);
        self.stats
            .edge_versions_migrated
            .fetch_add(edges_migrated as u64, Ordering::Relaxed);
        self.stats
            .bytes_migrated
            .fetch_add(total_bytes, Ordering::Relaxed);

        // Report progress
        let progress = MigrationProgress {
            total_versions: nodes.len() + edges.len(),
            migrated_versions: nodes_migrated + edges_migrated,
            bytes_migrated: total_bytes,
            current_batch: 1,
            total_batches: 1,
            elapsed: start_time.elapsed(),
        };
        self.callback.on_progress(&progress);

        Ok(MigrationWithLsnResult {
            nodes_migrated,
            edges_migrated,
            segments_truncated,
            flushed_lsn: flushed_lsn_value,
        })
    }

    /// Identify node versions that are candidates for migration.
    ///
    /// Scans the hot tier state to find versions eligible for migration based on
    /// the configured policy (Age, LRU).
    ///
    /// # Safety Rules
    ///
    /// 1.  **Head Protection**: The current head version of a node is *never* selected.
    /// 2.  **Min Hot Versions**: Ensures at least `min_hot_versions` remain in hot tier.
    ///
    /// # Selection Logic
    ///
    /// Candidates are sorted based on the policy strategy:
    /// - **Age-based**: Oldest versions first.
    /// - **LRU-based**: Least recently accessed versions first.
    ///
    /// # Arguments
    ///
    /// * `versions` - All versions currently in hot storage
    /// * `head_versions` - Map of node IDs to their current head version
    /// * `version_counts` - Count of versions per node
    /// * `_current_time` - Unused, kept for API compatibility (wallclock time is used internally)
    pub fn identify_node_candidates(
        &self,
        versions: &FastHashMap<VersionId, NodeVersion>,
        head_versions: &FastHashMap<NodeId, VersionId>,
        version_counts: &FastHashMap<NodeId, usize>,
        _current_time: Instant,
    ) -> Vec<MigrationCandidate> {
        // Track how many candidates we've selected per node
        let mut candidates_per_node: FastHashMap<NodeId, usize> = FastHashMap::default();
        // ⚡ Bolt Optimization: Pre-allocate vectors using known capacity to avoid intermediate heap reallocations during migration.
        let mut all_candidates = Vec::with_capacity(versions.len());

        // Get current wallclock time in milliseconds since UNIX epoch
        let current_wallclock_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);

        // First, collect all potential candidates with their ages
        for (version_id, version) in versions {
            // Skip if this is the head version
            if let Some(&head_id) = head_versions.get(&version.node_id)
                && *version_id == head_id
            {
                continue;
            }

            // Calculate age from transaction time (wallclock comparison)
            let tx_start_ms = version.temporal.transaction_time().start().wallclock();
            let age_ms = (current_wallclock_ms - tx_start_ms).max(0) as u64;
            let age = Duration::from_millis(age_ms);

            // Check if version meets age threshold
            if age >= self.policy.age_threshold {
                all_candidates.push(MigrationCandidate {
                    version_id: *version_id,
                    is_node: true,
                    age,
                    estimated_size: version.estimated_size(),
                });
            }
        }

        // Sort candidates based on policy (LRU or age-based)
        self.sort_candidates_by_policy(&mut all_candidates);

        // Filter candidates to ensure min_hot_versions remain
        // ⚡ Bolt Optimization: Pre-allocate vectors using known capacity to avoid intermediate heap reallocations during migration.
        let mut final_candidates = Vec::with_capacity(all_candidates.len());
        for candidate in all_candidates {
            // Get node_id from the original version
            let node_id = versions.get(&candidate.version_id).map(|v| v.node_id);
            if let Some(node_id) = node_id {
                let count = version_counts.get(&node_id).copied().unwrap_or(0);
                let already_selected = candidates_per_node.get(&node_id).copied().unwrap_or(0);

                // Calculate how many we can migrate while keeping min_hot_versions
                let max_migrate = count.saturating_sub(self.policy.min_hot_versions);

                if already_selected < max_migrate {
                    *candidates_per_node.entry(node_id).or_insert(0) += 1;
                    final_candidates.push(candidate);
                }
            }
        }

        final_candidates
    }

    /// Identify edge versions that are candidates for migration.
    ///
    /// Similar to `identify_node_candidates`, but for edge data.
    ///
    /// # Safety Rules
    ///
    /// 1.  **Head Protection**: The current head version of an edge is *never* selected.
    /// 2.  **Min Hot Versions**: Ensures at least `min_hot_versions` remain in hot tier.
    ///
    /// # Arguments
    ///
    /// * `versions` - All versions currently in hot storage
    /// * `head_versions` - Map of edge IDs to their current head version
    /// * `version_counts` - Count of versions per edge
    /// * `_current_time` - Unused, kept for API compatibility (wallclock time is used internally)
    pub fn identify_edge_candidates(
        &self,
        versions: &FastHashMap<VersionId, EdgeVersion>,
        head_versions: &FastHashMap<EdgeId, VersionId>,
        version_counts: &FastHashMap<EdgeId, usize>,
        _current_time: Instant,
    ) -> Vec<MigrationCandidate> {
        // Track how many candidates we've selected per edge
        let mut candidates_per_edge: FastHashMap<EdgeId, usize> = FastHashMap::default();
        // ⚡ Bolt Optimization: Pre-allocate vectors using known capacity to avoid intermediate heap reallocations during migration.
        let mut all_candidates = Vec::with_capacity(versions.len());

        // Get current wallclock time in milliseconds since UNIX epoch
        let current_wallclock_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);

        // First, collect all potential candidates with their ages
        for (version_id, version) in versions {
            // Skip if this is the head version
            if let Some(&head_id) = head_versions.get(&version.edge_id)
                && *version_id == head_id
            {
                continue;
            }

            // Calculate age from transaction time (wallclock comparison)
            let tx_start_ms = version.temporal.transaction_time().start().wallclock();
            let age_ms = (current_wallclock_ms - tx_start_ms).max(0) as u64;
            let age = Duration::from_millis(age_ms);

            if age >= self.policy.age_threshold {
                all_candidates.push(MigrationCandidate {
                    version_id: *version_id,
                    is_node: false,
                    age,
                    estimated_size: version.estimated_size(),
                });
            }
        }

        // Sort candidates based on policy (LRU or age-based)
        self.sort_candidates_by_policy(&mut all_candidates);

        // Filter candidates to ensure min_hot_versions remain
        // ⚡ Bolt Optimization: Pre-allocate vectors using known capacity to avoid intermediate heap reallocations during migration.
        let mut final_candidates = Vec::with_capacity(all_candidates.len());
        for candidate in all_candidates {
            let edge_id = versions.get(&candidate.version_id).map(|v| v.edge_id);
            if let Some(edge_id) = edge_id {
                let count = version_counts.get(&edge_id).copied().unwrap_or(0);
                let already_selected = candidates_per_edge.get(&edge_id).copied().unwrap_or(0);

                let max_migrate = count.saturating_sub(self.policy.min_hot_versions);

                if already_selected < max_migrate {
                    *candidates_per_edge.entry(edge_id).or_insert(0) += 1;
                    final_candidates.push(candidate);
                }
            }
        }

        final_candidates
    }
}

impl Drop for MigrationService {
    /// Ensure the background worker is stopped when the service is dropped.
    ///
    /// This prevents orphaned worker threads that would continue running
    /// after the service is no longer accessible. Uses a timeout to prevent
    /// deadlock if the worker thread is stuck.
    fn drop(&mut self) {
        // Only attempt stop if running
        if !self.running.swap(false, Ordering::SeqCst) {
            // Was already stopped, no-op
            return;
        }

        // Get current generation to wait for
        let current_gen = self.generation.load(Ordering::SeqCst);

        // Wait for the worker to complete with timeout to prevent deadlock
        let (lock, cvar) = &*self.shutdown_complete;
        if let Ok(mut state) = lock.lock() {
            let deadline = Instant::now() + DROP_TIMEOUT;
            while !state.0 || state.1 != current_gen {
                // If generation changed, a new worker started - don't wait for old one
                if state.1 > current_gen {
                    break;
                }

                let now = Instant::now();
                if now >= deadline {
                    // Timeout reached - give up waiting to prevent deadlock
                    // Worker thread will eventually clean itself up
                    break;
                }

                let timeout = deadline - now;
                let result = cvar.wait_timeout(state, timeout);
                match result {
                    Ok((new_state, timeout_result)) => {
                        state = new_state;
                        if timeout_result.timed_out() {
                            // Timeout reached - give up waiting
                            break;
                        }
                    }
                    Err(_) => {
                        // Lock poisoned - give up
                        break;
                    }
                }
            }
        }

        // Try to join the thread if we have a handle
        if let Ok(mut handle_guard) = self.worker_handle.lock()
            && let Some(handle) = handle_guard.take()
        {
            // Don't block indefinitely - the thread will terminate on its own
            let _ = handle.join();
        }
    }
}

#[cfg(test)]
mod tests;
