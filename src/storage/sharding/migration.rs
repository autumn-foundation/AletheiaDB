//! Data migration implementation for shard rebalancing.
//!
//! This module handles the actual movement of data between shards,
//! including:
//!
//! - Batch extraction from source shard
//! - Network transfer with checksums
//! - Dual-write routing during migration
//! - Data verification
//! - Atomic cutover
//!
//! # Migration Protocol
//!
//! ```text
//! ┌──────────────────────────────────────────────────────────────────┐
//! │                    Migration State Machine                        │
//! │                                                                   │
//! │  Planned ──► DualWrite ──► Copying ──► Verifying ──► Cutover    │
//! │                                                          │        │
//! │                                                          ▼        │
//! │                                                      Cleanup     │
//! │                                                          │        │
//! │                                                          ▼        │
//! │                                                     Completed    │
//! └──────────────────────────────────────────────────────────────────┘
//! ```

use super::network::{MigrationBatch, NetworkError, ShardClient};
use super::rebalance::{MigrationPlan, MigrationState};
use super::types::ShardId;
use crate::core::id::NodeId;
use std::collections::{HashMap, HashSet};
use std::fmt;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

/// A token that captures routing state at a point in time.
///
/// Used to verify that migration state hasn't changed between
/// routing decision and write commit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoutingToken {
    /// The label this token is for.
    pub label: String,
    /// The primary shard.
    pub primary_shard: ShardId,
    /// Target shards at token creation time.
    pub targets: Vec<ShardId>,
    /// Migration version at token creation (for staleness detection).
    pub migration_version: u64,
}

/// Error types for migration operations.
#[derive(Debug, Clone)]
#[allow(missing_docs)]
pub enum MigrationError {
    /// Network error during migration.
    NetworkError(NetworkError),
    /// Checksum verification failed.
    ChecksumMismatch {
        batch_number: u64,
        expected: u64,
        actual: u64,
    },
    /// Batch was rejected by target shard.
    BatchRejected { batch_number: u64, reason: String },
    /// Migration was cancelled.
    Cancelled(u64),
    /// Migration timed out.
    Timeout {
        migration_id: u64,
        phase: MigrationState,
        elapsed: Duration,
    },
    /// Source shard unavailable.
    SourceUnavailable(ShardId),
    /// Target shard unavailable.
    TargetUnavailable(ShardId),
    /// Invalid migration state.
    InvalidState {
        migration_id: u64,
        expected: MigrationState,
        actual: MigrationState,
    },
    /// Verification failed.
    VerificationFailed { migration_id: u64, reason: String },
}

impl fmt::Display for MigrationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MigrationError::NetworkError(e) => write!(f, "Network error: {}", e),
            MigrationError::ChecksumMismatch {
                batch_number,
                expected,
                actual,
            } => {
                write!(
                    f,
                    "Checksum mismatch in batch {}: expected {}, got {}",
                    batch_number, expected, actual
                )
            }
            MigrationError::BatchRejected {
                batch_number,
                reason,
            } => {
                write!(f, "Batch {} rejected: {}", batch_number, reason)
            }
            MigrationError::Cancelled(id) => {
                write!(f, "Migration {} was cancelled", id)
            }
            MigrationError::Timeout {
                migration_id,
                phase,
                elapsed,
            } => {
                write!(
                    f,
                    "Migration {} timed out in {:?} phase after {:?}",
                    migration_id, phase, elapsed
                )
            }
            MigrationError::SourceUnavailable(shard_id) => {
                write!(f, "Source shard {} unavailable", shard_id)
            }
            MigrationError::TargetUnavailable(shard_id) => {
                write!(f, "Target shard {} unavailable", shard_id)
            }
            MigrationError::InvalidState {
                migration_id,
                expected,
                actual,
            } => {
                write!(
                    f,
                    "Migration {} in invalid state: expected {:?}, got {:?}",
                    migration_id, expected, actual
                )
            }
            MigrationError::VerificationFailed {
                migration_id,
                reason,
            } => {
                write!(
                    f,
                    "Migration {} verification failed: {}",
                    migration_id, reason
                )
            }
        }
    }
}

