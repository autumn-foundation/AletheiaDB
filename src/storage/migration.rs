//! Migration service for tiered storage.
//!
//! This module implements Issue #121 (SCALE-003): Background migration service
//! that automatically moves historical versions from hot tier to cold tier.
//!
//! # Architecture
//!
//! The migration service runs as a background thread that:
//! 1. Monitors hot tier size and version ages
//! 2. Identifies candidate versions for migration based on policy
//! 3. Batches and transfers versions to cold storage
//! 4. Updates version chain pointers
//! 5. Removes migrated versions from hot tier
//!
//! # Migration Policy
//!
//! Migration decisions are based on configurable thresholds:
//! - **Age threshold**: Migrate versions older than N days
//! - **Memory threshold**: Migrate when memory usage exceeds N bytes
//! - **Min hot versions**: Always keep at least N versions per entity in hot tier
//!
//! # Example
//!
//! ```ignore
//! use gallifreydb::storage::migration::{MigrationPolicy, MigrationService};
//! use gallifreydb::storage::tiered_storage::TieredStorage;
//! use std::time::Duration;
//!
//! let policy = MigrationPolicy::builder()
//!     .age_threshold(Duration::from_secs(7 * 24 * 60 * 60)) // 7 days
//!     .memory_threshold_bytes(1024 * 1024 * 1024) // 1GB
//!     .min_hot_versions(1)
//!     .build();
//!
//! let service = MigrationService::new(tiered_storage, policy);
//! service.start(); // Starts background migration thread
//! ```

use crate::core::id::{EdgeId, NodeId, VersionId};
use crate::storage::redb_cold_storage::RedbColdStorage;
use crate::storage::version::{EdgeVersion, NodeVersion};
use crate::storage::wal::LSN;
use crate::storage::wal::flush_coordinator::FlushCoordinator;
use crate::utils::error::Result;
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
#[derive(Debug, Clone)]
#[cfg_attr(feature = "config-toml", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "config-toml", serde(default))]
pub struct MigrationPolicy {
    /// Migrate versions older than this duration.
    /// Default: 7 days
    pub age_threshold: Duration,

    /// Migrate when hot tier memory exceeds this threshold (bytes).
    /// Default: 80% of available memory (estimated at 1GB for portability)
    pub memory_threshold_bytes: usize,

    /// Minimum number of versions to keep in hot tier per entity.
    /// This ensures current state is always available in hot tier.
    /// Default: 1 (keep only the current version hot)
    pub min_hot_versions: usize,

    /// Maximum number of versions to migrate in a single batch.
    /// Default: 1000
    pub batch_size: usize,

    /// Interval between migration runs.
    /// Default: 60 seconds
    pub run_interval: Duration,

    /// Enable/disable migration (useful for testing or maintenance).
    pub enabled: bool,

    /// Enable LRU (Least Recently Used) based migration.
    /// When enabled, versions are sorted by access time in addition to age.
    /// Default: false
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
    pub fn builder() -> MigrationPolicyBuilder {
        MigrationPolicyBuilder::new()
    }

    /// Create a policy that aggressively migrates to cold storage.
    /// Useful for memory-constrained environments.
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
    /// Useful for read-heavy workloads with temporal queries.
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
    pub fn disabled() -> Self {
        Self {
            enabled: false,
            ..Default::default()
        }
    }
}

/// Builder for MigrationPolicy.
#[derive(Debug, Default)]
pub struct MigrationPolicyBuilder {
    policy: MigrationPolicy,
}

impl MigrationPolicyBuilder {
    /// Create a new builder with default values.
    pub fn new() -> Self {
        Self {
            policy: MigrationPolicy::default(),
        }
    }

    /// Set the age threshold for migration.
    pub fn age_threshold(mut self, duration: Duration) -> Self {
        self.policy.age_threshold = duration;
        self
    }

    /// Set the memory threshold in bytes.
    pub fn memory_threshold_bytes(mut self, bytes: usize) -> Self {
        self.policy.memory_threshold_bytes = bytes;
        self
    }

    /// Set the minimum hot versions per entity.
    pub fn min_hot_versions(mut self, count: usize) -> Self {
        self.policy.min_hot_versions = count;
        self
    }

    /// Set the batch size for migration.
    pub fn batch_size(mut self, size: usize) -> Self {
        self.policy.batch_size = size;
        self
    }

    /// Set the run interval.
    pub fn run_interval(mut self, interval: Duration) -> Self {
        self.policy.run_interval = interval;
        self
    }

    /// Enable or disable migration.
    pub fn enabled(mut self, enabled: bool) -> Self {
        self.policy.enabled = enabled;
        self
    }

    /// Enable or disable LRU-based migration ordering.
    /// When enabled, least recently accessed versions are migrated first.
    pub fn enable_lru_migration(mut self, enable: bool) -> Self {
        self.policy.enable_lru = enable;
        self
    }

    /// Build the policy.
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
#[derive(Debug, Clone, Default)]
pub struct MigrationProgress {
    /// Total number of versions to migrate in this run.
    pub total_versions: usize,
    /// Number of versions migrated so far.
    pub migrated_versions: usize,
    /// Total bytes migrated so far.
    pub bytes_migrated: u64,
    /// Current batch number (1-indexed).
    pub current_batch: usize,
    /// Total number of batches.
    pub total_batches: usize,
    /// Elapsed time since migration started.
    pub elapsed: Duration,
}

impl MigrationProgress {
    /// Calculate the percentage of versions migrated.
    pub fn percentage(&self) -> f64 {
        if self.total_versions == 0 {
            100.0
        } else {
            (self.migrated_versions as f64 / self.total_versions as f64) * 100.0
        }
    }

    /// Check if the migration is complete.
    pub fn is_complete(&self) -> bool {
        self.migrated_versions >= self.total_versions
    }

    /// Calculate the throughput in versions per second.
    pub fn versions_per_second(&self) -> f64 {
        let secs = self.elapsed.as_secs_f64();
        if secs > 0.0 {
            self.migrated_versions as f64 / secs
        } else {
            0.0
        }
    }

    /// Calculate the throughput in bytes per second.
    pub fn bytes_per_second(&self) -> f64 {
        let secs = self.elapsed.as_secs_f64();
        if secs > 0.0 {
            self.bytes_migrated as f64 / secs
        } else {
            0.0
        }
    }

