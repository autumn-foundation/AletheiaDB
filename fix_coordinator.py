import re

with open("src/storage/sharding/coordinator.rs", "r") as f:
    content = f.read()

# RecoveryResult
content = content.replace(
    "/// Result of recovery operation.\n#[derive(Debug, Clone)]\npub struct RecoveryResult",
    "/// Result of recovery operation.\n///\n/// # Details\n///\n/// Returned when the `ShardCoordinator` replays pending decisions\n/// from the log, typically during startup.\n#[derive(Debug, Clone)]\npub struct RecoveryResult"
)

content = content.replace(
    "    pub fn is_complete(&self) -> bool {",
    "    /// Check if recovery was fully successful (no dead letters).\n    ///\n    /// # Details\n    ///\n    /// A successful recovery means all pending transactions were either\n    /// completed or successfully aborted, with none stuck in an indeterminate state.\n    pub fn is_complete(&self) -> bool {"
)

content = content.replace(
    "    pub fn dead_letter_count(&self) -> usize {",
    "    /// Get the number of transactions that required manual intervention.\n    ///\n    /// # Details\n    ///\n    /// These are transactions that exceeded the maximum number of recovery\n    /// attempts due to persistent participant failures and require an operator\n    /// to manually resolve them.\n    pub fn dead_letter_count(&self) -> usize {"
)

# DeadLetteredTransaction
content = content.replace(
    "/// A transaction that failed recovery and requires manual intervention.\n#[derive(Debug, Clone)]\npub struct DeadLetteredTransaction",
    "/// A transaction that failed recovery and requires manual intervention.\n///\n/// # Details\n///\n/// Transactions enter this state when they exhaust all retry attempts\n/// during crash recovery. Operators can inspect these and retry or abort them.\n#[derive(Debug, Clone)]\npub struct DeadLetteredTransaction"
)

# ShardConnection
content = content.replace(
    "/// Connection to a shard (placeholder for actual network implementation).\n#[derive(Debug)]\npub struct ShardConnection",
    "/// Connection to a shard (placeholder for actual network implementation).\n///\n/// # Details\n///\n/// Maintains the endpoint string, health status, and synchronizes the\n/// Hybrid Logical Clock (HLC) frontier with the remote shard.\n#[derive(Debug)]\npub struct ShardConnection"
)

content = content.replace(
    "    pub fn new(shard_id: ShardId, endpoint: String) -> Self {",
    "    /// Create a new shard connection.\n    ///\n    /// # Details\n    ///\n    /// Initializes the connection as healthy by default and sets the initial\n    /// HLC frontier to the current wallclock time.\n    ///\n    /// # Examples\n    ///\n    /// ```rust\n    /// use aletheiadb::storage::sharding::coordinator::ShardConnection;\n    /// use aletheiadb::storage::sharding::types::ShardId;\n    ///\n    /// let shard_id = ShardId::new(1).unwrap();\n    /// let connection = ShardConnection::new(shard_id, \"http://localhost:8080\".to_string());\n    /// ```\n    pub fn new(shard_id: ShardId, endpoint: String) -> Self {"
)

content = content.replace(
    "    pub fn prepare(\n        &self,\n        _tx_id: TxId,\n        timestamp: Option<HybridTimestamp>,\n    ) -> Result<(), DistributedTxError> {",
    "    /// Simulate a prepare call to the shard.\n    ///\n    /// # Details\n    ///\n    /// This is part of the 2PC protocol. It sends the `Prepare` message to the\n    /// participant. If the connection is unhealthy, it returns an error immediately.\n    /// It also synchronizes the HLC clock with the remote timestamp.\n    pub fn prepare(\n        &self,\n        _tx_id: TxId,\n        timestamp: Option<HybridTimestamp>,\n    ) -> Result<(), DistributedTxError> {"
)

content = content.replace(
    "    pub fn commit(\n        &self,\n        _tx_id: TxId,\n        commit_timestamp: Option<HybridTimestamp>,\n    ) -> Result<(), DistributedTxError> {",
    "    /// Simulate a commit call to the shard.\n    ///\n    /// # Details\n    ///\n    /// This is part of the 2PC protocol. It sends the `Commit` message to the\n    /// participant to finalize the transaction.\n    pub fn commit(\n        &self,\n        _tx_id: TxId,\n        commit_timestamp: Option<HybridTimestamp>,\n    ) -> Result<(), DistributedTxError> {"
)