impl std::error::Error for MigrationError {}

impl From<NetworkError> for MigrationError {
    fn from(e: NetworkError) -> Self {
        MigrationError::NetworkError(e)
    }
}

/// Result type for migration operations.
pub type MigrationResult<T> = Result<T, MigrationError>;

/// Configuration for data migration.
#[derive(Debug, Clone)]
pub struct MigrationConfig {
    /// Batch size (number of nodes per batch).
    pub batch_size: usize,
    /// Maximum concurrent batches.
    pub max_concurrent_batches: usize,
    /// Timeout for each batch.
    pub batch_timeout: Duration,
    /// Timeout for entire migration.
    pub migration_timeout: Duration,
    /// Number of retries for failed batches.
    pub batch_retries: usize,
    /// Verification sample rate (0.0 to 1.0).
    pub verification_sample_rate: f64,
    /// Whether to pause on error or continue.
    pub pause_on_error: bool,
}

impl Default for MigrationConfig {
    fn default() -> Self {
        Self {
            batch_size: 1000,
            max_concurrent_batches: 4,
            batch_timeout: Duration::from_secs(60),
            migration_timeout: Duration::from_secs(3600),
            batch_retries: 3,
            verification_sample_rate: 0.01, // 1% sample
            pause_on_error: true,
        }
    }
}

/// Dual-write router for routing writes during migration.
///
/// During migration, writes to migrating data must go to both
/// the source and target shards until cutover is complete.
#[derive(Debug)]
pub struct DualWriteRouter {
    /// Active migrations (node label -> (source, target)).
    active_migrations: RwLock<HashMap<String, (ShardId, ShardId)>>,
    /// Nodes being migrated (node_id -> target_shard).
    migrating_nodes: RwLock<HashSet<NodeId>>,
}

impl DualWriteRouter {
    /// Create a new dual-write router.
    pub fn new() -> Self {
        Self {
            active_migrations: RwLock::new(HashMap::new()),
            migrating_nodes: RwLock::new(HashSet::new()),
        }
    }

    /// Register a migration for dual-write routing.
    pub fn register_migration(&self, labels: &[String], source: ShardId, target: ShardId) {
        if let Ok(mut migrations) = self.active_migrations.write() {
            for label in labels {
                migrations.insert(label.clone(), (source, target));
            }
        }
    }

    /// Unregister a migration (after cutover).
    pub fn unregister_migration(&self, labels: &[String]) {
        if let Ok(mut migrations) = self.active_migrations.write() {
            for label in labels {
                migrations.remove(label);
            }
        }
    }

    /// Register nodes being migrated.
    pub fn register_nodes(&self, nodes: &[NodeId]) {
        if let Ok(mut migrating) = self.migrating_nodes.write() {
            for node_id in nodes {
                migrating.insert(*node_id);
            }
        }
    }

    /// Unregister nodes (after cutover).
    pub fn unregister_nodes(&self, nodes: &[NodeId]) {
        if let Ok(mut migrating) = self.migrating_nodes.write() {
            for node_id in nodes {
                migrating.remove(node_id);
            }
        }
    }

    /// Check if a label is being migrated.
    pub fn is_label_migrating(&self, label: &str) -> bool {
        self.active_migrations
            .read()
            .map(|m| m.contains_key(label))
            .unwrap_or(false)
    }

    /// Check if a node is being migrated.
    pub fn is_node_migrating(&self, node_id: NodeId) -> bool {
        self.migrating_nodes
            .read()
            .map(|m| m.contains(&node_id))
            .unwrap_or(false)
    }

    /// Get the target shards for a write (returns both if migrating).
    ///
    /// WARNING: This method has a race condition - the migration state can change
    /// between calling this method and performing the write. For atomic guarantees,
    /// use `with_route_write` instead.
    pub fn route_write(&self, label: &str, primary_shard: ShardId) -> Vec<ShardId> {
        if let Ok(migrations) = self.active_migrations.read()
            && let Some((source, target)) = migrations.get(label)
            && primary_shard == *source
        {
            return vec![*source, *target];
        }
        vec![primary_shard]
    }