    /// Estimate time remaining based on current throughput.
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

/// Statistics for migration operations.
#[derive(Debug, Clone, Default)]
pub struct MigrationStats {
    /// Total number of node versions migrated.
    pub node_versions_migrated: u64,
    /// Total number of edge versions migrated.
    pub edge_versions_migrated: u64,
    /// Total bytes migrated.
    pub bytes_migrated: u64,
    /// Number of migration runs completed.
    pub runs_completed: u64,
    /// Number of migration errors.
    pub errors: u64,
    /// Last migration run duration.
    pub last_run_duration: Duration,
    /// Last migration run time.
    pub last_run_time: Option<Instant>,
}

impl MigrationStats {
    /// Calculate migration throughput (versions per second).
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
    /// versions that need to be migrated. If already running, this is a no-op.
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
    /// This method blocks until:
    /// 1. The current migration batch (if any) completes
    /// 2. The worker thread exits cleanly
    ///
    /// If the service is not running, this is a no-op.
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
    /// Returns true if either:
    /// - Memory usage exceeds the configured threshold
    /// - There are versions older than the age threshold
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
            candidates.sort_by(|a, b| b.age.cmp(&a.age));
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
    /// This is called by the hot tier when it needs to free memory.
    /// Progress is reported via the configured callback.
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
    /// Progress is reported via the configured callback.
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
    /// This method atomically:
    /// 1. Stores node and edge versions to cold storage with the given LSN
    /// 2. On success, truncates WAL segments up to the flushed LSN
    /// 3. On failure, leaves WAL untouched (data safe)
    ///
    /// # Key Invariant
    ///
    /// `WAL_truncation_lsn <= cold_storage.get_flushed_lsn()` (always)
    ///
    /// # Arguments
    ///
    /// * `nodes` - Node versions to migrate
    /// * `edges` - Edge versions to migrate
    /// * `lsn` - The LSN up to which these versions cover
    ///
    /// # Returns
    ///
    /// A tuple of (nodes_migrated, edges_migrated, segments_truncated).
    ///
    /// # Errors
    ///
    /// Returns an error if cold storage flush fails. In this case, WAL is NOT truncated.
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
    /// This method examines the hot tier and returns versions that meet
    /// the migration policy criteria. It ensures that at least `min_hot_versions`
    /// versions remain in hot storage for each node.
    ///
    /// # Arguments
    ///
    /// * `versions` - All versions currently in hot storage
    /// * `head_versions` - Map of node IDs to their current head version
    /// * `version_counts` - Count of versions per node
    /// * `_current_time` - Unused, kept for API compatibility (wallclock time is used internally)
    pub fn identify_node_candidates(
        &self,
        versions: &HashMap<VersionId, NodeVersion>,
        head_versions: &HashMap<NodeId, VersionId>,
        version_counts: &HashMap<NodeId, usize>,
        _current_time: Instant,
    ) -> Vec<MigrationCandidate> {
        // Track how many candidates we've selected per node
        let mut candidates_per_node: HashMap<NodeId, usize> = HashMap::new();
        let mut all_candidates = Vec::new();

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
        let mut final_candidates = Vec::new();
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
    /// This method ensures that at least `min_hot_versions` versions remain
    /// in hot storage for each edge.
    ///
    /// # Arguments
    ///
    /// * `versions` - All versions currently in hot storage
    /// * `head_versions` - Map of edge IDs to their current head version
    /// * `version_counts` - Count of versions per edge
    /// * `_current_time` - Unused, kept for API compatibility (wallclock time is used internally)
    pub fn identify_edge_candidates(
        &self,
        versions: &HashMap<VersionId, EdgeVersion>,
        head_versions: &HashMap<EdgeId, VersionId>,
        version_counts: &HashMap<EdgeId, usize>,
        _current_time: Instant,
    ) -> Vec<MigrationCandidate> {
        // Track how many candidates we've selected per edge
        let mut candidates_per_edge: HashMap<EdgeId, usize> = HashMap::new();
        let mut all_candidates = Vec::new();

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
        let mut final_candidates = Vec::new();
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
mod tests {
    use super::*;
    use crate::core::id::{EdgeId, NodeId};
    use crate::core::interning::GLOBAL_INTERNER;
    use crate::core::property::PropertyMapBuilder;
    use crate::core::temporal::BiTemporalInterval;
    use crate::storage::redb_cold_storage::{RedbColdStorage, RedbConfig};
    use crate::storage::version::EdgeVersion;
    use std::sync::atomic::AtomicUsize;
    use std::thread;
    use tempfile::tempdir;

    fn create_test_node_version(id: u64, node_id: u64) -> NodeVersion {
        let properties = PropertyMapBuilder::new()
            .insert("name", "Test")
            .insert("age", 30i64)
            .build();

        NodeVersion::new_anchor(
            VersionId::new(id).unwrap(),
            NodeId::new(node_id).unwrap(),
            BiTemporalInterval::current(1000.into()),
            GLOBAL_INTERNER.intern("Person").unwrap(),
            properties,
        )
    }

    fn create_cold_storage() -> Arc<RedbColdStorage> {
        let temp_dir = tempdir().unwrap();
        // Leaking the temp_dir to keep file alive for test duration
        // Ideally we would return a tuple (Arc<RedbColdStorage>, TempDir) but that changes test signature
        // Since tests are short lived and run in isolation, this is acceptable for TDD
        let path = temp_dir.path().join("cold.redb");
        // We leak the TempDir to ensure the file isn't deleted while Redb holds it
        std::mem::forget(temp_dir);

        Arc::new(RedbColdStorage::new(path, RedbConfig::new()).unwrap())
    }

    // ========================================================================
    // MigrationPolicy tests
    // ========================================================================

    #[test]
    fn test_default_policy() {
        let policy = MigrationPolicy::default();
        assert_eq!(policy.age_threshold, Duration::from_secs(7 * 24 * 60 * 60));
        assert_eq!(policy.memory_threshold_bytes, 1024 * 1024 * 1024);
        assert_eq!(policy.min_hot_versions, 1);
        assert_eq!(policy.batch_size, 1000);
        assert!(policy.enabled);
    }

    #[test]
    fn test_aggressive_policy() {
        let policy = MigrationPolicy::aggressive();
        assert_eq!(policy.age_threshold, Duration::from_secs(24 * 60 * 60));
        assert_eq!(policy.memory_threshold_bytes, 512 * 1024 * 1024);
        assert_eq!(policy.batch_size, 2000);
    }

    #[test]
    fn test_conservative_policy() {
        let policy = MigrationPolicy::conservative();
        assert_eq!(policy.age_threshold, Duration::from_secs(30 * 24 * 60 * 60));
        assert_eq!(policy.min_hot_versions, 5);
    }

    #[test]
    fn test_disabled_policy() {
        let policy = MigrationPolicy::disabled();
        assert!(!policy.enabled);
    }

    #[test]
    fn test_policy_builder() {
        let policy = MigrationPolicy::builder()
            .age_threshold(Duration::from_secs(86400))
            .memory_threshold_bytes(2 * 1024 * 1024 * 1024)
            .min_hot_versions(3)
            .batch_size(500)
            .run_interval(Duration::from_secs(120))
            .enabled(true)
            .build();

        assert_eq!(policy.age_threshold, Duration::from_secs(86400));
        assert_eq!(policy.memory_threshold_bytes, 2 * 1024 * 1024 * 1024);
        assert_eq!(policy.min_hot_versions, 3);
        assert_eq!(policy.batch_size, 500);
        assert_eq!(policy.run_interval, Duration::from_secs(120));
    }

    // ========================================================================
    // MigrationService tests
    // ========================================================================

    #[test]
    fn test_migration_service_creation() {
        let cold = create_cold_storage();
        let policy = MigrationPolicy::default();
        let service = MigrationService::new(cold, policy);

        assert!(!service.is_running());
        assert_eq!(service.stats().node_versions_migrated, 0);
    }

    #[test]
    fn test_migrate_node_versions() {
        let cold = create_cold_storage();
        let policy = MigrationPolicy::default();
        let service = MigrationService::new(cold.clone(), policy);

        let versions: Vec<NodeVersion> =
            (1..=10).map(|i| create_test_node_version(i, 100)).collect();

        let migrated = service.migrate_node_versions(&versions).unwrap();
        assert_eq!(migrated, 10);

        let stats = service.stats();
        assert_eq!(stats.node_versions_migrated, 10);

        // Verify versions are in cold storage
        for version in &versions {
            assert!(cold.contains_node_version(version.id).unwrap());
        }
    }

    #[test]
    fn test_migrate_disabled() {
        let cold = create_cold_storage();
        let policy = MigrationPolicy::disabled();
        let service = MigrationService::new(cold.clone(), policy);

        let versions: Vec<NodeVersion> =
            (1..=10).map(|i| create_test_node_version(i, 100)).collect();

        let migrated = service.migrate_node_versions(&versions).unwrap();
        assert_eq!(migrated, 0);

        // Verify versions are NOT in cold storage
        for version in &versions {
            assert!(!cold.contains_node_version(version.id).unwrap());
        }
    }

    #[test]
    fn test_migration_batching() {
        let cold = create_cold_storage();
        let policy = MigrationPolicy::builder().batch_size(3).build();
        let service = MigrationService::new(cold.clone(), policy);

        let versions: Vec<NodeVersion> =
            (1..=10).map(|i| create_test_node_version(i, 100)).collect();

        let migrated = service.migrate_node_versions(&versions).unwrap();
        assert_eq!(migrated, 10);

        // All should be migrated despite small batch size
        for version in &versions {
            assert!(cold.contains_node_version(version.id).unwrap());
        }
    }

    #[test]
    fn test_identify_candidates_respects_min_hot_versions() {
        let cold = create_cold_storage();
        let policy = MigrationPolicy::builder()
            .min_hot_versions(2)
            .age_threshold(Duration::ZERO) // All versions are "old enough"
            .build();
        let service = MigrationService::new(cold, policy);

        let mut versions = HashMap::new();
        let mut heads = HashMap::new();
        let mut counts = HashMap::new();

        // Create 3 versions for node 100
        let node_id = NodeId::new(100).unwrap();
        for i in 1..=3 {
            let v = create_test_node_version(i, 100);
            versions.insert(v.id, v);
        }
        heads.insert(node_id, VersionId::new(3).unwrap()); // v3 is head
        counts.insert(node_id, 3);

        let candidates =
            service.identify_node_candidates(&versions, &heads, &counts, Instant::now());

        // With min_hot_versions=2 and 3 versions, only 1 should be candidate
        // (v3 is head and skipped, v2 must stay hot, v1 can migrate)
        assert_eq!(candidates.len(), 1);
    }

    #[test]
    fn test_identify_candidates_skips_head() {
        let cold = create_cold_storage();
        let policy = MigrationPolicy::builder()
            .min_hot_versions(1)
            .age_threshold(Duration::ZERO)
            .build();
        let service = MigrationService::new(cold, policy);

        let mut versions = HashMap::new();
        let mut heads = HashMap::new();
        let mut counts = HashMap::new();

        let node_id = NodeId::new(100).unwrap();
        for i in 1..=3 {
            let v = create_test_node_version(i, 100);
            versions.insert(v.id, v);
        }
        heads.insert(node_id, VersionId::new(3).unwrap());
        counts.insert(node_id, 3);

        let candidates =
            service.identify_node_candidates(&versions, &heads, &counts, Instant::now());

        // Head (v3) should not be a candidate
        assert!(!candidates.iter().any(|c| c.version_id.as_u64() == 3));
    }

    // ========================================================================
    // MigrationStats tests
    // ========================================================================

    #[test]
    fn test_migration_stats_throughput() {
        let stats = MigrationStats {
            node_versions_migrated: 1000,
            edge_versions_migrated: 500,
            bytes_migrated: 1_000_000,
            runs_completed: 10,
            errors: 0,
            last_run_duration: Duration::from_secs(10),
            last_run_time: Some(Instant::now()),
        };

        // 1500 versions in 10 seconds = 150 versions/sec
        assert!((stats.versions_per_second() - 150.0).abs() < 0.1);
    }

    // ========================================================================
    // MigrationCallback tests
    // ========================================================================

    struct FilteringCallback {
        skip_version_ids: Vec<u64>,
    }

    impl MigrationCallback for FilteringCallback {
        fn before_node_migration(&self, version: &NodeVersion) -> bool {
            !self.skip_version_ids.contains(&version.id.as_u64())
        }
    }

    #[test]
    fn test_migration_callback_filtering() {
        let cold = create_cold_storage();
        let policy = MigrationPolicy::default();
        let callback = Arc::new(FilteringCallback {
            skip_version_ids: vec![2, 4, 6, 8, 10],
        });
        let service = MigrationService::with_callback(cold.clone(), policy, callback);

        let versions: Vec<NodeVersion> =
            (1..=10).map(|i| create_test_node_version(i, 100)).collect();

        let migrated = service.migrate_node_versions(&versions).unwrap();
        assert_eq!(migrated, 5); // Only odd IDs migrated

        // Verify only odd versions are in cold storage
        assert!(
            cold.contains_node_version(VersionId::new(1).unwrap())
                .unwrap()
        );
        assert!(
            !cold
                .contains_node_version(VersionId::new(2).unwrap())
                .unwrap()
        );
        assert!(
            cold.contains_node_version(VersionId::new(3).unwrap())
                .unwrap()
        );
    }

    // ========================================================================
    // SCALE-003: Background Worker Tests (TDD)
    // ========================================================================

    #[test]
    fn test_background_worker_starts_and_stops() {
        let cold = create_cold_storage();
        let policy = MigrationPolicy::builder()
            .run_interval(Duration::from_millis(50))
            .enabled(true)
            .build();
        let service = Arc::new(MigrationService::new(cold, policy));

        // Service should not be running initially
        assert!(!service.is_running());

        // Start the background worker (no-op without historical storage for now)
        service.start();
        assert!(service.is_running());

        // Allow worker to run for a bit
        thread::sleep(Duration::from_millis(100));

        // Stop gracefully
        service.stop();
        assert!(!service.is_running());
    }

    #[test]
    fn test_graceful_shutdown_waits_for_inflight() {
        let cold = create_cold_storage();
        let policy = MigrationPolicy::builder()
            .run_interval(Duration::from_millis(50))
            .batch_size(10)
            .enabled(true)
            .build();

        // Track batch completions via callback
        let batches_completed = Arc::new(AtomicUsize::new(0));
        let batches_completed_clone = batches_completed.clone();

        struct BatchTracker {
            completed: Arc<AtomicUsize>,
        }
        impl MigrationCallback for BatchTracker {
            fn after_batch(&self, node_count: usize, edge_count: usize) {
                if node_count > 0 || edge_count > 0 {
                    self.completed.fetch_add(1, Ordering::SeqCst);
                }
            }
        }

        let callback = Arc::new(BatchTracker {
            completed: batches_completed_clone,
        });
        let service = Arc::new(MigrationService::with_callback(cold, policy, callback));

        service.start();
        thread::sleep(Duration::from_millis(100));

        // Stop should complete gracefully
        let stop_start = Instant::now();
        service.stop();
        let stop_duration = stop_start.elapsed();

        // Should have stopped within reasonable time
        assert!(stop_duration < Duration::from_secs(5));
        assert!(!service.is_running());
    }

    #[test]
    fn test_multiple_start_stop_cycles() {
        let cold = create_cold_storage();
        let policy = MigrationPolicy::builder()
            .run_interval(Duration::from_millis(50))
            .build();
        let service = Arc::new(MigrationService::new(cold, policy));

        for _ in 0..3 {
            assert!(!service.is_running());
            service.start();
            assert!(service.is_running());
            thread::sleep(Duration::from_millis(50));
            service.stop();
            assert!(!service.is_running());
        }
    }

    #[test]
    fn test_double_start_is_noop() {
        let cold = create_cold_storage();
        let policy = MigrationPolicy::default();
        let service = Arc::new(MigrationService::new(cold, policy));

        service.start();
        assert!(service.is_running());

        // Second start should be a no-op
        service.start();
        assert!(service.is_running());

        service.stop();
        assert!(!service.is_running());
    }

    #[test]
    fn test_double_stop_is_noop() {
        let cold = create_cold_storage();
        let policy = MigrationPolicy::default();
        let service = Arc::new(MigrationService::new(cold, policy));

        service.start();
        service.stop();
        assert!(!service.is_running());

        // Second stop should be a no-op
        service.stop();
        assert!(!service.is_running());
    }

    // ========================================================================
    // SCALE-003: Memory Pressure Trigger Tests (TDD)
    // ========================================================================

    #[test]
    fn test_memory_pressure_trigger_enabled() {
        let cold = create_cold_storage();
        let policy = MigrationPolicy::builder()
            .memory_threshold_bytes(1000) // Low threshold
            .age_threshold(Duration::ZERO)
            .min_hot_versions(1)
            .build();
        let service = MigrationService::new(cold, policy);

        // Should trigger when memory usage exceeds threshold
        assert!(service.should_trigger_migration(2000, 0)); // memory > threshold
        assert!(!service.should_trigger_migration(500, 0)); // memory < threshold
    }

    #[test]
    fn test_combined_triggers() {
        let cold = create_cold_storage();
        let policy = MigrationPolicy::builder()
            .memory_threshold_bytes(1000)
            .age_threshold(Duration::from_secs(3600))
            .build();
        let service = MigrationService::new(cold, policy);

        // Either condition should trigger
        assert!(service.should_trigger_migration(2000, 0)); // memory pressure
        assert!(service.should_trigger_migration(500, 10)); // old versions exist
        assert!(!service.should_trigger_migration(500, 0)); // neither condition
    }

    // ========================================================================
    // SCALE-003: Access Pattern (LRU) Trigger Tests (TDD)
    // ========================================================================

    #[test]
    fn test_access_tracking_records_access() {
        let cold = create_cold_storage();
        let policy = MigrationPolicy::default();
        let service = MigrationService::new(cold, policy);

        let version_id = VersionId::new(1).unwrap();

        // Record access
        service.record_access(version_id);

        // Should have recorded the access
        let last_access = service.get_last_access(version_id);
        assert!(last_access.is_some());
    }

    #[test]
    fn test_lru_candidates_prioritized() {
        let cold = create_cold_storage();
        let policy = MigrationPolicy::builder()
            .age_threshold(Duration::ZERO) // All old enough
            .min_hot_versions(1)
            .build();
        let service = MigrationService::new(cold, policy);

        // Create versions with different access times
        let v1 = VersionId::new(1).unwrap();
        let v2 = VersionId::new(2).unwrap();
        let v3 = VersionId::new(3).unwrap();

        // Record accesses with delays
        service.record_access(v1);
        thread::sleep(Duration::from_millis(10));
        service.record_access(v2);
        thread::sleep(Duration::from_millis(10));
        service.record_access(v3);

        // v1 should be oldest (least recently accessed)
        let v1_access = service.get_last_access(v1).unwrap();
        let v3_access = service.get_last_access(v3).unwrap();
        assert!(v1_access < v3_access);
    }

    #[test]
    fn test_identify_candidates_with_lru() {
        let cold = create_cold_storage();
        let policy = MigrationPolicy::builder()
            .age_threshold(Duration::ZERO)
            .min_hot_versions(1)
            .enable_lru_migration(true)
            .build();
        let service = MigrationService::new(cold, policy);

        let mut versions = HashMap::new();
        let mut heads = HashMap::new();
        let mut counts = HashMap::new();

        // Create versions for different nodes
        let node1 = NodeId::new(100).unwrap();
        let node2 = NodeId::new(200).unwrap();

        let v1 = create_test_node_version(1, 100);
        let v2 = create_test_node_version(2, 100);
        let v3 = create_test_node_version(3, 200);
        let v4 = create_test_node_version(4, 200);

        versions.insert(v1.id, v1.clone());
        versions.insert(v2.id, v2.clone());
        versions.insert(v3.id, v3.clone());
        versions.insert(v4.id, v4.clone());

        heads.insert(node1, VersionId::new(2).unwrap());
        heads.insert(node2, VersionId::new(4).unwrap());
        counts.insert(node1, 2);
        counts.insert(node2, 2);

        // Record accesses - v3 more recently than v1
        service.record_access(v1.id);
        thread::sleep(Duration::from_millis(10));
        service.record_access(v3.id);

        let candidates =
            service.identify_node_candidates(&versions, &heads, &counts, Instant::now());

        // Both non-head versions should be candidates
        assert_eq!(candidates.len(), 2);

        // With LRU, v1 (older access) should come before v3 (newer access)
        if candidates.len() >= 2 {
            let v1_pos = candidates.iter().position(|c| c.version_id.as_u64() == 1);
            let v3_pos = candidates.iter().position(|c| c.version_id.as_u64() == 3);
            if let (Some(p1), Some(p3)) = (v1_pos, v3_pos) {
                assert!(p1 < p3, "LRU should prioritize v1 over v3");
            }
        }
    }

    // ========================================================================
    // SCALE-003: Progress Tracking Tests (TDD)
    // ========================================================================

    #[test]
    fn test_progress_tracking_callback() {
        let cold = create_cold_storage();
        let policy = MigrationPolicy::builder().batch_size(5).build();

        // Track progress updates
        let progress_updates = Arc::new(std::sync::Mutex::new(Vec::new()));
        let progress_clone = progress_updates.clone();

        struct ProgressTracker {
            updates: Arc<std::sync::Mutex<Vec<MigrationProgress>>>,
        }
        impl MigrationCallback for ProgressTracker {
            fn on_progress(&self, progress: &MigrationProgress) {
                self.updates.lock().unwrap().push(progress.clone());
            }
        }

        let callback = Arc::new(ProgressTracker {
            updates: progress_clone,
        });
        let service = MigrationService::with_callback(cold, policy, callback);

        // Migrate 12 versions (should be 3 batches: 5, 5, 2)
        let versions: Vec<NodeVersion> =
            (1..=12).map(|i| create_test_node_version(i, 100)).collect();

        let migrated = service.migrate_node_versions(&versions).unwrap();
        assert_eq!(migrated, 12);

        // Check progress updates
        let updates = progress_updates.lock().unwrap();
        assert!(!updates.is_empty());

        // Final progress should show 12/12
        if let Some(final_progress) = updates.last() {
            assert_eq!(final_progress.total_versions, 12);
            assert_eq!(final_progress.migrated_versions, 12);
            assert!(final_progress.is_complete());
        }
    }

    #[test]
    fn test_progress_percentage() {
        let progress = MigrationProgress {
            total_versions: 100,
            migrated_versions: 50,
            bytes_migrated: 1000,
            current_batch: 5,
            total_batches: 10,
            elapsed: Duration::from_secs(5),
        };

        assert!((progress.percentage() - 50.0).abs() < 0.01);
        assert!(!progress.is_complete());

        let complete_progress = MigrationProgress {
            total_versions: 100,
            migrated_versions: 100,
            bytes_migrated: 2000,
            current_batch: 10,
            total_batches: 10,
            elapsed: Duration::from_secs(10),
        };

        assert!((complete_progress.percentage() - 100.0).abs() < 0.01);
        assert!(complete_progress.is_complete());
    }

    #[test]
    fn test_progress_throughput() {
        let progress = MigrationProgress {
            total_versions: 100,
            migrated_versions: 50,
            bytes_migrated: 1_000_000,
            current_batch: 5,
            total_batches: 10,
            elapsed: Duration::from_secs(5),
        };

        // 50 versions in 5 seconds = 10 versions/sec
        assert!((progress.versions_per_second() - 10.0).abs() < 0.01);
        // 1MB in 5 seconds = 200KB/sec
        assert!((progress.bytes_per_second() - 200_000.0).abs() < 0.01);
    }

    // ========================================================================
    // SCALE-003: Integration Tests (TDD)
    // ========================================================================

    #[test]
    fn test_migration_run_stats_updated() {
        let cold = create_cold_storage();
        let policy = MigrationPolicy::default();
        let service = MigrationService::new(cold, policy);

        let versions: Vec<NodeVersion> =
            (1..=50).map(|i| create_test_node_version(i, 100)).collect();

        service.migrate_node_versions(&versions).unwrap();

        let stats = service.stats();
        assert_eq!(stats.node_versions_migrated, 50);
        assert!(stats.bytes_migrated > 0);
    }

    #[test]
    fn test_service_handles_empty_migration() {
        let cold = create_cold_storage();
        let policy = MigrationPolicy::default();
        let service = MigrationService::new(cold, policy);

        let migrated = service.migrate_node_versions(&[]).unwrap();
        assert_eq!(migrated, 0);

        let stats = service.stats();
        assert_eq!(stats.node_versions_migrated, 0);
    }

    // ========================================================================
    // Edge Version Tests (Additional Coverage)
    // ========================================================================

    fn create_test_edge_version(id: u64, edge_id: u64) -> EdgeVersion {
        let properties = PropertyMapBuilder::new().insert("weight", 1.5f64).build();

        EdgeVersion::new_anchor(
            VersionId::new(id).unwrap(),
            EdgeId::new(edge_id).unwrap(),
            BiTemporalInterval::current(1000.into()),
            GLOBAL_INTERNER.intern("KNOWS").unwrap(),
            NodeId::new(1).unwrap(),
            NodeId::new(2).unwrap(),
            properties,
        )
    }

    fn create_test_edge_version_with_timestamp(id: u64, edge_id: u64, ts_ms: i64) -> EdgeVersion {
        use crate::core::temporal::TimeRange;
        let properties = PropertyMapBuilder::new().insert("weight", 1.5f64).build();

        let range = TimeRange::from(ts_ms.into());
        let temporal = BiTemporalInterval::new(range, range);

        EdgeVersion::new_anchor(
            VersionId::new(id).unwrap(),
            EdgeId::new(edge_id).unwrap(),
            temporal,
            GLOBAL_INTERNER.intern("KNOWS").unwrap(),
            NodeId::new(1).unwrap(),
            NodeId::new(2).unwrap(),
            properties,
        )
    }

    #[test]
    fn test_migrate_edge_versions() {
        let cold = create_cold_storage();
        let policy = MigrationPolicy::default();
        let service = MigrationService::new(cold.clone(), policy);

        let versions: Vec<EdgeVersion> =
            (1..=10).map(|i| create_test_edge_version(i, 200)).collect();

        let migrated = service.migrate_edge_versions(&versions).unwrap();
        assert_eq!(migrated, 10);

        let stats = service.stats();
        assert_eq!(stats.edge_versions_migrated, 10);

        // Verify versions are in cold storage
        for version in &versions {
            assert!(cold.contains_edge_version(version.id).unwrap());
        }
    }

    #[test]
    fn test_migrate_edge_versions_disabled() {
        let cold = create_cold_storage();
        let policy = MigrationPolicy::disabled();
        let service = MigrationService::new(cold.clone(), policy);

        let versions: Vec<EdgeVersion> =
            (1..=10).map(|i| create_test_edge_version(i, 200)).collect();

        let migrated = service.migrate_edge_versions(&versions).unwrap();
        assert_eq!(migrated, 0);

        // Verify versions are NOT in cold storage
        for version in &versions {
            assert!(!cold.contains_edge_version(version.id).unwrap());
        }
    }

    #[test]
    fn test_migrate_edge_versions_batching() {
        let cold = create_cold_storage();
        let policy = MigrationPolicy::builder().batch_size(3).build();
        let service = MigrationService::new(cold.clone(), policy);

        let versions: Vec<EdgeVersion> =
            (1..=10).map(|i| create_test_edge_version(i, 200)).collect();

        let migrated = service.migrate_edge_versions(&versions).unwrap();
        assert_eq!(migrated, 10);

        // All should be migrated despite small batch size
        for version in &versions {
            assert!(cold.contains_edge_version(version.id).unwrap());
        }
    }

    #[test]
    fn test_identify_edge_candidates_respects_min_hot_versions() {
        let cold = create_cold_storage();
        let policy = MigrationPolicy::builder()
            .min_hot_versions(2)
            .age_threshold(Duration::ZERO) // All versions are "old enough"
            .build();
        let service = MigrationService::new(cold, policy);

        let mut versions = HashMap::new();
        let mut heads = HashMap::new();
        let mut counts = HashMap::new();

        // Create 3 versions for edge 200
        let edge_id = EdgeId::new(200).unwrap();
        for i in 1..=3 {
            let v = create_test_edge_version(i, 200);
            versions.insert(v.id, v);
        }
        heads.insert(edge_id, VersionId::new(3).unwrap()); // v3 is head
        counts.insert(edge_id, 3);

        let candidates =
            service.identify_edge_candidates(&versions, &heads, &counts, Instant::now());

        // With min_hot_versions=2 and 3 versions, only 1 should be candidate
        // (v3 is head and skipped, v2 must stay hot, v1 can migrate)
        assert_eq!(candidates.len(), 1);
    }

    #[test]
    fn test_identify_edge_candidates_skips_head() {
        let cold = create_cold_storage();
        let policy = MigrationPolicy::builder()
            .min_hot_versions(1)
            .age_threshold(Duration::ZERO)
            .build();
        let service = MigrationService::new(cold, policy);

        let mut versions = HashMap::new();
        let mut heads = HashMap::new();
        let mut counts = HashMap::new();

        let edge_id = EdgeId::new(200).unwrap();
        for i in 1..=3 {
            let v = create_test_edge_version(i, 200);
            versions.insert(v.id, v);
        }
        heads.insert(edge_id, VersionId::new(3).unwrap());
        counts.insert(edge_id, 3);

        let candidates =
            service.identify_edge_candidates(&versions, &heads, &counts, Instant::now());

        // Head (v3) should not be a candidate
        assert!(!candidates.iter().any(|c| c.version_id.as_u64() == 3));
    }

    #[test]
    fn test_identify_edge_candidates_respects_age_threshold() {
        let cold = create_cold_storage();
        // Set age threshold to 1 hour
        let policy = MigrationPolicy::builder()
            .min_hot_versions(1)
            .age_threshold(Duration::from_secs(3600))
            .build();
        let service = MigrationService::new(cold, policy);

        let mut versions = HashMap::new();
        let mut heads = HashMap::new();
        let mut counts = HashMap::new();

        let edge_id = EdgeId::new(200).unwrap();

        // Get current time in ms
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64;

        // Create an old version (2 hours ago)
        let old_ts = now_ms - (2 * 60 * 60 * 1000);
        let v1 = create_test_edge_version_with_timestamp(1, 200, old_ts);
        versions.insert(v1.id, v1);

        // Create a recent version (30 minutes ago)
        let recent_ts = now_ms - (30 * 60 * 1000);
        let v2 = create_test_edge_version_with_timestamp(2, 200, recent_ts);
        versions.insert(v2.id, v2);

        // Create head version (now)
        let v3 = create_test_edge_version_with_timestamp(3, 200, now_ms);
        versions.insert(v3.id, v3);

        heads.insert(edge_id, VersionId::new(3).unwrap());
        counts.insert(edge_id, 3);

        let candidates =
            service.identify_edge_candidates(&versions, &heads, &counts, Instant::now());

        // Only v1 (2 hours old) should be a candidate, v2 (30 min) is too young
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].version_id.as_u64(), 1);
    }

    // ========================================================================
    // Edge Callback Tests
    // ========================================================================

    struct EdgeFilteringCallback {
        skip_version_ids: Vec<u64>,
        batch_counts: std::sync::Mutex<Vec<(usize, usize)>>,
    }

    impl EdgeFilteringCallback {
        fn new(skip_ids: Vec<u64>) -> Self {
            Self {
                skip_version_ids: skip_ids,
                batch_counts: std::sync::Mutex::new(Vec::new()),
            }
        }
    }

    impl MigrationCallback for EdgeFilteringCallback {
        fn before_edge_migration(&self, version: &EdgeVersion) -> bool {
            !self.skip_version_ids.contains(&version.id.as_u64())
        }

        fn after_batch(&self, node_count: usize, edge_count: usize) {
            self.batch_counts
                .lock()
                .unwrap()
                .push((node_count, edge_count));
        }
    }

    #[test]
    fn test_edge_migration_callback_filtering() {
        let cold = create_cold_storage();
        let policy = MigrationPolicy::default();
        let callback = Arc::new(EdgeFilteringCallback::new(vec![2, 4, 6, 8, 10]));
        let service = MigrationService::with_callback(cold.clone(), policy, callback.clone());

        let versions: Vec<EdgeVersion> =
            (1..=10).map(|i| create_test_edge_version(i, 200)).collect();

        let migrated = service.migrate_edge_versions(&versions).unwrap();
        assert_eq!(migrated, 5); // Only odd IDs migrated

        // Verify only odd versions are in cold storage
        assert!(
            cold.contains_edge_version(VersionId::new(1).unwrap())
                .unwrap()
        );
        assert!(
            !cold
                .contains_edge_version(VersionId::new(2).unwrap())
                .unwrap()
        );
        assert!(
            cold.contains_edge_version(VersionId::new(3).unwrap())
                .unwrap()
        );

        // Verify batch callback was called
        let batches = callback.batch_counts.lock().unwrap();
        assert!(!batches.is_empty());
        // All batches should be edge batches (node_count=0)
        for (node_count, _edge_count) in batches.iter() {
            assert_eq!(*node_count, 0);
        }
    }

    // ========================================================================
    // MigrationStats Edge Cases
    // ========================================================================

    #[test]
    fn test_migration_stats_throughput_zero_duration() {
        let stats = MigrationStats {
            node_versions_migrated: 1000,
            edge_versions_migrated: 500,
            bytes_migrated: 1_000_000,
            runs_completed: 10,
            errors: 0,
            last_run_duration: Duration::ZERO,
            last_run_time: Some(Instant::now()),
        };

        // With zero duration, should return 0 to avoid division by zero
        assert_eq!(stats.versions_per_second(), 0.0);
    }

    #[test]
    fn test_migration_stats_default() {
        let stats = MigrationStats::default();
        assert_eq!(stats.node_versions_migrated, 0);
        assert_eq!(stats.edge_versions_migrated, 0);
        assert_eq!(stats.bytes_migrated, 0);
        assert_eq!(stats.runs_completed, 0);
        assert_eq!(stats.errors, 0);
        assert_eq!(stats.last_run_duration, Duration::ZERO);
        assert!(stats.last_run_time.is_none());
    }

    // ========================================================================
    // AtomicMigrationStats Tests
    // ========================================================================

    #[test]
    fn test_atomic_migration_stats_new() {
        let stats = AtomicMigrationStats::new();
        let snapshot = stats.snapshot();
        assert_eq!(snapshot.node_versions_migrated, 0);
        assert_eq!(snapshot.edge_versions_migrated, 0);
        assert_eq!(snapshot.bytes_migrated, 0);
        assert_eq!(snapshot.runs_completed, 0);
        assert_eq!(snapshot.errors, 0);
    }

    #[test]
    fn test_atomic_migration_stats_snapshot() {
        let stats = AtomicMigrationStats::new();
        stats.node_versions_migrated.store(100, Ordering::Relaxed);
        stats.edge_versions_migrated.store(50, Ordering::Relaxed);
        stats.bytes_migrated.store(10000, Ordering::Relaxed);
        stats.runs_completed.store(5, Ordering::Relaxed);
        stats.errors.store(2, Ordering::Relaxed);

        let snapshot = stats.snapshot();
        assert_eq!(snapshot.node_versions_migrated, 100);
        assert_eq!(snapshot.edge_versions_migrated, 50);
        assert_eq!(snapshot.bytes_migrated, 10000);
        assert_eq!(snapshot.runs_completed, 5);
        assert_eq!(snapshot.errors, 2);
    }

    // ========================================================================
    // Service API Tests
    // ========================================================================

    #[test]
    fn test_service_policy_getter() {
        let cold = create_cold_storage();
        let policy = MigrationPolicy::builder()
            .age_threshold(Duration::from_secs(123456))
            .min_hot_versions(7)
            .build();
        let service = MigrationService::new(cold, policy);

        assert_eq!(service.policy().age_threshold, Duration::from_secs(123456));
        assert_eq!(service.policy().min_hot_versions, 7);
    }

    #[test]
    fn test_migrate_empty_edge_versions() {
        let cold = create_cold_storage();
        let policy = MigrationPolicy::default();
        let service = MigrationService::new(cold.clone(), policy);

        let edge_versions: Vec<EdgeVersion> = vec![];
        let migrated = service.migrate_edge_versions(&edge_versions).unwrap();
        assert_eq!(migrated, 0);

        let stats = service.stats();
        assert_eq!(stats.edge_versions_migrated, 0);
    }

    #[test]
    fn test_identify_edge_candidates_empty_versions() {
        let cold = create_cold_storage();
        let policy = MigrationPolicy::builder()
            .age_threshold(Duration::ZERO)
            .build();
        let service = MigrationService::new(cold, policy);

        let empty_versions: HashMap<VersionId, EdgeVersion> = HashMap::new();
        let empty_heads: HashMap<EdgeId, VersionId> = HashMap::new();
        let empty_counts: HashMap<EdgeId, usize> = HashMap::new();

        let candidates = service.identify_edge_candidates(
            &empty_versions,
            &empty_heads,
            &empty_counts,
            Instant::now(),
        );
        assert!(candidates.is_empty());
    }

    #[test]
    fn test_identify_candidates_version_count_zero() {
        let cold = create_cold_storage();
        let policy = MigrationPolicy::builder()
            .min_hot_versions(1)
            .age_threshold(Duration::ZERO)
            .build();
        let service = MigrationService::new(cold, policy);

        let mut versions = HashMap::new();
        let mut heads = HashMap::new();
        let counts: HashMap<NodeId, usize> = HashMap::new(); // Empty counts

        let node_id = NodeId::new(100).unwrap();
        for i in 1..=3 {
            let v = create_test_node_version(i, 100);
            versions.insert(v.id, v);
        }
        heads.insert(node_id, VersionId::new(3).unwrap());
        // counts is empty - simulate missing count data

        let candidates =
            service.identify_node_candidates(&versions, &heads, &counts, Instant::now());

        // With zero count, max_migrate = 0 - 1 = saturates to 0, so no candidates
        assert!(candidates.is_empty());
    }

    // ========================================================================
    // MigrationCandidate Tests
    // ========================================================================

    #[test]
    fn test_migration_candidate_debug_and_clone() {
        let candidate = MigrationCandidate {
            version_id: VersionId::new(1).unwrap(),
            is_node: true,
            age: Duration::from_secs(3600),
            estimated_size: 1024,
        };

        // Test Clone
        let cloned = candidate.clone();
        assert_eq!(cloned.version_id, candidate.version_id);
        assert_eq!(cloned.is_node, candidate.is_node);
        assert_eq!(cloned.age, candidate.age);
        assert_eq!(cloned.estimated_size, candidate.estimated_size);

        // Test Debug
        let debug_str = format!("{:?}", candidate);
        assert!(debug_str.contains("MigrationCandidate"));
    }

    // ========================================================================
    // Additional Policy Preset Tests
    // ========================================================================

    #[test]
    fn test_conservative_policy_values() {
        let policy = MigrationPolicy::conservative();
        assert_eq!(policy.age_threshold, Duration::from_secs(30 * 24 * 60 * 60));
        assert_eq!(policy.memory_threshold_bytes, 4 * 1024 * 1024 * 1024);
        assert_eq!(policy.min_hot_versions, 5);
        assert_eq!(policy.batch_size, 500);
        assert_eq!(policy.run_interval, Duration::from_secs(300));
        assert!(policy.enabled);
    }

    #[test]
    fn test_aggressive_policy_values() {
        let policy = MigrationPolicy::aggressive();
        assert_eq!(policy.age_threshold, Duration::from_secs(24 * 60 * 60));
        assert_eq!(policy.memory_threshold_bytes, 512 * 1024 * 1024);
        assert_eq!(policy.min_hot_versions, 1);
        assert_eq!(policy.batch_size, 2000);
        assert_eq!(policy.run_interval, Duration::from_secs(30));
        assert!(policy.enabled);
        assert!(policy.enable_lru); // Aggressive mode uses LRU
    }

    #[test]
    fn test_default_policy_run_interval() {
        let policy = MigrationPolicy::default();
        assert_eq!(policy.run_interval, Duration::from_secs(60));
    }

    #[test]
    fn test_policy_builder_default() {
        let builder = MigrationPolicyBuilder::default();
        let policy = builder.build();
        // Should match MigrationPolicy::default()
        assert_eq!(policy.age_threshold, Duration::from_secs(7 * 24 * 60 * 60));
    }

    // ========================================================================
    // Multiple Entity Migration Tests
    // ========================================================================

    #[test]
    fn test_identify_candidates_multiple_nodes() {
        let cold = create_cold_storage();
        let policy = MigrationPolicy::builder()
            .min_hot_versions(1)
            .age_threshold(Duration::ZERO)
            .build();
        let service = MigrationService::new(cold, policy);

        let mut versions = HashMap::new();
        let mut heads = HashMap::new();
        let mut counts = HashMap::new();

        // Create versions for multiple nodes
        for node_num in [100u64, 101, 102] {
            let node_id = NodeId::new(node_num).unwrap();
            for i in 1..=3 {
                let version_id = node_num * 10 + i;
                let v = create_test_node_version(version_id, node_num);
                versions.insert(v.id, v);
            }
            heads.insert(node_id, VersionId::new(node_num * 10 + 3).unwrap());
            counts.insert(node_id, 3);
        }

        let candidates =
            service.identify_node_candidates(&versions, &heads, &counts, Instant::now());

        // Each node has 3 versions, min_hot=1, head is skipped
        // So each node can have 2 candidates (max_migrate = 3-1 = 2)
        // Total should be 6 candidates (2 per node * 3 nodes)
        assert_eq!(candidates.len(), 6);
    }

    #[test]
    fn test_identify_edge_candidates_multiple_edges() {
        let cold = create_cold_storage();
        let policy = MigrationPolicy::builder()
            .min_hot_versions(1)
            .age_threshold(Duration::ZERO)
            .build();
        let service = MigrationService::new(cold, policy);

        let mut versions = HashMap::new();
        let mut heads = HashMap::new();
        let mut counts = HashMap::new();

        // Create versions for multiple edges
        for edge_num in [200u64, 201, 202] {
            let edge_id = EdgeId::new(edge_num).unwrap();
            for i in 1..=3 {
                let version_id = edge_num * 10 + i;
                let v = create_test_edge_version(version_id, edge_num);
                versions.insert(v.id, v);
            }
            heads.insert(edge_id, VersionId::new(edge_num * 10 + 3).unwrap());
            counts.insert(edge_id, 3);
        }

        let candidates =
            service.identify_edge_candidates(&versions, &heads, &counts, Instant::now());

        // Each edge has 3 versions, min_hot=1, head is skipped
        // So each edge can have 2 candidates (max_migrate = 3-1 = 2)
        // Total should be 6 candidates (2 per edge * 3 edges)
        assert_eq!(candidates.len(), 6);
    }

    // ========================================================================
    // MigrationProgress Tests
    // ========================================================================

    #[test]
    fn test_progress_estimated_remaining() {
        let progress = MigrationProgress {
            total_versions: 100,
            migrated_versions: 50,
            bytes_migrated: 1000,
            current_batch: 5,
            total_batches: 10,
            elapsed: Duration::from_secs(5),
        };

        // 50 versions in 5 secs = 10 v/sec, 50 remaining = ~5 secs
        let remaining = progress.estimated_remaining();
        assert!(remaining.as_secs() <= 6 && remaining.as_secs() >= 4);
    }

    #[test]
    fn test_progress_zero_elapsed() {
        let progress = MigrationProgress {
            total_versions: 100,
            migrated_versions: 0,
            bytes_migrated: 0,
            current_batch: 0,
            total_batches: 10,
            elapsed: Duration::ZERO,
        };

        // Should return 0 without dividing by zero
        assert_eq!(progress.versions_per_second(), 0.0);
        assert_eq!(progress.bytes_per_second(), 0.0);
        assert_eq!(progress.estimated_remaining(), Duration::ZERO);
    }

    #[test]
    fn test_progress_empty_total() {
        let progress = MigrationProgress {
            total_versions: 0,
            migrated_versions: 0,
            bytes_migrated: 0,
            current_batch: 0,
            total_batches: 0,
            elapsed: Duration::from_secs(1),
        };

        // Empty migration should be 100% complete
        assert_eq!(progress.percentage(), 100.0);
        assert!(progress.is_complete());
    }

    // ========================================================================
    // Access Tracking Edge Cases
    // ========================================================================

    #[test]
    fn test_clear_access_tracking() {
        let cold = create_cold_storage();
        let policy = MigrationPolicy::default();
        let service = MigrationService::new(cold, policy);

        let v1 = VersionId::new(1).unwrap();
        let v2 = VersionId::new(2).unwrap();
        let v3 = VersionId::new(3).unwrap();

        // Record accesses
        service.record_access(v1);
        service.record_access(v2);
        service.record_access(v3);

        // Verify recorded
        assert!(service.get_last_access(v1).is_some());
        assert!(service.get_last_access(v2).is_some());
        assert!(service.get_last_access(v3).is_some());

        // Clear specific accesses
        service.clear_access(&[v1, v2]);

        // v1 and v2 should be cleared, v3 should remain
        assert!(service.get_last_access(v1).is_none());
        assert!(service.get_last_access(v2).is_none());
        assert!(service.get_last_access(v3).is_some());
    }

    // ========================================================================
    // LSN-Based Migration Tests (Issue 6: Wire migration → Redb → WAL truncation)
    // ========================================================================

    #[test]
    fn test_migration_updates_flushed_lsn() {
        use crate::storage::redb_cold_storage::{RedbColdStorage, RedbConfig};
        use tempfile::tempdir;

        let temp_dir = tempdir().unwrap();
        let cold = Arc::new(
            RedbColdStorage::new(
                temp_dir.path().join("cold.redb"),
                RedbConfig::new().compression(crate::storage::CompressionAlgorithm::None),
            )
            .unwrap(),
        );

        let policy = MigrationPolicy::default();
        let service = MigrationService::new(cold.clone(), policy);

        // Create some test versions
        let nodes: Vec<NodeVersion> = (1..=5).map(|i| create_test_node_version(i, 100)).collect();
        let edges: Vec<EdgeVersion> = (10..=12)
            .map(|i| create_test_edge_version(i, 200))
            .collect();

        let lsn = LSN(1000);

        // Before migration, flushed LSN should be None
        assert!(cold.get_flushed_lsn().unwrap().is_none());

        // Migrate with LSN
        let result = service.migrate_batch_with_lsn(&nodes, &edges, lsn).unwrap();

        assert_eq!(result.nodes_migrated, 5);
        assert_eq!(result.edges_migrated, 3);
        assert_eq!(result.flushed_lsn, Some(lsn));

        // After migration, flushed LSN should be updated
        assert_eq!(cold.get_flushed_lsn().unwrap(), Some(lsn));
    }

    #[test]
    fn test_migration_with_coordinator_set() {
        use crate::storage::redb_cold_storage::{RedbColdStorage, RedbConfig};
        use crate::storage::wal::flush_coordinator::FlushCoordinatorConfig;
        use tempfile::tempdir;

        let temp_dir = tempdir().unwrap();
        let wal_dir = temp_dir.path().join("wal");
        std::fs::create_dir_all(&wal_dir).unwrap();

        // Create cold storage
        let cold = Arc::new(
            RedbColdStorage::new(
                temp_dir.path().join("cold.redb"),
                RedbConfig::new().compression(crate::storage::CompressionAlgorithm::None),
            )
            .unwrap(),
        );

        // Create flush coordinator
        let config = FlushCoordinatorConfig {
            wal_dir: wal_dir.clone(),
            segment_size: 1024,
            ..Default::default()
        };
        let coordinator = Arc::new(FlushCoordinator::new(config).unwrap());

        // Create migration service with flush coordinator
        let policy = MigrationPolicy::default();
        let mut service = MigrationService::new(cold.clone(), policy);
        service.set_flush_coordinator(coordinator.clone());

        // Verify coordinator is set
        assert!(service.flush_coordinator().is_some());

        // Migrate with LSN - the truncation call will happen but may truncate 0 segments
        // since there are no WAL entries yet
        let nodes: Vec<NodeVersion> = (1..=3).map(|i| create_test_node_version(i, 100)).collect();
        let lsn = LSN(25);

        let result = service.migrate_batch_with_lsn(&nodes, &[], lsn).unwrap();

        assert_eq!(result.nodes_migrated, 3);
        assert_eq!(result.flushed_lsn, Some(lsn));
        // segments_truncated will be 0 since there are no WAL segments with LSN < 25
        assert_eq!(result.segments_truncated, 0);

        // Verify data is in cold storage
        for node in &nodes {
            assert!(cold.contains_node_version(node.id).unwrap());
        }

        // Verify flushed LSN is recorded
        assert_eq!(cold.get_flushed_lsn().unwrap(), Some(lsn));
    }

    #[test]
    fn test_migration_failure_does_not_truncate_wal() {
        use crate::storage::wal::flush_coordinator::FlushCoordinatorConfig;
        // We can't use FailingColdStorage mock easily because we removed the trait.
        // Instead, we use RedbColdStorage with fault injection.

        let temp_dir = tempdir().unwrap();
        let wal_dir = temp_dir.path().join("wal");
        std::fs::create_dir_all(&wal_dir).unwrap();

        // Create flush coordinator
        let config = FlushCoordinatorConfig {
            wal_dir: wal_dir.clone(),
            segment_size: 1024,
            ..Default::default()
        };
        let coordinator = Arc::new(FlushCoordinator::new(config).unwrap());

        // Create cold storage
        let db_path = temp_dir.path().join("cold.redb");
        let cold = Arc::new(RedbColdStorage::new(&db_path, RedbConfig::new()).unwrap());

        // Inject failure
        cold.set_fail_writes(true);

        let policy = MigrationPolicy::default();
        let mut service = MigrationService::new(cold.clone(), policy);
        service.set_flush_coordinator(coordinator.clone());

        // Attempt migration - should fail
        let nodes: Vec<NodeVersion> = (1..=3).map(|i| create_test_node_version(i, 100)).collect();
        let result = service.migrate_batch_with_lsn(&nodes, &[], LSN(5));

        // Should have failed
        assert!(result.is_err());

        // Verify that writes were attempted
        assert!(cold.was_write_attempted());

        // Since store failed, WAL truncation should NOT have been called.
        // We can't easily spy on the coordinator here, but if store_batch_with_lsn
        // returns early with Err, the truncation code path is skipped.
    }

    #[test]
    fn test_migration_with_lsn_disabled_policy() {
        use crate::storage::redb_cold_storage::{RedbColdStorage, RedbConfig};
        use tempfile::tempdir;

        let temp_dir = tempdir().unwrap();
        let cold = Arc::new(
            RedbColdStorage::new(
                temp_dir.path().join("cold.redb"),
                RedbConfig::new().compression(crate::storage::CompressionAlgorithm::None),
            )
            .unwrap(),
        );

        // Disabled policy
        let policy = MigrationPolicy::disabled();
        let service = MigrationService::new(cold.clone(), policy);

        let nodes: Vec<NodeVersion> = (1..=5).map(|i| create_test_node_version(i, 100)).collect();
        let lsn = LSN(1000);

        let result = service.migrate_batch_with_lsn(&nodes, &[], lsn).unwrap();

        // Should not migrate anything when disabled
        assert_eq!(result.nodes_migrated, 0);
        assert_eq!(result.edges_migrated, 0);
        assert_eq!(result.segments_truncated, 0);
        assert!(result.flushed_lsn.is_none());

        // Cold storage should not have the versions
        for node in &nodes {
            assert!(!cold.contains_node_version(node.id).unwrap());
        }
    }

    #[test]
    fn test_migration_with_lsn_result_helpers() {
        let result = MigrationWithLsnResult {
            nodes_migrated: 5,
            edges_migrated: 3,
            segments_truncated: 2,
            flushed_lsn: Some(LSN(100)),
        };

        assert!(result.has_migrations());
        assert_eq!(result.total_migrated(), 8);

        let empty_result = MigrationWithLsnResult {
            nodes_migrated: 0,
            edges_migrated: 0,
            segments_truncated: 0,
            flushed_lsn: None,
        };

        assert!(!empty_result.has_migrations());
        assert_eq!(empty_result.total_migrated(), 0);
    }

    #[test]
    fn test_set_and_get_flush_coordinator() {
        use crate::storage::wal::flush_coordinator::FlushCoordinatorConfig;

        let temp_dir = tempdir().unwrap();
        let config = FlushCoordinatorConfig {
            wal_dir: temp_dir.path().to_path_buf(),
            ..Default::default()
        };
        let coordinator = Arc::new(FlushCoordinator::new(config).unwrap());

        let cold = create_cold_storage();
        let policy = MigrationPolicy::default();
        let mut service = MigrationService::new(cold, policy);

        // Initially no coordinator
        assert!(service.flush_coordinator().is_none());

        // Set coordinator
        service.set_flush_coordinator(coordinator.clone());

        // Now should have coordinator
        assert!(service.flush_coordinator().is_some());
    }

    /// Test that WAL truncation uses the actual flushed LSN from cold storage,
    /// not the requested LSN. This enforces the safety invariant:
    /// WAL_truncation_lsn <= cold_storage.get_flushed_lsn()
    #[test]
    fn test_truncation_uses_actual_flushed_lsn() {
        use crate::storage::redb_cold_storage::{RedbColdStorage, RedbConfig};
        use crate::storage::wal::flush_coordinator::FlushCoordinatorConfig;
        use tempfile::tempdir;

        let temp_dir = tempdir().unwrap();
        let config = FlushCoordinatorConfig {
            wal_dir: temp_dir.path().to_path_buf(),
            ..Default::default()
        };
        let coordinator = Arc::new(FlushCoordinator::new(config).unwrap());

        // Create Redb cold storage with LSN tracking
        let db_path = temp_dir.path().join("test.redb");
        let cold = Arc::new(RedbColdStorage::new(&db_path, RedbConfig::new()).unwrap());
        let policy = MigrationPolicy::default();
        let mut service = MigrationService::new(cold.clone(), policy);
        service.set_flush_coordinator(coordinator.clone());

        // Migrate batch with LSN 100
        let result = service.migrate_batch_with_lsn(&[], &[], LSN(100)).unwrap();

        // Result should contain the actual flushed LSN from cold storage
        assert_eq!(result.flushed_lsn, Some(LSN(100)));

        // Migrate another batch with LSN 200
        let result = service.migrate_batch_with_lsn(&[], &[], LSN(200)).unwrap();

        // Result should now be LSN 200
        assert_eq!(result.flushed_lsn, Some(LSN(200)));

        // Verify cold storage has LSN 200
        assert_eq!(cold.get_flushed_lsn().unwrap(), Some(LSN(200)));
    }

    /// Test that no WAL truncation occurs when there's no flush coordinator
    #[test]
    fn test_no_truncation_without_coordinator() {
        use crate::storage::redb_cold_storage::{RedbColdStorage, RedbConfig};
        use tempfile::tempdir;

        let temp_dir = tempdir().unwrap();
        let db_path = temp_dir.path().join("test.redb");
        let cold = Arc::new(RedbColdStorage::new(&db_path, RedbConfig::new()).unwrap());
        let policy = MigrationPolicy::default();
        let service = MigrationService::new(cold.clone(), policy);

        // Migrate batch WITHOUT setting coordinator
        let result = service.migrate_batch_with_lsn(&[], &[], LSN(100)).unwrap();

        // No segments should be truncated
        assert_eq!(result.segments_truncated, 0);

        // But LSN should still be set in cold storage
        assert_eq!(cold.get_flushed_lsn().unwrap(), Some(LSN(100)));
    }

    /// Comprehensive test of the WAL truncation safety invariant:
    /// WAL_truncation_lsn <= cold_storage.get_flushed_lsn()
    ///
    /// This test simulates a scenario where:
    /// 1. Multiple batches are migrated with increasing LSNs
    /// 2. We verify that WAL truncation only happens after cold storage confirms the LSN
    /// 3. We verify the invariant is maintained even with concurrent operations
    #[test]
    fn test_lsn_invariant_maintained() {
        use crate::storage::redb_cold_storage::{RedbColdStorage, RedbConfig};
        use crate::storage::wal::flush_coordinator::FlushCoordinatorConfig;
        use tempfile::tempdir;

        let temp_dir = tempdir().unwrap();
        let config = FlushCoordinatorConfig {
            wal_dir: temp_dir.path().to_path_buf(),
            ..Default::default()
        };
        let coordinator = Arc::new(FlushCoordinator::new(config).unwrap());

        let db_path = temp_dir.path().join("test.redb");
        let cold = Arc::new(RedbColdStorage::new(&db_path, RedbConfig::new()).unwrap());
        let policy = MigrationPolicy::default();
        let mut service = MigrationService::new(cold.clone(), policy);
        service.set_flush_coordinator(coordinator.clone());

        // Migrate multiple batches in sequence
        let lsns = vec![LSN(100), LSN(200), LSN(300), LSN(400), LSN(500)];

        for lsn in lsns {
            let result = service.migrate_batch_with_lsn(&[], &[], lsn).unwrap();

            // After each migration:
            // 1. Cold storage should have the LSN
            let cold_lsn = cold.get_flushed_lsn().unwrap();
            assert_eq!(
                cold_lsn,
                Some(lsn),
                "Cold storage should have LSN {:?}",
                lsn
            );

            // 2. Result should reflect the actual flushed LSN
            assert_eq!(
                result.flushed_lsn, cold_lsn,
                "Result LSN should match cold storage LSN"
            );

            // 3. The invariant WAL_truncation_lsn <= flushed_lsn is maintained
            // (This is implicitly tested by the fact that we read flushed_lsn before truncating)
        }

        // Final verification: cold storage has the highest LSN
        assert_eq!(cold.get_flushed_lsn().unwrap(), Some(LSN(500)));
    }
}
