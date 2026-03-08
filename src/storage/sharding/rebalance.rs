//! Shard rebalancing implementation.
//!
//! Handles online data migration between shards when they become unbalanced
//! or when new shards are added to the cluster.
//!
//! # Online Data Migration Architecture
//!
//! Rebalancing a distributed database without downtime requires a careful orchestration
//! of states. AletheiaDB uses a **Dual-Write** approach to ensure that data being
//! migrated remains available and consistent even as application traffic continues.
//!
//! ## The Migration Lifecycle
//!
//! A [`MigrationPlan`] moves through a strict state machine defined by [`MigrationState`]:
//!
//! 1. **`Planned`**: The manager has identified an imbalance and queued a migration.
//! 2. **`DualWrite`**: The router is updated to send all *new* writes (for the migrating nodes)
//!    to **both** the source and target shards. Reads still go to the source.
//! 3. **`Copying`**: A background process copies the *existing* historical data from the
//!    source to the target. Because of `DualWrite`, we don't miss new updates.
//! 4. **`Verifying`**: Checksums and row counts are compared to ensure the target
//!    has a perfect copy of the source data.
//! 5. **`Cutover`**: The router is updated again. Reads and writes now go exclusively
//!    to the target shard. The migration is logically complete.
//! 6. **`Cleanup`**: The data on the source shard is asynchronously deleted to free space.
//! 7. **`Completed`**: The migration is finished and recorded in history.
//!
//! ## Cooldown and Concurrency
//!
//! To prevent the cluster from thrashing (rapidly moving data back and forth), the
//! [`RebalanceManager`] enforces a cooldown period between automated rebalance planning phases.
//! It also limits the number of concurrent migrations to avoid overwhelming network
//! and I/O resources.
//!
//! # Setup and Usage
//!
//! The rebalancing subsystem is configured via `RebalanceConfig` and coordinated
//! by the `RebalanceManager`. When shard states are submitted to the manager,
//! it compares their node counts against the configured thresholds to identify
//! imbalances and generates migration plans automatically.

use super::config::RebalanceConfig;
use super::types::{ShardId, ShardState};
use crate::core::id::NodeId;
use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::time::{Duration, Instant};

/// The strict state machine phases of a migration operation.
///
/// See the module-level documentation for an explanation of the Dual-Write lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MigrationState {
    /// Migration is planned but not started.
    Planned,
    /// Dual-write phase: new writes go to both shards.
    DualWrite,
    /// Background data copy in progress.
    Copying,
    /// Verifying data integrity before cutover.
    Verifying,
    /// Updating routing tables.
    Cutover,
    /// Cleaning up source shard.
    Cleanup,
    /// Migration completed successfully.
    Completed,
    /// Migration failed.
    Failed,
    /// Migration was cancelled.
    Cancelled,
}

impl fmt::Display for MigrationState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MigrationState::Planned => write!(f, "Planned"),
            MigrationState::DualWrite => write!(f, "DualWrite"),
            MigrationState::Copying => write!(f, "Copying"),
            MigrationState::Verifying => write!(f, "Verifying"),
            MigrationState::Cutover => write!(f, "Cutover"),
            MigrationState::Cleanup => write!(f, "Cleanup"),
            MigrationState::Completed => write!(f, "Completed"),
            MigrationState::Failed => write!(f, "Failed"),
            MigrationState::Cancelled => write!(f, "Cancelled"),
        }
    }
}

/// Progress of a migration operation.
#[derive(Debug, Clone)]
pub struct MigrationProgress {
    /// Total nodes to migrate.
    pub total_nodes: u64,
    /// Nodes migrated so far.
    pub migrated_nodes: u64,
    /// Total edges to migrate (including cross-shard edge updates).
    pub total_edges: u64,
    /// Edges migrated so far.
    pub migrated_edges: u64,
    /// Bytes transferred.
    pub bytes_transferred: u64,
    /// Errors encountered (non-fatal).
    pub errors: u64,
    /// Start time of the migration.
    pub start_time: Instant,
    /// Estimated completion time.
    pub estimated_completion: Option<Instant>,
}

impl MigrationProgress {
    /// Create a new progress tracker.
    pub fn new(total_nodes: u64, total_edges: u64) -> Self {
        Self {
            total_nodes,
            migrated_nodes: 0,
            total_edges,
            migrated_edges: 0,
            bytes_transferred: 0,
            errors: 0,
            start_time: Instant::now(),
            estimated_completion: None,
        }
    }