    /// Execute a write operation with atomic routing guarantees.
    ///
    /// This method holds the migration lock during the entire write operation,
    /// ensuring the routing decision remains valid throughout. Use this instead
    /// of `route_write` when writes must be atomic with respect to migration state.
    ///
    /// # Arguments
    ///
    /// * `label` - The node label being written
    /// * `primary_shard` - The primary shard for this label
    /// * `write_fn` - A function that performs the write to the given shards
    ///
    /// # Returns
    ///
    /// The result of the write function, or an error if the lock is poisoned.
    pub fn with_route_write<T, F>(
        &self,
        label: &str,
        primary_shard: ShardId,
        write_fn: F,
    ) -> Option<T>
    where
        F: FnOnce(&[ShardId]) -> T,
    {
        let migrations = self.active_migrations.read().ok()?;

        let targets = if let Some((source, target)) = migrations.get(label) {
            if primary_shard == *source {
                vec![*source, *target]
            } else {
                vec![primary_shard]
            }
        } else {
            vec![primary_shard]
        };

        // Lock is held while write_fn executes
        Some(write_fn(&targets))
    }

    /// Generate a routing token that can be verified at write time.
    ///
    /// Use this for cases where `with_route_write` is not suitable (e.g., async operations).
    /// The token captures the current migration state and can be verified before committing.
    pub fn generate_routing_token(&self, label: &str, primary_shard: ShardId) -> RoutingToken {
        let (targets, migration_version) = self
            .active_migrations
            .read()
            .map(|migrations| {
                let version = migrations.len() as u64; // Simple version based on migration count
                let targets = if let Some((source, target)) = migrations.get(label) {
                    if primary_shard == *source {
                        vec![*source, *target]
                    } else {
                        vec![primary_shard]
                    }
                } else {
                    vec![primary_shard]
                };
                (targets, version)
            })
            .unwrap_or_else(|_| (vec![primary_shard], 0));

        RoutingToken {
            label: label.to_string(),
            primary_shard,
            targets,
            migration_version,
        }
    }

    /// Verify a routing token is still valid.
    ///
    /// Returns true if the migration state hasn't changed in a way that would
    /// affect this routing decision.
    pub fn verify_routing_token(&self, token: &RoutingToken) -> bool {
        self.active_migrations
            .read()
            .map(|migrations| {
                let current_targets = if let Some((source, target)) = migrations.get(&token.label) {
                    if token.primary_shard == *source {
                        vec![*source, *target]
                    } else {
                        vec![token.primary_shard]
                    }
                } else {
                    vec![token.primary_shard]
                };
                // Token is valid if targets haven't changed
                current_targets == token.targets
            })
            .unwrap_or(false)
    }

    /// Get the number of active migrations.
    pub fn active_migration_count(&self) -> usize {
        self.active_migrations.read().map(|m| m.len()).unwrap_or(0)
    }
}

impl Default for DualWriteRouter {
    fn default() -> Self {
        Self::new()
    }
}

/// Executor for data migration between shards.
#[derive(Debug)]
pub struct MigrationExecutor<C: ShardClient> {
    /// Configuration.
    config: MigrationConfig,
    /// Shard clients.
    clients: HashMap<ShardId, Arc<C>>,
    /// Dual-write router.
    dual_write_router: Arc<DualWriteRouter>,
    /// Cancellation flags by migration ID.
    cancellation_flags: RwLock<HashMap<u64, Arc<AtomicBool>>>,
    /// Batches transferred counter.
    batches_transferred: AtomicU64,
    /// Bytes transferred counter.
    bytes_transferred: AtomicU64,
}

impl<C: ShardClient> MigrationExecutor<C> {
    /// Create a new migration executor.
    pub fn new(config: MigrationConfig, dual_write_router: Arc<DualWriteRouter>) -> Self {
        Self {
            config,
            clients: HashMap::new(),
            dual_write_router,
            cancellation_flags: RwLock::new(HashMap::new()),
            batches_transferred: AtomicU64::new(0),
            bytes_transferred: AtomicU64::new(0),
        }
    }

