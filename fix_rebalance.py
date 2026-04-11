import re

with open("src/storage/sharding/rebalance.rs", "r") as f:
    content = f.read()

# Structs/Enums
content = content.replace(
    "/// State of a migration.\n#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]\npub enum MigrationState",
    "/// State of a migration.\n///\n/// # Details\n///\n/// Tracks the lifecycle of moving data between shards, from queued to completion or failure.\n#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]\npub enum MigrationState"
)

content = content.replace(
    "/// Reason for a migration.\n#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]\npub enum MigrationReason",
    "/// Reason for a migration.\n///\n/// # Details\n///\n/// Specifies whether the migration is to balance size, CPU load, or manually requested.\n#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]\npub enum MigrationReason"
)

content = content.replace(
    "/// Rebalance error.\n#[derive(Debug, Clone, Error)]\npub enum RebalanceError",
    "/// Rebalance error.\n///\n/// # Details\n///\n/// Errors related to data migration and sharding rebalance plans.\n#[derive(Debug, Clone, Error)]\npub enum RebalanceError"
)

content = content.replace(
    "/// Progress of a migration operation.\n#[derive(Debug, Clone)]\npub struct MigrationProgress",
    "/// Progress of a migration operation.\n///\n/// # Details\n///\n/// Tracks the number of nodes and edges copied, as well as elapsed time.\n#[derive(Debug, Clone)]\npub struct MigrationProgress"
)

content = content.replace(
    "/// Plan for a migration operation.\n#[derive(Debug, Clone)]\npub struct MigrationPlan",
    "/// Plan for a migration operation.\n///\n/// # Details\n///\n/// Contains the source, target, items to move, and current state.\n#[derive(Debug, Clone)]\npub struct MigrationPlan"
)

content = content.replace(
    "/// Manager for shard rebalancing and migrations.\npub struct RebalanceManager",
    "/// Manager for shard rebalancing and migrations.\n///\n/// # Details\n///\n/// Orchestrates the process of rebalancing shards when they become uneven,\n/// handling migration plans, queues, and concurrency limits.\npub struct RebalanceManager"
)


# MigrationProgress methods
content = content.replace(
    "    pub fn new(total_nodes: u64, total_edges: u64) -> Self {",
    "    /// Create a new progress tracker.\n    ///\n    /// # Details\n    ///\n    /// Initializes progress to zero with the expected total counts.\n    pub fn new(total_nodes: u64, total_edges: u64) -> Self {"
)

content = content.replace(
    "    pub fn percentage(&self) -> f64 {",
    "    /// Get overall completion percentage.\n    ///\n    /// # Details\n    ///\n    /// Calculates the weighted average of node and edge copy progress,\n    /// returning a value between `0.0` and `100.0`.\n    pub fn percentage(&self) -> f64 {"
)

content = content.replace(
    "    pub fn elapsed(&self) -> Duration {",
    "    /// Get elapsed time since the migration started.\n    ///\n    /// # Details\n    ///\n    /// Returns the duration from the start time, or 0 if not started.\n    pub fn elapsed(&self) -> Duration {"
)

content = content.replace(
    "    pub fn update(&mut self, nodes: u64, edges: u64, bytes: u64) {",
    "    /// Update progress counts.\n    ///\n    /// # Details\n    ///\n    /// Adds to the running totals of copied nodes, edges, and bytes.\n    pub fn update(&mut self, nodes: u64, edges: u64, bytes: u64) {"
)

content = content.replace(
    "    pub fn record_error(&mut self) {",
    "    /// Record a failed item.\n    ///\n    /// # Details\n    ///\n    /// Increments the internal error counter.\n    pub fn record_error(&mut self) {"
)

content = content.replace(
    "    pub fn is_complete(&self) -> bool {",
    "    /// Check if the migration is completely finished.\n    ///\n    /// # Details\n    ///\n    /// True if both node and edge counts meet or exceed the expected totals.\n    pub fn is_complete(&self) -> bool {"
)

# MigrationPlan methods
content = content.replace(
    "    pub fn new(\n        id: u64,\n        source_shard: ShardId,\n        target_shard: ShardId,\n        reason: MigrationReason,\n        expected_nodes: u64,\n        expected_edges: u64,\n    ) -> Self {",
    "    /// Create a new generic migration plan.\n    ///\n    /// # Details\n    ///\n    /// Use this to manually build a migration from source to target.\n    pub fn new(\n        id: u64,\n        source_shard: ShardId,\n        target_shard: ShardId,\n        reason: MigrationReason,\n        expected_nodes: u64,\n        expected_edges: u64,\n    ) -> Self {"
)