    /// Calculate the percentage of completion.
    pub fn percentage(&self) -> f64 {
        let total = self.total_nodes + self.total_edges;
        if total == 0 {
            return 100.0;
        }
        let completed = self.migrated_nodes + self.migrated_edges;
        (completed as f64 / total as f64) * 100.0
    }

    /// Calculate the elapsed time.
    pub fn elapsed(&self) -> Duration {
        self.start_time.elapsed()
    }

    /// Update progress and estimate completion time.
    pub fn update(&mut self, nodes: u64, edges: u64, bytes: u64) {
        self.migrated_nodes += nodes;
        self.migrated_edges += edges;
        self.bytes_transferred += bytes;

        // Estimate completion based on current rate
        let elapsed = self.start_time.elapsed().as_secs_f64();
        let progress = self.percentage() / 100.0;
        if progress > 0.0 && elapsed > 0.0 {
            let remaining_ratio = (1.0 - progress) / progress;
            let remaining_secs = (elapsed * remaining_ratio) as u64;
            self.estimated_completion = Some(Instant::now() + Duration::from_secs(remaining_secs));
        }
    }

    /// Record an error.
    pub fn record_error(&mut self) {
        self.errors += 1;
    }

    /// Check if migration is complete.
    pub fn is_complete(&self) -> bool {
        self.migrated_nodes >= self.total_nodes && self.migrated_edges >= self.total_edges
    }
}

/// A plan for migrating data between shards.
#[derive(Debug, Clone)]
pub struct MigrationPlan {
    /// Unique identifier for this migration.
    pub id: u64,
    /// Source shard.
    pub source_shard: ShardId,
    /// Target shard.
    pub target_shard: ShardId,
    /// Node labels to migrate.
    pub labels_to_migrate: Vec<String>,
    /// Specific node IDs to migrate (if not migrating by label).
    pub nodes_to_migrate: Option<Vec<NodeId>>,
    /// Current state of the migration.
    pub state: MigrationState,
    /// Progress tracking.
    pub progress: MigrationProgress,
    /// When this plan was created.
    pub created_at: Instant,
    /// Priority (higher = more urgent).
    pub priority: u32,
    /// Reason for this migration.
    pub reason: MigrationReason,
}

/// Reason for initiating a migration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MigrationReason {
    /// Rebalancing due to size imbalance.
    Imbalance,
    /// New shard was added.
    NewShard,
    /// Shard is being removed.
    ShardRemoval,
    /// Manual migration requested by admin.
    Manual,
    /// Optimization based on query patterns.
    QueryOptimization,
}

impl fmt::Display for MigrationReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MigrationReason::Imbalance => write!(f, "Imbalance"),
            MigrationReason::NewShard => write!(f, "NewShard"),
            MigrationReason::ShardRemoval => write!(f, "ShardRemoval"),
            MigrationReason::Manual => write!(f, "Manual"),
            MigrationReason::QueryOptimization => write!(f, "QueryOptimization"),
        }
    }
}

impl MigrationPlan {
    /// Create a new migration plan.
    pub fn new(
        id: u64,
        source: ShardId,
        target: ShardId,
        labels: Vec<String>,
        estimated_nodes: u64,
        estimated_edges: u64,
        reason: MigrationReason,
    ) -> Self {
        Self {
            id,
            source_shard: source,
            target_shard: target,
            labels_to_migrate: labels,
            nodes_to_migrate: None,
            state: MigrationState::Planned,
            progress: MigrationProgress::new(estimated_nodes, estimated_edges),
            created_at: Instant::now(),
            priority: 1,
            reason,
        }
    }

    /// Create a migration plan for specific nodes.
    pub fn for_nodes(
        id: u64,
        source: ShardId,
        target: ShardId,
        nodes: Vec<NodeId>,
        estimated_edges: u64,
        reason: MigrationReason,
    ) -> Self {
        let num_nodes = nodes.len() as u64;
        Self {
            id,
            source_shard: source,
            target_shard: target,
            labels_to_migrate: Vec::new(),
            nodes_to_migrate: Some(nodes),
            state: MigrationState::Planned,
            progress: MigrationProgress::new(num_nodes, estimated_edges),
            created_at: Instant::now(),
            priority: 1,
            reason,
        }
    }