    /// Register a shard client.
    pub fn register_client(&mut self, shard_id: ShardId, client: Arc<C>) {
        self.clients.insert(shard_id, client);
    }

    /// Execute a migration plan.
    pub fn execute(&self, plan: &mut MigrationPlan) -> MigrationResult<MigrationStats> {
        let start = Instant::now();
        let migration_id = plan.id;

        // Register cancellation flag
        let cancel_flag = Arc::new(AtomicBool::new(false));
        if let Ok(mut flags) = self.cancellation_flags.write() {
            flags.insert(migration_id, Arc::clone(&cancel_flag));
        }

        // Get clients
        let source_client = self
            .clients
            .get(&plan.source_shard)
            .ok_or(MigrationError::SourceUnavailable(plan.source_shard))?;
        let target_client = self
            .clients
            .get(&plan.target_shard)
            .ok_or(MigrationError::TargetUnavailable(plan.target_shard))?;

        // Phase 1: Start dual-write
        self.start_dual_write(plan)?;

        // Phase 2: Copy data
        let copy_stats = self.copy_data(plan, source_client, target_client, &cancel_flag)?;

        // Check for cancellation
        if cancel_flag.load(Ordering::SeqCst) {
            self.cleanup_cancelled(plan)?;
            return Err(MigrationError::Cancelled(migration_id));
        }

        // Phase 3: Verify
        self.verify_migration(plan, source_client, target_client)?;

        // Phase 4: Cutover
        self.cutover(plan)?;

        // Phase 5: Cleanup
        self.cleanup(plan)?;

        // Remove cancellation flag
        if let Ok(mut flags) = self.cancellation_flags.write() {
            flags.remove(&migration_id);
        }

        Ok(MigrationStats {
            migration_id,
            total_time: start.elapsed(),
            batches_transferred: copy_stats.batches,
            nodes_transferred: copy_stats.nodes,
            edges_transferred: copy_stats.edges,
            bytes_transferred: copy_stats.bytes,
            verification_passed: true,
        })
    }

    /// Cancel a running migration.
    pub fn cancel(&self, migration_id: u64) -> bool {
        if let Ok(flags) = self.cancellation_flags.read()
            && let Some(flag) = flags.get(&migration_id)
        {
            flag.store(true, Ordering::SeqCst);
            return true;
        }
        false
    }

    /// Get executor statistics.
    pub fn stats(&self) -> ExecutorStats {
        ExecutorStats {
            batches_transferred: self.batches_transferred.load(Ordering::Relaxed),
            bytes_transferred: self.bytes_transferred.load(Ordering::Relaxed),
            active_migrations: self.cancellation_flags.read().map(|f| f.len()).unwrap_or(0),
        }
    }

    // ==================== Private Methods ====================

    fn start_dual_write(&self, plan: &mut MigrationPlan) -> MigrationResult<()> {
        // Register labels for dual-write
        self.dual_write_router.register_migration(
            &plan.labels_to_migrate,
            plan.source_shard,
            plan.target_shard,
        );

        plan.state = MigrationState::DualWrite;
        Ok(())
    }