content = content.replace(
    "    pub fn abort(&self, _tx_id: TxId) -> Result<(), DistributedTxError> {",
    "    /// Simulate an abort call to the shard.\n    ///\n    /// # Details\n    ///\n    /// Rolls back the specified transaction on this shard if the coordinator\n    /// decides to abort the transaction.\n    pub fn abort(&self, _tx_id: TxId) -> Result<(), DistributedTxError> {"
)

content = content.replace(
    "    pub fn health_check(&mut self) -> bool {",
    "    /// Perform a health check.\n    ///\n    /// # Details\n    ///\n    /// Pings the remote shard to update the connection's healthy status.\n    /// Returns the current health state.\n    pub fn health_check(&mut self) -> bool {"
)

content = content.replace(
    "    pub fn mark_unhealthy(&mut self) {",
    "    /// Mark the connection as unhealthy.\n    ///\n    /// # Details\n    ///\n    /// Used manually or automatically when requests to the shard timeout or fail.\n    pub fn mark_unhealthy(&mut self) {"
)

content = content.replace(
    "    pub fn mark_healthy(&mut self) {",
    "    /// Mark the connection as healthy.\n    ///\n    /// # Details\n    ///\n    /// Resets the connection to a healthy state after a successful health check.\n    pub fn mark_healthy(&mut self) {"
)

# ShardCoordinator missing methods
content = content.replace(
    "    pub fn with_rebalance_config(mut self, config: RebalanceConfig) -> Self {",
    "    /// Create a coordinator with custom rebalance config.\n    ///\n    /// # Details\n    ///\n    /// Overrides the default rebalance settings to tune the frequency\n    /// and aggressiveness of data migrations.\n    pub fn with_rebalance_config(mut self, config: RebalanceConfig) -> Self {"
)

content = content.replace(
    "    pub fn router(&self) -> &ShardRouter {",
    "    /// Get the router.\n    ///\n    /// # Details\n    ///\n    /// Provides access to the `ShardRouter` for routing queries\n    /// based on node labels.\n    pub fn router(&self) -> &ShardRouter {"
)

content = content.replace(
    "    pub fn route_node(&self, label: &str) -> ShardId {",
    "    /// Route a node query.\n    ///\n    /// # Details\n    ///\n    /// Hashes the node's label to determine its assigned shard.\n    pub fn route_node(&self, label: &str) -> ShardId {"
)

content = content.replace(
    "    pub fn route_traversal(&self, start_label: &str, target_labels: &[&str]) -> TraversalPlan {",
    "    /// Route a traversal query.\n    ///\n    /// # Details\n    ///\n    /// Creates an execution plan for distributed traversals by identifying\n    /// which shards need to be queried based on the start and target labels.\n    pub fn route_traversal(&self, start_label: &str, target_labels: &[&str]) -> TraversalPlan {"
)

content = content.replace(
    "    pub fn get_shard_state(&self, shard_id: ShardId) -> Option<ShardState> {",
    "    /// Get the state of a shard.\n    ///\n    /// # Details\n    ///\n    /// Retrieves the current status and metrics (like node count) for\n    /// a specific shard, if it exists.\n    pub fn get_shard_state(&self, shard_id: ShardId) -> Option<ShardState> {"
)

content = content.replace(
    "    pub fn get_all_shard_states(&self) -> Vec<ShardState> {",
    "    /// Get all shard states.\n    ///\n    /// # Details\n    ///\n    /// Returns a snapshot of the states for all managed shards, useful\n    /// for rebalancing and cluster monitoring.\n    pub fn get_all_shard_states(&self) -> Vec<ShardState> {"
)

content = content.replace(
    "    pub fn update_shard_state(&self, shard_id: ShardId, state: ShardState) {",
    "    /// Update shard state.\n    ///\n    /// # Details\n    ///\n    /// Updates the local view of a shard's state based on heartbeats\n    /// or metadata syncing.\n    pub fn update_shard_state(&self, shard_id: ShardId, state: ShardState) {"
)

with open("src/storage/sharding/coordinator.rs", "w") as f:
    f.write(content)