    /// Set the priority.
    pub fn with_priority(mut self, priority: u32) -> Self {
        self.priority = priority;
        self
    }

    /// Transition to the next state.
    pub fn advance_state(&mut self) -> Result<(), RebalanceError> {
        self.state = match self.state {
            MigrationState::Planned => MigrationState::DualWrite,
            MigrationState::DualWrite => MigrationState::Copying,
            MigrationState::Copying => MigrationState::Verifying,
            MigrationState::Verifying => MigrationState::Cutover,
            MigrationState::Cutover => MigrationState::Cleanup,
            MigrationState::Cleanup => MigrationState::Completed,
            MigrationState::Completed | MigrationState::Failed | MigrationState::Cancelled => {
                return Err(RebalanceError::InvalidStateTransition {
                    from: self.state,
                    migration_id: self.id,
                });
            }
        };
        Ok(())
    }

    /// Mark the migration as failed.
    pub fn mark_failed(&mut self, reason: &str) {
        self.state = MigrationState::Failed;
        // In a real implementation, would log the reason
        let _ = reason;
    }

    /// Cancel the migration.
    pub fn cancel(&mut self) {
        self.state = MigrationState::Cancelled;
    }

    /// Check if the migration is in a terminal state.
    pub fn is_terminal(&self) -> bool {
        matches!(
            self.state,
            MigrationState::Completed | MigrationState::Failed | MigrationState::Cancelled
        )
    }

    /// Check if the migration is active.
    pub fn is_active(&self) -> bool {
        !self.is_terminal() && self.state != MigrationState::Planned
    }
}

/// Error types for rebalancing operations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RebalanceError {
    /// Invalid state transition.
    InvalidStateTransition {
        /// Current state.
        from: MigrationState,
        /// Migration ID.
        migration_id: u64,
    },
    /// Migration not found.
    MigrationNotFound(u64),
    /// Too many concurrent migrations.
    TooManyConcurrentMigrations {
        /// Current count.
        current: usize,
        /// Maximum allowed.
        max: usize,
    },
    /// Shard is unavailable.
    ShardUnavailable(ShardId),
    /// Migration timeout.
    Timeout {
        /// Migration ID.
        migration_id: u64,
        /// Phase that timed out.
        phase: MigrationState,
    },
    /// Data verification failed.
    VerificationFailed {
        /// Migration ID.
        migration_id: u64,
        /// Reason for failure.
        reason: String,
    },
    /// Cooldown period not elapsed.
    CooldownNotElapsed {
        /// Time remaining.
        remaining: Duration,
    },
}

impl fmt::Display for RebalanceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RebalanceError::InvalidStateTransition { from, migration_id } => {
                write!(
                    f,
                    "Invalid state transition from {} for migration {}",
                    from, migration_id
                )
            }
            RebalanceError::MigrationNotFound(id) => {
                write!(f, "Migration {} not found", id)
            }
            RebalanceError::TooManyConcurrentMigrations { current, max } => {
                write!(
                    f,
                    "Too many concurrent migrations: {} (max: {})",
                    current, max
                )
            }
            RebalanceError::ShardUnavailable(shard_id) => {
                write!(f, "Shard {} is unavailable", shard_id)
            }
            RebalanceError::Timeout {
                migration_id,
                phase,
            } => {
                write!(f, "Migration {} timed out in {} phase", migration_id, phase)
            }
            RebalanceError::VerificationFailed {
                migration_id,
                reason,
            } => {
                write!(
                    f,
                    "Verification failed for migration {}: {}",
                    migration_id, reason
                )
            }
            RebalanceError::CooldownNotElapsed { remaining } => {
                write!(f, "Cooldown not elapsed, {} remaining", remaining.as_secs())
            }
        }
    }
}

impl std::error::Error for RebalanceError {}