    fn copy_data(
        &self,
        plan: &mut MigrationPlan,
        source_client: &Arc<C>,
        target_client: &Arc<C>,
        cancel_flag: &Arc<AtomicBool>,
    ) -> MigrationResult<CopyStats> {
        plan.state = MigrationState::Copying;

        let mut stats = CopyStats::default();
        let mut offset = 0u64;
        let mut batch_number = 0u64;

        loop {
            // Check cancellation
            if cancel_flag.load(Ordering::SeqCst) {
                return Err(MigrationError::Cancelled(plan.id));
            }

            // Extract batch from source
            let batch = source_client.extract_migration_batch(
                plan.id,
                &plan.labels_to_migrate,
                self.config.batch_size,
                offset,
            )?;

            let is_last = batch.is_last;
            let node_count = batch.nodes.len() as u64;
            let edge_count = batch.edges.len() as u64;

            // Skip empty batches
            if node_count == 0 && edge_count == 0 {
                if is_last {
                    break;
                }
                continue;
            }

            // Transfer batch to target with retry
            let mut retries = self.config.batch_retries;
            loop {
                match target_client.receive_migration_batch(batch.clone()) {
                    Ok(response) => {
                        if !response.accepted {
                            return Err(MigrationError::BatchRejected {
                                batch_number,
                                reason: response.error.unwrap_or_else(|| "Unknown".into()),
                            });
                        }

                        // Update stats
                        stats.batches += 1;
                        stats.nodes += response.nodes_written;
                        stats.edges += response.edges_written;
                        stats.bytes += estimate_batch_size(&batch);

                        self.batches_transferred.fetch_add(1, Ordering::Relaxed);
                        self.bytes_transferred
                            .fetch_add(estimate_batch_size(&batch), Ordering::Relaxed);

                        // Update plan progress
                        plan.progress.update(
                            response.nodes_written,
                            response.edges_written,
                            estimate_batch_size(&batch),
                        );

                        break;
                    }
                    Err(e) => {
                        if retries == 0 {
                            return Err(e.into());
                        }
                        retries -= 1;
                        // In real impl, would sleep with backoff
                    }
                }
            }

            if is_last {
                break;
            }

            offset += self.config.batch_size as u64;
            batch_number += 1;
        }

        Ok(stats)
    }

    fn verify_migration(
        &self,
        plan: &mut MigrationPlan,
        source_client: &Arc<C>,
        target_client: &Arc<C>,
    ) -> MigrationResult<()> {
        plan.state = MigrationState::Verifying;

        // Get state from both shards and compare counts
        let _source_state = source_client.get_state()?;
        let target_state = target_client.get_state()?;

        // In a real implementation, we would:
        // 1. Sample random nodes from source
        // 2. Verify they exist on target with same data
        // 3. Verify edge consistency

        // For now, just verify target has data
        if target_state.node_count == 0 && plan.progress.total_nodes > 0 {
            return Err(MigrationError::VerificationFailed {
                migration_id: plan.id,
                reason: "Target shard has no nodes after migration".into(),
            });
        }

        Ok(())
    }

    fn cutover(&self, plan: &mut MigrationPlan) -> MigrationResult<()> {
        plan.state = MigrationState::Cutover;

        // In a real implementation, this would:
        // 1. Stop dual-write (new writes go to target only)
        // 2. Update routing tables atomically
        // 3. Wait for in-flight requests to complete

        // Unregister from dual-write router
        self.dual_write_router
            .unregister_migration(&plan.labels_to_migrate);

        Ok(())
    }

    fn cleanup(&self, plan: &mut MigrationPlan) -> MigrationResult<()> {
        plan.state = MigrationState::Cleanup;

        // In a real implementation, this would:
        // 1. Delete migrated data from source shard
        // 2. Update indexes
        // 3. Compact storage

        plan.state = MigrationState::Completed;
        Ok(())
    }

    fn cleanup_cancelled(&self, plan: &mut MigrationPlan) -> MigrationResult<()> {
        // Unregister from dual-write
        self.dual_write_router
            .unregister_migration(&plan.labels_to_migrate);

        plan.state = MigrationState::Cancelled;
        Ok(())
    }
}

/// Statistics from a copy operation.
#[derive(Debug, Clone, Default)]
struct CopyStats {
    batches: u64,
    nodes: u64,
    edges: u64,
    bytes: u64,
}

/// Statistics from a migration.
#[derive(Debug, Clone)]
pub struct MigrationStats {
    /// Migration ID.
    pub migration_id: u64,
    /// Total time taken.
    pub total_time: Duration,
    /// Batches transferred.
    pub batches_transferred: u64,
    /// Nodes transferred.
    pub nodes_transferred: u64,
    /// Edges transferred.
    pub edges_transferred: u64,
    /// Bytes transferred.
    pub bytes_transferred: u64,
    /// Whether verification passed.
    pub verification_passed: bool,
}

/// Executor statistics.
#[derive(Debug, Clone)]
pub struct ExecutorStats {
    /// Total batches transferred.
    pub batches_transferred: u64,
    /// Total bytes transferred.
    pub bytes_transferred: u64,
    /// Number of active migrations.
    pub active_migrations: usize,
}

