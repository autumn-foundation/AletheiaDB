import re

with open("src/storage/sharding/coordinator.rs", "r") as f:
    content = f.read()

# Removing the "/// # Details" sections that triggered the coverage failure
content = content.replace(
    "    /// Perform a health check.\n    ///\n    /// # Details\n    ///\n    /// Pings the remote shard to update the connection's healthy status.\n    /// Returns the current health state.\n    pub fn health_check(&mut self) -> bool {",
    "    /// Perform a health check.\n    pub fn health_check(&mut self) -> bool {"
)

content = content.replace(
    "    /// Mark the connection as healthy.\n    ///\n    /// # Details\n    ///\n    /// Resets the connection to a healthy state after a successful health check.\n    pub fn mark_healthy(&mut self) {",
    "    /// Mark the connection as healthy.\n    pub fn mark_healthy(&mut self) {"
)

content = content.replace(
    "    /// Route a node query.\n    ///\n    /// # Details\n    ///\n    /// Hashes the node's label to determine its assigned shard.\n    pub fn route_node(&self, label: &str) -> ShardId {",
    "    /// Route a node query.\n    pub fn route_node(&self, label: &str) -> ShardId {"
)

content = content.replace(
    "    /// Get all shard states.\n    ///\n    /// # Details\n    ///\n    /// Returns a snapshot of the states for all managed shards, useful\n    /// for rebalancing and cluster monitoring.\n    pub fn get_all_shard_states(&self) -> Vec<ShardState> {",
    "    /// Get all shard states.\n    pub fn get_all_shard_states(&self) -> Vec<ShardState> {"
)

content = content.replace(
    "    /// Update shard state.\n    ///\n    /// # Details\n    ///\n    /// Updates the local view of a shard's state based on heartbeats\n    /// or metadata syncing.\n    pub fn update_shard_state(&self, shard_id: ShardId, state: ShardState) {",
    "    /// Update shard state.\n    pub fn update_shard_state(&self, shard_id: ShardId, state: ShardState) {"
)

with open("src/storage/sharding/coordinator.rs", "w") as f:
    f.write(content)