/// Manager for coordinating shard rebalancing operations.
///
/// The `RebalanceManager` acts as the control plane for data movement. It is responsible for:
/// - Calculating cluster imbalance and generating `MigrationPlan`s.
/// - Enforcing cooldown periods to prevent cluster thrashing.
/// - Managing the queue and concurrency limits of active migrations.
/// - Tracking the history of completed and failed migrations.
///
/// # Examples
///
/// ```rust
/// use aletheiadb::storage::sharding::config::RebalanceConfig;
/// use aletheiadb::storage::sharding::rebalance::RebalanceManager;
///
/// // Create a manager with default configuration
/// let config = RebalanceConfig::new();
/// let manager = RebalanceManager::new(config);
///
/// assert_eq!(manager.active_count(), 0);
/// assert_eq!(manager.queued_count(), 0);
/// ```
pub struct RebalanceManager {
    /// Configuration for rebalancing.
    config: RebalanceConfig,
    /// Queue of planned migrations.
    migration_queue: VecDeque<MigrationPlan>,
    /// Currently active migrations.
    active_migrations: HashMap<u64, MigrationPlan>,
    /// Completed migrations (for history).
    completed_migrations: VecDeque<MigrationPlan>,
    /// Migration ID generator.
    next_migration_id: u64,
    /// Last rebalance time (for cooldown).
    last_rebalance: Option<Instant>,
    /// Maximum completed migrations to keep in history.
    max_history: usize,
}

impl RebalanceManager {
    /// Create a new rebalance manager.
    pub fn new(config: RebalanceConfig) -> Self {
        Self {
            config,
            migration_queue: VecDeque::new(),
            active_migrations: HashMap::new(),
            completed_migrations: VecDeque::new(),
            next_migration_id: 0,
            last_rebalance: None,
            max_history: 100,
        }
    }

    /// Plan a rebalancing operation based on shard states.
    ///
    /// Returns a list of migration plans to execute.
    pub fn plan_rebalance(
        &mut self,
        shard_states: &[ShardState],
    ) -> Result<Vec<MigrationPlan>, RebalanceError> {
        // Check cooldown
        if let Some(last) = self.last_rebalance {
            let elapsed = last.elapsed();
            if elapsed < self.config.cooldown {
                return Err(RebalanceError::CooldownNotElapsed {
                    remaining: self.config.cooldown - elapsed,
                });
            }
        }

        // Calculate imbalance
        let total_nodes: u64 = shard_states.iter().map(|s| s.node_count).sum();
        if total_nodes == 0 {
            return Ok(Vec::new());
        }

        let avg_nodes = total_nodes / shard_states.len() as u64;

        // Find overloaded and underloaded shards
        let mut overloaded: Vec<&ShardState> = shard_states
            .iter()
            .filter(|s| {
                s.node_count as f64 > avg_nodes as f64 * (1.0 + self.config.imbalance_threshold)
            })
            .collect();

        let mut underloaded: Vec<&ShardState> = shard_states
            .iter()
            .filter(|s| {
                (s.node_count as f64) < avg_nodes as f64 * (1.0 - self.config.imbalance_threshold)
            })
            .collect();

        // Sort by severity
        overloaded.sort_by_key(|s| std::cmp::Reverse(s.node_count));
        underloaded.sort_by_key(|s| s.node_count);

        let mut plans = Vec::new();

        // Create migration plans
        for (over, under) in overloaded.iter().zip(underloaded.iter()) {
            let excess = over.node_count.saturating_sub(avg_nodes);
            let deficit = avg_nodes.saturating_sub(under.node_count);
            let to_migrate = excess.min(deficit).min(self.config.batch_size as u64);

            if to_migrate > 0 {
                let plan = MigrationPlan::new(
                    self.next_migration_id,
                    over.shard_id,
                    under.shard_id,
                    Vec::new(), // Labels will be determined by the coordinator
                    to_migrate,
                    to_migrate * 2, // Estimate 2 edges per node on average
                    MigrationReason::Imbalance,
                );
                self.next_migration_id += 1;
                plans.push(plan);
            }
        }

        self.last_rebalance = Some(Instant::now());
        Ok(plans)
    }

    /// Add a migration plan to the queue.
    pub fn queue_migration(&mut self, plan: MigrationPlan) {
        self.migration_queue.push_back(plan);
        // Sort by priority (higher priority first)
        let mut vec: Vec<_> = self.migration_queue.drain(..).collect();
        vec.sort_by_key(|p| std::cmp::Reverse(p.priority));
        self.migration_queue = vec.into_iter().collect();
    }