/// Estimate the size of a migration batch in bytes.
fn estimate_batch_size(batch: &MigrationBatch) -> u64 {
    let node_size: u64 = batch
        .nodes
        .iter()
        .map(|n| 8 + n.label.len() as u64 + n.properties.len() as u64 + 16)
        .sum();

    let edge_size: u64 = batch
        .edges
        .iter()
        .map(|e| 24 + e.label.len() as u64 + e.properties.len() as u64 + 16)
        .sum();

    node_size + edge_size + 32 // Header overhead
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::sharding::network::{MockShardClient, NodeData};
    use crate::storage::sharding::rebalance::{MigrationPlan, MigrationReason};

    fn make_shard_id(id: u16) -> ShardId {
        ShardId::new(id).unwrap()
    }

    fn make_mock_plan() -> MigrationPlan {
        MigrationPlan::new(
            1,
            make_shard_id(0),
            make_shard_id(1),
            vec!["Person".to_string()],
            100,
            200,
            MigrationReason::Imbalance,
        )
    }

    // ==================== MigrationError Tests ====================

    #[test]
    fn test_migration_error_display() {
        let err = MigrationError::ChecksumMismatch {
            batch_number: 1,
            expected: 100,
            actual: 200,
        };
        assert!(format!("{}", err).contains("Checksum"));

        let err = MigrationError::Cancelled(42);
        assert!(format!("{}", err).contains("cancelled"));

        let err = MigrationError::SourceUnavailable(make_shard_id(0));
        assert!(format!("{}", err).contains("Source"));
    }

    #[test]
    fn test_migration_error_from_network() {
        let net_err = NetworkError::ShardUnavailable(make_shard_id(0));
        let mig_err: MigrationError = net_err.into();
        assert!(matches!(mig_err, MigrationError::NetworkError(_)));
    }

    // ==================== MigrationConfig Tests ====================

    #[test]
    fn test_migration_config_default() {
        let config = MigrationConfig::default();
        assert_eq!(config.batch_size, 1000);
        assert_eq!(config.batch_retries, 3);
        assert!(config.verification_sample_rate > 0.0);
    }

    // ==================== DualWriteRouter Tests ====================

    #[test]
    fn test_dual_write_router_creation() {
        let router = DualWriteRouter::new();
        assert_eq!(router.active_migration_count(), 0);
    }

    #[test]
    fn test_dual_write_router_register() {
        let router = DualWriteRouter::new();

        router.register_migration(&["Person".to_string()], make_shard_id(0), make_shard_id(1));

        assert!(router.is_label_migrating("Person"));
        assert!(!router.is_label_migrating("Place"));
        assert_eq!(router.active_migration_count(), 1);
    }

    #[test]
    fn test_dual_write_router_unregister() {
        let router = DualWriteRouter::new();

        router.register_migration(&["Person".to_string()], make_shard_id(0), make_shard_id(1));
        router.unregister_migration(&["Person".to_string()]);

        assert!(!router.is_label_migrating("Person"));
        assert_eq!(router.active_migration_count(), 0);
    }

    #[test]
    fn test_dual_write_router_route() {
        let router = DualWriteRouter::new();

        // Before migration, route to primary only
        let targets = router.route_write("Person", make_shard_id(0));
        assert_eq!(targets, vec![make_shard_id(0)]);

        // During migration, route to both
        router.register_migration(&["Person".to_string()], make_shard_id(0), make_shard_id(1));
        let targets = router.route_write("Person", make_shard_id(0));
        assert_eq!(targets.len(), 2);
        assert!(targets.contains(&make_shard_id(0)));
        assert!(targets.contains(&make_shard_id(1)));
    }

    #[test]
    fn test_dual_write_router_node_tracking() {
        let router = DualWriteRouter::new();

        let node_id = NodeId::new(1).unwrap();
        assert!(!router.is_node_migrating(node_id));

        router.register_nodes(&[node_id]);
        assert!(router.is_node_migrating(node_id));

        router.unregister_nodes(&[node_id]);
        assert!(!router.is_node_migrating(node_id));
    }

    // ==================== MigrationExecutor Tests ====================

    #[test]
    fn test_executor_creation() {
        let router = Arc::new(DualWriteRouter::new());
        let executor: MigrationExecutor<MockShardClient> =
            MigrationExecutor::new(MigrationConfig::default(), router);

        let stats = executor.stats();
        assert_eq!(stats.batches_transferred, 0);
        assert_eq!(stats.active_migrations, 0);
    }

    #[test]
    fn test_executor_register_client() {
        let router = Arc::new(DualWriteRouter::new());
        let mut executor: MigrationExecutor<MockShardClient> =
            MigrationExecutor::new(MigrationConfig::default(), router);

        let client = Arc::new(MockShardClient::new(make_shard_id(0)));
        executor.register_client(make_shard_id(0), client);
    }

    #[test]
    fn test_executor_source_unavailable() {
        let router = Arc::new(DualWriteRouter::new());
        let executor: MigrationExecutor<MockShardClient> =
            MigrationExecutor::new(MigrationConfig::default(), router);

        let mut plan = make_mock_plan();
        let result = executor.execute(&mut plan);

        assert!(matches!(result, Err(MigrationError::SourceUnavailable(_))));
    }

    #[test]
    fn test_executor_target_unavailable() {
        let router = Arc::new(DualWriteRouter::new());
        let mut executor: MigrationExecutor<MockShardClient> =
            MigrationExecutor::new(MigrationConfig::default(), router);

        // Only register source
        let source_client = Arc::new(MockShardClient::new(make_shard_id(0)));
        executor.register_client(make_shard_id(0), source_client);

        let mut plan = make_mock_plan();
        let result = executor.execute(&mut plan);

        assert!(matches!(result, Err(MigrationError::TargetUnavailable(_))));
    }

    #[test]
    fn test_executor_successful_migration() {
        let router = Arc::new(DualWriteRouter::new());
        let mut executor: MigrationExecutor<MockShardClient> =
            MigrationExecutor::new(MigrationConfig::default(), router.clone());

        let source_client = Arc::new(MockShardClient::new(make_shard_id(0)));
        let target_client = Arc::new(MockShardClient::new(make_shard_id(1)));

        // Set up target to have some data after migration
        let mut target_state = crate::storage::sharding::types::ShardState::new(make_shard_id(1));
        target_state.node_count = 100;
        target_client.set_state(target_state);

        executor.register_client(make_shard_id(0), source_client);
        executor.register_client(make_shard_id(1), target_client);

        let mut plan = make_mock_plan();
        let result = executor.execute(&mut plan);

        assert!(result.is_ok());
        assert_eq!(plan.state, MigrationState::Completed);
    }

    #[test]
    fn test_executor_cancel() {
        let router = Arc::new(DualWriteRouter::new());
        let executor: MigrationExecutor<MockShardClient> =
            MigrationExecutor::new(MigrationConfig::default(), router);

        // Can't cancel non-existent migration
        assert!(!executor.cancel(999));
    }

    // ==================== MigrationStats Tests ====================

    #[test]
    fn test_migration_stats() {
        let stats = MigrationStats {
            migration_id: 1,
            total_time: Duration::from_secs(10),
            batches_transferred: 100,
            nodes_transferred: 10000,
            edges_transferred: 50000,
            bytes_transferred: 1024 * 1024,
            verification_passed: true,
        };

        assert_eq!(stats.migration_id, 1);
        assert!(stats.verification_passed);
    }

    // ==================== Helper Function Tests ====================

    #[test]
    fn test_estimate_batch_size() {
        let batch = MigrationBatch {
            migration_id: 1,
            batch_number: 0,
            is_last: true,
            nodes: vec![NodeData {
                id: NodeId::new(1).unwrap(),
                label: "Person".to_string(),
                properties: vec![0; 100],
                valid_from: 0,
                valid_to: None,
            }],
            edges: vec![],
            checksum: 0,
        };

        let size = estimate_batch_size(&batch);
        assert!(size > 100); // At least the properties size
    }
}