content = content.replace(
    "    pub fn for_nodes(\n        id: u64,\n        source_shard: ShardId,\n        target_shard: ShardId,\n        reason: MigrationReason,\n        nodes: Vec<NodeId>,\n        expected_edges: u64,\n    ) -> Self {",
    "    /// Create a migration plan for specific node IDs.\n    ///\n    /// # Details\n    ///\n    /// Pre-populates the items list with the exact nodes to move.\n    pub fn for_nodes(\n        id: u64,\n        source_shard: ShardId,\n        target_shard: ShardId,\n        reason: MigrationReason,\n        nodes: Vec<NodeId>,\n        expected_edges: u64,\n    ) -> Self {"
)

content = content.replace(
    "    pub fn with_priority(mut self, priority: u32) -> Self {",
    "    /// Assign a specific priority to the migration.\n    ///\n    /// # Details\n    ///\n    /// Higher priority migrations are scheduled first.\n    pub fn with_priority(mut self, priority: u32) -> Self {"
)

content = content.replace(
    "    pub fn advance_state(&mut self) -> Result<(), RebalanceError> {",
    "    /// Advance to the next logical state.\n    ///\n    /// # Details\n    ///\n    /// Validates the transition before changing state (e.g. Queued -> InProgress).\n    pub fn advance_state(&mut self) -> Result<(), RebalanceError> {"
)

content = content.replace(
    "    pub fn mark_failed(&mut self, reason: &str) {",
    "    /// Mark the migration as failed.\n    ///\n    /// # Details\n    ///\n    /// Transitions state to `Failed` and saves the error message.\n    pub fn mark_failed(&mut self, reason: &str) {"
)

content = content.replace(
    "    pub fn cancel(&mut self) {",
    "    /// Cancel the migration.\n    ///\n    /// # Details\n    ///\n    /// Aborts an ongoing or queued migration and transitions state to `Cancelled`.\n    pub fn cancel(&mut self) {"
)

content = content.replace(
    "    pub fn is_terminal(&self) -> bool {",
    "    /// Check if the migration is in a terminal state.\n    ///\n    /// # Details\n    ///\n    /// Returns true if it has Completed, Failed, or Cancelled.\n    pub fn is_terminal(&self) -> bool {"
)

content = content.replace(
    "    pub fn is_active(&self) -> bool {",
    "    /// Check if the migration is currently active.\n    ///\n    /// # Details\n    ///\n    /// Returns true if it is InProgress or Finalizing.\n    pub fn is_active(&self) -> bool {"
)


# RebalanceManager methods
content = content.replace(
    "    pub fn new(config: RebalanceConfig) -> Self {",
    "    /// Create a new RebalanceManager.\n    ///\n    /// # Details\n    ///\n    /// Initializes empty queues and the provided configuration.\n    pub fn new(config: RebalanceConfig) -> Self {"
)

content = content.replace(
    "    pub fn plan_rebalance(\n        &mut self,\n        shard_states: &HashMap<ShardId, ShardState>,\n    ) -> Vec<MigrationPlan> {",
    "    /// Plan a rebalance operation across all shards.\n    ///\n    /// # Details\n    ///\n    /// Identifies shards with counts below or above the average by the threshold.\n    /// Creates migration plans to shift items from overloaded shards to underloaded ones.\n    pub fn plan_rebalance(\n        &mut self,\n        shard_states: &HashMap<ShardId, ShardState>,\n    ) -> Vec<MigrationPlan> {"
)

content = content.replace(
    "    pub fn queue_migration(&mut self, plan: MigrationPlan) {",
    "    /// Manually queue a migration plan.\n    ///\n    /// # Details\n    ///\n    /// Adds a migration to the pending queue and sorts by priority.\n    pub fn queue_migration(&mut self, plan: MigrationPlan) {"
)

content = content.replace(
    "    pub fn start_next_migration(&mut self) -> Result<Option<u64>, RebalanceError> {",
    "    /// Start the next pending migration.\n    ///\n    /// # Details\n    ///\n    /// Checks if the maximum concurrency limit is reached, then pulls the\n    /// highest priority queued migration to make it active.\n    pub fn start_next_migration(&mut self) -> Result<Option<u64>, RebalanceError> {"
)

with open("src/storage/sharding/rebalance.rs", "w") as f:
    f.write(content)