    /// Start the next migration from the queue.
    pub fn start_next_migration(&mut self) -> Result<Option<u64>, RebalanceError> {
        // Check if we can start more migrations
        if self.active_migrations.len() >= self.config.max_concurrent_migrations {
            return Err(RebalanceError::TooManyConcurrentMigrations {
                current: self.active_migrations.len(),
                max: self.config.max_concurrent_migrations,
            });
        }

        if let Some(mut plan) = self.migration_queue.pop_front() {
            let id = plan.id;
            plan.advance_state()?; // Move to DualWrite
            self.active_migrations.insert(id, plan);
            Ok(Some(id))
        } else {
            Ok(None)
        }
    }

    /// Advance a migration to the next phase.
    pub fn advance_migration(&mut self, migration_id: u64) -> Result<(), RebalanceError> {
        let plan = self
            .active_migrations
            .get_mut(&migration_id)
            .ok_or(RebalanceError::MigrationNotFound(migration_id))?;

        plan.advance_state()?;

        // If completed, move to history
        if plan.is_terminal()
            && let Some(completed) = self.active_migrations.remove(&migration_id)
        {
            self.add_to_history(completed);
        }

        Ok(())
    }

    /// Update migration progress.
    pub fn update_progress(
        &mut self,
        migration_id: u64,
        nodes: u64,
        edges: u64,
        bytes: u64,
    ) -> Result<(), RebalanceError> {
        let plan = self
            .active_migrations
            .get_mut(&migration_id)
            .ok_or(RebalanceError::MigrationNotFound(migration_id))?;

        plan.progress.update(nodes, edges, bytes);
        Ok(())
    }

    /// Mark a migration as failed.
    pub fn fail_migration(
        &mut self,
        migration_id: u64,
        reason: &str,
    ) -> Result<(), RebalanceError> {
        let plan = self
            .active_migrations
            .get_mut(&migration_id)
            .ok_or(RebalanceError::MigrationNotFound(migration_id))?;

        plan.mark_failed(reason);

        if let Some(failed) = self.active_migrations.remove(&migration_id) {
            self.add_to_history(failed);
        }

        Ok(())
    }

    /// Cancel a migration.
    pub fn cancel_migration(&mut self, migration_id: u64) -> Result<(), RebalanceError> {
        // Check active migrations first
        if let Some(plan) = self.active_migrations.get_mut(&migration_id) {
            plan.cancel();
            if let Some(cancelled) = self.active_migrations.remove(&migration_id) {
                self.add_to_history(cancelled);
            }
            return Ok(());
        }

        // Check queue
        let original_len = self.migration_queue.len();
        self.migration_queue.retain(|p| p.id != migration_id);
        if self.migration_queue.len() < original_len {
            return Ok(());
        }

        Err(RebalanceError::MigrationNotFound(migration_id))
    }

    /// Get the current status of a migration.
    pub fn get_migration(&self, migration_id: u64) -> Option<&MigrationPlan> {
        self.active_migrations
            .get(&migration_id)
            .or_else(|| self.migration_queue.iter().find(|p| p.id == migration_id))
            .or_else(|| {
                self.completed_migrations
                    .iter()
                    .find(|p| p.id == migration_id)
            })
    }

    /// Get all active migrations.
    pub fn active_migrations(&self) -> Vec<&MigrationPlan> {
        self.active_migrations.values().collect()
    }

    /// Get queued migrations.
    pub fn queued_migrations(&self) -> Vec<&MigrationPlan> {
        self.migration_queue.iter().collect()
    }

    /// Get completed migrations history.
    pub fn completed_migrations(&self) -> Vec<&MigrationPlan> {
        self.completed_migrations.iter().collect()
    }

    /// Check if any migrations are in progress.
    pub fn has_active_migrations(&self) -> bool {
        !self.active_migrations.is_empty()
    }

    /// Get the number of active migrations.
    pub fn active_count(&self) -> usize {
        self.active_migrations.len()
    }

    /// Get the number of queued migrations.
    pub fn queued_count(&self) -> usize {
        self.migration_queue.len()
    }

    /// Add a migration to the history, maintaining max size.
    fn add_to_history(&mut self, plan: MigrationPlan) {
        self.completed_migrations.push_front(plan);
        while self.completed_migrations.len() > self.max_history {
            self.completed_migrations.pop_back();
        }
    }

    /// Reset the cooldown timer (for testing).
    pub fn reset_cooldown(&mut self) {
        self.last_rebalance = None;
    }
}

