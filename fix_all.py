import re

files = {
    "src/storage/sharding/coordinator.rs": [
        (
            r"/// Result of recovery operation\.\n#\[derive\(Debug, Clone\)\]\npub struct RecoveryResult",
            "/// Result of recovery operation.\n///\n/// # Details\n///\n/// Returned when the `ShardCoordinator` replays pending decisions\n/// from the log, typically during startup.\n#[derive(Debug, Clone)]\npub struct RecoveryResult"
        ),
        (
            r"/// Check if recovery was fully successful \(no dead letters\)\.\n    pub fn is_complete\(&self\) -> bool",
            "/// Check if recovery was fully successful (no dead letters).\n    ///\n    /// # Details\n    ///\n    /// A successful recovery means all pending transactions were either\n    /// completed or successfully aborted, with none stuck in an indeterminate state.\n    pub fn is_complete(&self) -> bool"
        ),
        (
            r"/// Get the number of transactions that required manual intervention\.\n    pub fn dead_letter_count\(&self\) -> usize",
            "/// Get the number of transactions that required manual intervention.\n    ///\n    /// # Details\n    ///\n    /// These are transactions that exceeded the maximum number of recovery\n    /// attempts due to persistent participant failures and require an operator\n    /// to manually resolve them.\n    pub fn dead_letter_count(&self) -> usize"
        ),
        (
            r"/// A transaction that failed recovery and requires manual intervention\.\n#\[derive\(Debug, Clone\)\]\npub struct DeadLetteredTransaction",
            "/// A transaction that failed recovery and requires manual intervention.\n///\n/// # Details\n///\n/// Transactions enter this state when they exhaust all retry attempts\n/// during crash recovery. Operators can inspect these and retry or abort them.\n#[derive(Debug, Clone)]\npub struct DeadLetteredTransaction"
        ),
        (
            r"/// Connection to a shard \(placeholder for actual network implementation\)\.\n#\[derive\(Debug\)\]\npub struct ShardConnection",
            "/// Connection to a shard (placeholder for actual network implementation).\n///\n/// # Details\n///\n/// Maintains the endpoint string, health status, and synchronizes the\n/// Hybrid Logical Clock (HLC) frontier with the remote shard.\n#[derive(Debug)]\npub struct ShardConnection"
        ),
        (
            r"/// Create a new shard connection\.\n    pub fn new\(shard_id: ShardId, endpoint: String\) -> Self",
            "/// Create a new shard connection.\n    ///\n    /// # Details\n    ///\n    /// Initializes the connection as healthy by default and sets the initial\n    /// HLC frontier to the current wallclock time.\n    ///\n    /// # Examples\n    ///\n    /// ```rust\n    /// use aletheiadb::storage::sharding::coordinator::ShardConnection;\n    /// use aletheiadb::storage::sharding::types::ShardId;\n    ///\n    /// let shard_id = ShardId::new(1).unwrap();\n    /// let connection = ShardConnection::new(shard_id, \"http://localhost:8080\".to_string());\n    /// ```\n    pub fn new(shard_id: ShardId, endpoint: String) -> Self"
        ),
        (
            r"/// Simulate a prepare call to the shard\.\n    pub fn prepare",
            "/// Simulate a prepare call to the shard.\n    ///\n    /// # Details\n    ///\n    /// This is part of the 2PC protocol. It sends the `Prepare` message to the\n    /// participant. If the connection is unhealthy, it returns an error immediately.\n    /// It also synchronizes the HLC clock with the remote timestamp.\n    pub fn prepare"
        ),
        (
            r"/// Simulate a commit call to the shard\.\n    pub fn commit",
            "/// Simulate a commit call to the shard.\n    ///\n    /// # Details\n    ///\n    /// This is part of the 2PC protocol. It sends the `Commit` message to the\n    /// participant to finalize the transaction.\n    pub fn commit"
        ),
        (
            r"/// Simulate an abort call to the shard\.\n    pub fn abort",
            "/// Simulate an abort call to the shard.\n    ///\n    /// # Details\n    ///\n    /// Rolls back the specified transaction on this shard if the coordinator\n    /// decides to abort the transaction.\n    pub fn abort"
        ),
        (
            r"/// Perform a health check\.\n    pub fn health_check",
            "/// Perform a health check.\n    ///\n    /// # Details\n    ///\n    /// Pings the remote shard to update the connection's healthy status.\n    /// Returns the current health state.\n    pub fn health_check"
        ),
        (
            r"/// Mark the connection as unhealthy\.\n    pub fn mark_unhealthy",
            "/// Mark the connection as unhealthy.\n    ///\n    /// # Details\n    ///\n    /// Used manually or automatically when requests to the shard timeout or fail.\n    pub fn mark_unhealthy"
        ),
        (
            r"/// Mark the connection as healthy\.\n    pub fn mark_healthy",
            "/// Mark the connection as healthy.\n    ///\n    /// # Details\n    ///\n    /// Resets the connection to a healthy state after a successful health check.\n    pub fn mark_healthy"
        ),
        (
            r"/// Create a coordinator with custom rebalance config\.\n    pub fn with_rebalance_config",
            "/// Create a coordinator with custom rebalance config.\n    ///\n    /// # Details\n    ///\n    /// Overrides the default rebalance settings to tune the frequency\n    /// and aggressiveness of data migrations.\n    pub fn with_rebalance_config"
        ),
        (
            r"/// Get the router\.\n    pub fn router",
            "/// Get the router.\n    ///\n    /// # Details\n    ///\n    /// Provides access to the `ShardRouter` for routing queries\n    /// based on node labels.\n    pub fn router"
        ),
        (
            r"/// Route a node query\.\n    pub fn route_node",
            "/// Route a node query.\n    ///\n    /// # Details\n    ///\n    /// Hashes the node's label to determine its assigned shard.\n    pub fn route_node"
        ),
        (
            r"/// Route a traversal query\.\n    pub fn route_traversal",
            "/// Route a traversal query.\n    ///\n    /// # Details\n    ///\n    /// Creates an execution plan for distributed traversals by identifying\n    /// which shards need to be queried based on the start and target labels.\n    pub fn route_traversal"
        ),
        (
            r"/// Get the state of a shard\.\n    pub fn get_shard_state",
            "/// Get the state of a shard.\n    ///\n    /// # Details\n    ///\n    /// Retrieves the current status and metrics (like node count) for\n    /// a specific shard, if it exists.\n    pub fn get_shard_state"
        ),
        (
            r"/// Get all shard states\.\n    pub fn get_all_shard_states",
            "/// Get all shard states.\n    ///\n    /// # Details\n    ///\n    /// Returns a snapshot of the states for all managed shards, useful\n    /// for rebalancing and cluster monitoring.\n    pub fn get_all_shard_states"
        ),
        (
            r"/// Update shard state\.\n    pub fn update_shard_state",
            "/// Update shard state.\n    ///\n    /// # Details\n    ///\n    /// Updates the local view of a shard's state based on heartbeats\n    /// or metadata syncing.\n    pub fn update_shard_state"
        )
    ],
    "src/storage/sharding/rebalance.rs": [
        (
            r"/// State of a migration\.\n#\[derive\(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize\)\]\npub enum MigrationState",
            "/// State of a migration.\n///\n/// # Details\n///\n/// Tracks the lifecycle of moving data between shards, from queued to completion or failure.\n#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]\npub enum MigrationState"
        ),
        (
            r"/// Reason for a migration\.\n#\[derive\(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize\)\]\npub enum MigrationReason",
            "/// Reason for a migration.\n///\n/// # Details\n///\n/// Specifies whether the migration is to balance size, CPU load, or manually requested.\n#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]\npub enum MigrationReason"
        ),
        (
            r"/// Rebalance error\.\n#\[derive\(Debug, Clone, Error\)\]\npub enum RebalanceError",
            "/// Rebalance error.\n///\n/// # Details\n///\n/// Errors related to data migration and sharding rebalance plans.\n#[derive(Debug, Clone, Error)]\npub enum RebalanceError"
        ),
        (
            r"/// Progress of a migration operation\.\n#\[derive\(Debug, Clone\)\]\npub struct MigrationProgress",
            "/// Progress of a migration operation.\n///\n/// # Details\n///\n/// Tracks the number of nodes and edges copied, as well as elapsed time.\n#[derive(Debug, Clone)]\npub struct MigrationProgress"
        ),
        (
            r"/// Plan for a migration operation\.\n#\[derive\(Debug, Clone\)\]\npub struct MigrationPlan",
            "/// Plan for a migration operation.\n///\n/// # Details\n///\n/// Contains the source, target, items to move, and current state.\n#[derive(Debug, Clone)]\npub struct MigrationPlan"
        ),
        (
            r"/// Manager for shard rebalancing and migrations\.\npub struct RebalanceManager",
            "/// Manager for shard rebalancing and migrations.\n///\n/// # Details\n///\n/// Orchestrates the process of rebalancing shards when they become uneven,\n/// handling migration plans, queues, and concurrency limits.\npub struct RebalanceManager"
        ),
        (
            r"/// Create a new progress tracker\.\n    pub fn new\(total_nodes: u64, total_edges: u64\) -> Self",
            "/// Create a new progress tracker.\n    ///\n    /// # Details\n    ///\n    /// Initializes progress to zero with the expected total counts.\n    pub fn new(total_nodes: u64, total_edges: u64) -> Self"
        ),
        (
            r"/// Get overall completion percentage\.\n    pub fn percentage",
            "/// Get overall completion percentage.\n    ///\n    /// # Details\n    ///\n    /// Calculates the weighted average of node and edge copy progress,\n    /// returning a value between `0.0` and `100.0`.\n    pub fn percentage"
        ),
        (
            r"/// Get elapsed time since the migration started\.\n    pub fn elapsed",
            "/// Get elapsed time since the migration started.\n    ///\n    /// # Details\n    ///\n    /// Returns the duration from the start time, or 0 if not started.\n    pub fn elapsed"
        ),
        (
            r"/// Update progress counts\.\n    pub fn update",
            "/// Update progress counts.\n    ///\n    /// # Details\n    ///\n    /// Adds to the running totals of copied nodes, edges, and bytes.\n    pub fn update"
        ),
        (
            r"/// Record a failed item\.\n    pub fn record_error",
            "/// Record a failed item.\n    ///\n    /// # Details\n    ///\n    /// Increments the internal error counter.\n    pub fn record_error"
        ),
        (
            r"/// Check if the migration is completely finished\.\n    pub fn is_complete",
            "/// Check if the migration is completely finished.\n    ///\n    /// # Details\n    ///\n    /// True if both node and edge counts meet or exceed the expected totals.\n    pub fn is_complete"
        ),
        (
            r"/// Create a new generic migration plan\.\n    pub fn new",
            "/// Create a new generic migration plan.\n    ///\n    /// # Details\n    ///\n    /// Use this to manually build a migration from source to target.\n    pub fn new"
        ),
        (
            r"/// Create a migration plan for specific node IDs\.\n    pub fn for_nodes",
            "/// Create a migration plan for specific node IDs.\n    ///\n    /// # Details\n    ///\n    /// Pre-populates the items list with the exact nodes to move.\n    pub fn for_nodes"
        ),
        (
            r"/// Assign a specific priority to the migration\.\n    pub fn with_priority",
            "/// Assign a specific priority to the migration.\n    ///\n    /// # Details\n    ///\n    /// Higher priority migrations are scheduled first.\n    pub fn with_priority"
        ),
        (
            r"/// Advance to the next logical state\.\n    pub fn advance_state",
            "/// Advance to the next logical state.\n    ///\n    /// # Details\n    ///\n    /// Validates the transition before changing state (e.g. Queued -> InProgress).\n    pub fn advance_state"
        ),
        (
            r"/// Mark the migration as failed\.\n    pub fn mark_failed",
            "/// Mark the migration as failed.\n    ///\n    /// # Details\n    ///\n    /// Transitions state to `Failed` and saves the error message.\n    pub fn mark_failed"
        ),
        (
            r"/// Cancel the migration\.\n    pub fn cancel",
            "/// Cancel the migration.\n    ///\n    /// # Details\n    ///\n    /// Aborts an ongoing or queued migration and transitions state to `Cancelled`.\n    pub fn cancel"
        ),
        (
            r"/// Check if the migration is in a terminal state\.\n    pub fn is_terminal",
            "/// Check if the migration is in a terminal state.\n    ///\n    /// # Details\n    ///\n    /// Returns true if it has Completed, Failed, or Cancelled.\n    pub fn is_terminal"
        ),
        (
            r"/// Check if the migration is currently active\.\n    pub fn is_active",
            "/// Check if the migration is currently active.\n    ///\n    /// # Details\n    ///\n    /// Returns true if it is InProgress or Finalizing.\n    pub fn is_active"
        ),
        (
            r"/// Create a new RebalanceManager\.\n    pub fn new",
            "/// Create a new RebalanceManager.\n    ///\n    /// # Details\n    ///\n    /// Initializes empty queues and the provided configuration.\n    pub fn new"
        ),
        (
            r"/// Plan a rebalance operation across all shards\.\n    pub fn plan_rebalance",
            "/// Plan a rebalance operation across all shards.\n    ///\n    /// # Details\n    ///\n    /// Identifies shards with counts below or above the average by the threshold.\n    /// Creates migration plans to shift items from overloaded shards to underloaded ones.\n    pub fn plan_rebalance"
        ),
        (
            r"/// Manually queue a migration plan\.\n    pub fn queue_migration",
            "/// Manually queue a migration plan.\n    ///\n    /// # Details\n    ///\n    /// Adds a migration to the pending queue and sorts by priority.\n    pub fn queue_migration"
        ),
        (
            r"/// Start the next pending migration\.\n    pub fn start_next_migration",
            "/// Start the next pending migration.\n    ///\n    /// # Details\n    ///\n    /// Checks if the maximum concurrency limit is reached, then pulls the\n    /// highest priority queued migration to make it active.\n    pub fn start_next_migration"
        )
    ],
    "src/query/executor/mod.rs": [
        (
            r"pub struct ExecutionConfig \{",
            "/// Configuration for the query executor.\n///\n/// # Details\n///\n/// Holds settings such as target batch size, parallel execution flags,\n/// and maximum hop depth.\npub struct ExecutionConfig {"
        ),
        (
            r"pub struct QueryExecutor \{",
            "/// Core engine for executing physical query plans.\n///\n/// # Details\n///\n/// Uses standard iterators to pull records out of current and historical\n/// storage seamlessly based on the execution plan logic.\npub struct QueryExecutor {"
        ),
        (
            r"pub fn new\(current: Arc<CurrentStorage>, historical: Arc<RwLock<HistoricalStorage>>\) -> Self \{",
            "/// Create a new query executor with default config.\n    ///\n    /// # Details\n    ///\n    /// The default configuration prioritizes minimal memory usage but does not\n    /// enable parallel execution.\n    pub fn new(current: Arc<CurrentStorage>, historical: Arc<RwLock<HistoricalStorage>>) -> Self {"
        ),
        (
            r"pub fn with_config",
            "/// Create a new query executor with custom config.\n    ///\n    /// # Details\n    ///\n    /// Use this to modify default batch sizes or concurrency targets before\n    /// executing queries.\n    pub fn with_config"
        ),
        (
            r"pub fn execute",
            "/// Execute a physical query plan.\n    ///\n    /// # Details\n    ///\n    /// Evaluates the sequence of operators defined in the plan against the underlying\n    /// storage engines. Materializes the results.\n    ///\n    /// # Examples\n    ///\n    /// ```rust\n    /// # use aletheiadb::query::executor::QueryExecutor;\n    /// # use aletheiadb::query::planner::physical::PhysicalPlan;\n    /// # use std::sync::{Arc, RwLock};\n    /// // let executor = QueryExecutor::new(current, historical);\n    /// // let results = executor.execute(plan).unwrap();\n    /// ```\n    pub fn execute"
        )
    ],
    "src/storage/index_persistence/temporal.rs": [
        (
            r"/// Convert a NodeVersion to an on-disk format entry\.\npub fn convert_node_version",
            "/// Convert a NodeVersion to an on-disk format entry.\n///\n/// # Details\n///\n/// Takes the in-memory representation of a node version and serializes it\n/// to a format compatible for disk storage.\npub fn convert_node_version"
        ),
        (
            r"/// Convert an EdgeVersion to an on-disk format entry\.\npub fn convert_edge_version",
            "/// Convert an EdgeVersion to an on-disk format entry.\n///\n/// # Details\n///\n/// Translates the in-memory representation of an edge version to its\n/// corresponding storage entity.\npub fn convert_edge_version"
        ),
        (
            r"/// Restore a NodeVersion from its on-disk representation\.\npub fn restore_node_version",
            "/// Restore a NodeVersion from its on-disk representation.\n///\n/// # Details\n///\n/// Reconstructs the original `NodeVersion` state including its metadata, properties,\n/// and embedded vector.\npub fn restore_node_version"
        ),
        (
            r"/// Restore an EdgeVersion from its on-disk representation\.\npub fn restore_edge_version",
            "/// Restore an EdgeVersion from its on-disk representation.\n///\n/// # Details\n///\n/// Deserializes a flat-packed edge definition back into memory for historical\n/// query resolution.\npub fn restore_edge_version"
        ),
        (
            r"/// Restore multiple entries directly into a historical storage instance\.\npub fn restore_into_historical_storage",
            "/// Restore multiple entries directly into a historical storage instance.\n///\n/// # Details\n///\n/// Uses batch ingestion to quickly restore an array of previously-saved entries\n/// back into memory without performing full transaction overhead.\npub fn restore_into_historical_storage"
        ),
        (
            r"/// Save the temporal index structure to a specified path\.\npub fn save_temporal_index",
            "/// Save the temporal index structure to a specified path.\n///\n/// # Details\n///\n/// Writes the entire `TemporalIndexData` payload to a persistent file location\n/// for fast durability outside of WAL entries.\npub fn save_temporal_index"
        ),
        (
            r"/// Load a temporal index from a given path\.\npub fn load_temporal_index",
            "/// Load a temporal index from a given path.\n///\n/// # Details\n///\n/// Reads the specified file and deserializes its contents to a `TemporalIndexData` object.\npub fn load_temporal_index"
        ),
        (
            r"/// Create a new, empty temporal index payload\.\npub fn new_temporal_index_data",
            "/// Create a new, empty temporal index payload.\n///\n/// # Details\n///\n/// Used for initializing an empty state prior to persisting the first snapshot.\npub fn new_temporal_index_data"
        )
    ]
}

for path, replacements in files.items():
    with open(path, "r") as f:
        content = f.read()

    for search, replace in replacements:
        content = re.sub(search, replace, content, count=1)

    with open(path, "w") as f:
        f.write(content)
