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
use crate::storage::cold_storage::ColdStorage;
use crate::storage::version::{EdgeVersion, NodeVersion};
use crate::utils::error::Result;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[cfg(feature = "config-toml")]
use serde::{Deserialize, Serialize};

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
}

/// Default callback that allows all migrations.
pub struct DefaultMigrationCallback;

impl MigrationCallback for DefaultMigrationCallback {}

/// Migration service that moves versions from hot to cold storage.
///
/// This service runs in the background and periodically checks for versions
/// that should be migrated based on the configured policy.
pub struct MigrationService {
    cold_storage: Arc<dyn ColdStorage>,
    policy: MigrationPolicy,
    stats: Arc<AtomicMigrationStats>,
    running: Arc<AtomicBool>,
    callback: Arc<dyn MigrationCallback>,
}

impl MigrationService {
    /// Create a new migration service.
    pub fn new(cold_storage: Arc<dyn ColdStorage>, policy: MigrationPolicy) -> Self {
        Self {
            cold_storage,
            policy,
            stats: Arc::new(AtomicMigrationStats::new()),
            running: Arc::new(AtomicBool::new(false)),
            callback: Arc::new(DefaultMigrationCallback),
        }
    }

    /// Create a new migration service with a custom callback.
    pub fn with_callback(
        cold_storage: Arc<dyn ColdStorage>,
        policy: MigrationPolicy,
        callback: Arc<dyn MigrationCallback>,
    ) -> Self {
        Self {
            cold_storage,
            policy,
            stats: Arc::new(AtomicMigrationStats::new()),
            running: Arc::new(AtomicBool::new(false)),
            callback,
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
    pub fn migrate_node_versions(&self, versions: &[NodeVersion]) -> Result<usize> {
        if !self.policy.enabled {
            return Ok(0);
        }

        let mut migrated = 0;
        let mut batch = Vec::with_capacity(self.policy.batch_size.min(versions.len()));
        let mut total_bytes = 0usize;

        for version in versions {
            if !self.callback.before_node_migration(version) {
                continue;
            }

            total_bytes += version.estimated_size();
            batch.push(version.clone());

            if batch.len() >= self.policy.batch_size {
                self.cold_storage.store_node_versions_batch(&batch)?;
                migrated += batch.len();
                self.callback.after_batch(batch.len(), 0);
                batch.clear();
            }
        }

        // Migrate remaining versions
        if !batch.is_empty() {
            self.cold_storage.store_node_versions_batch(&batch)?;
            migrated += batch.len();
            self.callback.after_batch(batch.len(), 0);
        }

        self.stats
            .node_versions_migrated
            .fetch_add(migrated as u64, Ordering::Relaxed);
        self.stats
            .bytes_migrated
            .fetch_add(total_bytes as u64, Ordering::Relaxed);

        Ok(migrated)
    }

    /// Migrate a batch of edge versions to cold storage.
    pub fn migrate_edge_versions(&self, versions: &[EdgeVersion]) -> Result<usize> {
        if !self.policy.enabled {
            return Ok(0);
        }

        let mut migrated = 0;
        let mut batch = Vec::with_capacity(self.policy.batch_size.min(versions.len()));
        let mut total_bytes = 0usize;

        for version in versions {
            if !self.callback.before_edge_migration(version) {
                continue;
            }

            total_bytes += version.estimated_size();
            batch.push(version.clone());

            if batch.len() >= self.policy.batch_size {
                self.cold_storage.store_edge_versions_batch(&batch)?;
                migrated += batch.len();
                self.callback.after_batch(0, batch.len());
                batch.clear();
            }
        }

        if !batch.is_empty() {
            self.cold_storage.store_edge_versions_batch(&batch)?;
            migrated += batch.len();
            self.callback.after_batch(0, batch.len());
        }

        self.stats
            .edge_versions_migrated
            .fetch_add(migrated as u64, Ordering::Relaxed);
        self.stats
            .bytes_migrated
            .fetch_add(total_bytes as u64, Ordering::Relaxed);

        Ok(migrated)
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

        // Sort by age (oldest first) to prioritize older versions
        all_candidates.sort_by(|a, b| b.age.cmp(&a.age));

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

        // Sort by age (oldest first) to prioritize older versions
        all_candidates.sort_by(|a, b| b.age.cmp(&a.age));

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::id::NodeId;
    use crate::core::interning::GLOBAL_INTERNER;
    use crate::core::property::PropertyMapBuilder;
    use crate::core::temporal::BiTemporalInterval;
    use crate::storage::cold_storage::InMemoryColdStorage;

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
        let cold = Arc::new(InMemoryColdStorage::default_config());
        let policy = MigrationPolicy::default();
        let service = MigrationService::new(cold, policy);

        assert!(!service.is_running());
        assert_eq!(service.stats().node_versions_migrated, 0);
    }

    #[test]
    fn test_migrate_node_versions() {
        let cold = Arc::new(InMemoryColdStorage::default_config());
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
        let cold = Arc::new(InMemoryColdStorage::default_config());
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
        let cold = Arc::new(InMemoryColdStorage::default_config());
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
        let cold = Arc::new(InMemoryColdStorage::default_config());
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
        let cold = Arc::new(InMemoryColdStorage::default_config());
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
        let cold = Arc::new(InMemoryColdStorage::default_config());
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
}