impl std::fmt::Debug for RebalanceManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RebalanceManager")
            .field("active_migrations", &self.active_migrations.len())
            .field("queued_migrations", &self.migration_queue.len())
            .field("completed_migrations", &self.completed_migrations.len())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_shard_state(id: u16, node_count: u64) -> ShardState {
        let mut state = ShardState::new(ShardId::new(id).unwrap());
        state.node_count = node_count;
        state
    }

    #[test]
    fn test_migration_state_display() {
        assert_eq!(format!("{}", MigrationState::Planned), "Planned");
        assert_eq!(format!("{}", MigrationState::DualWrite), "DualWrite");
        assert_eq!(format!("{}", MigrationState::Copying), "Copying");
        assert_eq!(format!("{}", MigrationState::Completed), "Completed");
    }

    #[test]
    fn test_migration_progress() {
        let mut progress = MigrationProgress::new(100, 200);
        assert_eq!(progress.percentage(), 0.0);
        assert!(!progress.is_complete());

        progress.update(50, 100, 1024);
        assert!((progress.percentage() - 50.0).abs() < 0.1);

        progress.update(50, 100, 1024);
        assert!((progress.percentage() - 100.0).abs() < 0.1);
        assert!(progress.is_complete());
    }

    #[test]
    fn test_migration_progress_error_tracking() {
        let mut progress = MigrationProgress::new(100, 100);
        assert_eq!(progress.errors, 0);

        progress.record_error();
        progress.record_error();
        assert_eq!(progress.errors, 2);
    }

    #[test]
    fn test_migration_plan_creation() {
        let plan = MigrationPlan::new(
            1,
            ShardId::new(0).unwrap(),
            ShardId::new(1).unwrap(),
            vec!["Person".to_string()],
            100,
            200,
            MigrationReason::Imbalance,
        );

        assert_eq!(plan.id, 1);
        assert_eq!(plan.source_shard.as_u16(), 0);
        assert_eq!(plan.target_shard.as_u16(), 1);
        assert_eq!(plan.state, MigrationState::Planned);
        assert!(!plan.is_terminal());
        assert!(!plan.is_active());
    }

    #[test]
    fn test_migration_plan_for_nodes() {
        let nodes = vec![
            NodeId::new(1).unwrap(),
            NodeId::new(2).unwrap(),
            NodeId::new(3).unwrap(),
        ];
        let plan = MigrationPlan::for_nodes(
            1,
            ShardId::new(0).unwrap(),
            ShardId::new(1).unwrap(),
            nodes.clone(),
            10,
            MigrationReason::Manual,
        );

        assert_eq!(plan.nodes_to_migrate.as_ref().unwrap().len(), 3);
        assert_eq!(plan.progress.total_nodes, 3);
    }

    #[test]
    fn test_migration_plan_state_transitions() {
        let mut plan = MigrationPlan::new(
            1,
            ShardId::new(0).unwrap(),
            ShardId::new(1).unwrap(),
            vec![],
            10,
            20,
            MigrationReason::Imbalance,
        );

        assert_eq!(plan.state, MigrationState::Planned);

        plan.advance_state().unwrap();
        assert_eq!(plan.state, MigrationState::DualWrite);
        assert!(plan.is_active());

        plan.advance_state().unwrap();
        assert_eq!(plan.state, MigrationState::Copying);

        plan.advance_state().unwrap();
        assert_eq!(plan.state, MigrationState::Verifying);

        plan.advance_state().unwrap();
        assert_eq!(plan.state, MigrationState::Cutover);

        plan.advance_state().unwrap();
        assert_eq!(plan.state, MigrationState::Cleanup);

        plan.advance_state().unwrap();
        assert_eq!(plan.state, MigrationState::Completed);
        assert!(plan.is_terminal());

        // Can't advance from terminal state
        assert!(plan.advance_state().is_err());
    }

    #[test]
    fn test_migration_plan_failure() {
        let mut plan = MigrationPlan::new(
            1,
            ShardId::new(0).unwrap(),
            ShardId::new(1).unwrap(),
            vec![],
            10,
            20,
            MigrationReason::Imbalance,
        );

        plan.mark_failed("test failure");
        assert_eq!(plan.state, MigrationState::Failed);
        assert!(plan.is_terminal());
    }

    #[test]
    fn test_migration_plan_cancel() {
        let mut plan = MigrationPlan::new(
            1,
            ShardId::new(0).unwrap(),
            ShardId::new(1).unwrap(),
            vec![],
            10,
            20,
            MigrationReason::Imbalance,
        );

        plan.cancel();
        assert_eq!(plan.state, MigrationState::Cancelled);
        assert!(plan.is_terminal());
    }

    #[test]
    fn test_rebalance_manager_creation() {
        let config = RebalanceConfig::new();
        let manager = RebalanceManager::new(config);

        assert_eq!(manager.active_count(), 0);
        assert_eq!(manager.queued_count(), 0);
        assert!(!manager.has_active_migrations());
    }

    #[test]
    fn test_rebalance_manager_plan_rebalance() {
        let config = RebalanceConfig::new().with_imbalance_threshold(0.3);
        let mut manager = RebalanceManager::new(config);

        let states = vec![make_shard_state(0, 1000), make_shard_state(1, 100)];

        let plans = manager.plan_rebalance(&states).unwrap();
        assert!(!plans.is_empty());

        let plan = &plans[0];
        assert_eq!(plan.source_shard.as_u16(), 0);
        assert_eq!(plan.target_shard.as_u16(), 1);
        assert_eq!(plan.reason, MigrationReason::Imbalance);
    }

    #[test]
    fn test_rebalance_manager_cooldown() {
        let config = RebalanceConfig::new().with_cooldown(Duration::from_secs(3600));
        let mut manager = RebalanceManager::new(config);

        let states = vec![make_shard_state(0, 1000), make_shard_state(1, 100)];

        // First rebalance should succeed
        let _ = manager.plan_rebalance(&states).unwrap();

        // Second should fail due to cooldown
        let result = manager.plan_rebalance(&states);
        assert!(result.is_err());
        if let Err(RebalanceError::CooldownNotElapsed { .. }) = result {
            // Expected
        } else {
            panic!("Expected CooldownNotElapsed error");
        }

        // Reset cooldown and try again
        manager.reset_cooldown();
        let _ = manager.plan_rebalance(&states).unwrap();
    }

    #[test]
    fn test_rebalance_manager_queue_migration() {
        let config = RebalanceConfig::new();
        let mut manager = RebalanceManager::new(config);

        let plan1 = MigrationPlan::new(
            1,
            ShardId::new(0).unwrap(),
            ShardId::new(1).unwrap(),
            vec![],
            100,
            200,
            MigrationReason::Imbalance,
        )
        .with_priority(1);

        let plan2 = MigrationPlan::new(
            2,
            ShardId::new(0).unwrap(),
            ShardId::new(1).unwrap(),
            vec![],
            100,
            200,
            MigrationReason::Manual,
        )
        .with_priority(10);

        manager.queue_migration(plan1);
        manager.queue_migration(plan2);

        assert_eq!(manager.queued_count(), 2);

        // Higher priority should be first
        let queued = manager.queued_migrations();
        assert_eq!(queued[0].priority, 10);
    }

    #[test]
    fn test_rebalance_manager_start_migration() {
        let config = RebalanceConfig::new();
        let mut manager = RebalanceManager::new(config);

        let plan = MigrationPlan::new(
            1,
            ShardId::new(0).unwrap(),
            ShardId::new(1).unwrap(),
            vec![],
            100,
            200,
            MigrationReason::Imbalance,
        );

        manager.queue_migration(plan);
        assert_eq!(manager.queued_count(), 1);

        let id = manager.start_next_migration().unwrap();
        assert_eq!(id, Some(1));
        assert_eq!(manager.queued_count(), 0);
        assert_eq!(manager.active_count(), 1);
        assert!(manager.has_active_migrations());

        let active = manager.get_migration(1).unwrap();
        assert_eq!(active.state, MigrationState::DualWrite);
    }

    #[test]
    fn test_rebalance_manager_max_concurrent() {
        let config = RebalanceConfig::new();
        let mut manager = RebalanceManager::new(config);

        // Queue multiple migrations
        for i in 0..5 {
            let plan = MigrationPlan::new(
                i,
                ShardId::new(0).unwrap(),
                ShardId::new(1).unwrap(),
                vec![],
                100,
                200,
                MigrationReason::Imbalance,
            );
            manager.queue_migration(plan);
        }

        // Start up to max concurrent
        let max = manager.config.max_concurrent_migrations;
        for _ in 0..max {
            manager.start_next_migration().unwrap();
        }

        // Next should fail
        let result = manager.start_next_migration();
        assert!(result.is_err());
    }

    #[test]
    fn test_rebalance_manager_advance_migration() {
        let config = RebalanceConfig::new();
        let mut manager = RebalanceManager::new(config);

        let plan = MigrationPlan::new(
            1,
            ShardId::new(0).unwrap(),
            ShardId::new(1).unwrap(),
            vec![],
            100,
            200,
            MigrationReason::Imbalance,
        );

        manager.queue_migration(plan);
        manager.start_next_migration().unwrap();

        // Advance through all states
        for _ in 0..5 {
            manager.advance_migration(1).unwrap();
        }

        // Should now be completed and in history
        assert_eq!(manager.active_count(), 0);
        assert_eq!(manager.completed_migrations().len(), 1);

        let completed = manager.get_migration(1).unwrap();
        assert_eq!(completed.state, MigrationState::Completed);
    }

    #[test]
    fn test_rebalance_manager_update_progress() {
        let config = RebalanceConfig::new();
        let mut manager = RebalanceManager::new(config);

        let plan = MigrationPlan::new(
            1,
            ShardId::new(0).unwrap(),
            ShardId::new(1).unwrap(),
            vec![],
            100,
            200,
            MigrationReason::Imbalance,
        );

        manager.queue_migration(plan);
        manager.start_next_migration().unwrap();

        manager.update_progress(1, 50, 100, 1024).unwrap();

        let active = manager.get_migration(1).unwrap();
        assert_eq!(active.progress.migrated_nodes, 50);
        assert_eq!(active.progress.migrated_edges, 100);
    }

    #[test]
    fn test_rebalance_manager_fail_migration() {
        let config = RebalanceConfig::new();
        let mut manager = RebalanceManager::new(config);

        let plan = MigrationPlan::new(
            1,
            ShardId::new(0).unwrap(),
            ShardId::new(1).unwrap(),
            vec![],
            100,
            200,
            MigrationReason::Imbalance,
        );

        manager.queue_migration(plan);
        manager.start_next_migration().unwrap();

        manager.fail_migration(1, "test failure").unwrap();

        assert_eq!(manager.active_count(), 0);
        let failed = manager.get_migration(1).unwrap();
        assert_eq!(failed.state, MigrationState::Failed);
    }

    #[test]
    fn test_rebalance_manager_cancel_migration() {
        let config = RebalanceConfig::new();
        let mut manager = RebalanceManager::new(config);

        // Cancel from queue
        let plan = MigrationPlan::new(
            1,
            ShardId::new(0).unwrap(),
            ShardId::new(1).unwrap(),
            vec![],
            100,
            200,
            MigrationReason::Imbalance,
        );
        manager.queue_migration(plan);
        manager.cancel_migration(1).unwrap();
        assert_eq!(manager.queued_count(), 0);

        // Cancel active migration
        let plan = MigrationPlan::new(
            2,
            ShardId::new(0).unwrap(),
            ShardId::new(1).unwrap(),
            vec![],
            100,
            200,
            MigrationReason::Imbalance,
        );
        manager.queue_migration(plan);
        manager.start_next_migration().unwrap();
        manager.cancel_migration(2).unwrap();
        assert_eq!(manager.active_count(), 0);
    }

    #[test]
    fn test_rebalance_error_display() {
        let err = RebalanceError::TooManyConcurrentMigrations { current: 3, max: 2 };
        assert!(format!("{}", err).contains("Too many concurrent"));

        let err = RebalanceError::ShardUnavailable(ShardId::new(1).unwrap());
        assert!(format!("{}", err).contains("unavailable"));

        let err = RebalanceError::Timeout {
            migration_id: 1,
            phase: MigrationState::Copying,
        };
        assert!(format!("{}", err).contains("timed out"));
    }

    #[test]
    fn test_migration_reason_display() {
        assert_eq!(format!("{}", MigrationReason::Imbalance), "Imbalance");
        assert_eq!(format!("{}", MigrationReason::NewShard), "NewShard");
        assert_eq!(format!("{}", MigrationReason::Manual), "Manual");
    }

    #[test]
    fn test_rebalance_manager_debug() {
        let config = RebalanceConfig::new();
        let manager = RebalanceManager::new(config);
        let debug = format!("{:?}", manager);
        assert!(debug.contains("RebalanceManager"));
    }
}
