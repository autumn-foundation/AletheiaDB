//! Configuration types for the sharding system.

use super::types::ShardId;
use std::collections::HashMap;
use std::time::Duration;

/// Configuration for a single shard in the cluster.
#[derive(Debug, Clone)]
pub struct ShardDefinition {
    /// Unique identifier for this shard.
    pub id: ShardId,
    /// Network endpoint for this shard (e.g., "shard0.gallifrey.local:9000").
    pub endpoint: String,
    /// Node labels owned by this shard.
    /// Nodes with these labels will be routed to this shard.
    pub labels: Vec<String>,
    /// Optional read replica endpoints for load balancing.
    pub replicas: Vec<String>,
    /// Weight for load balancing (higher = more traffic).
    pub weight: u32,
}

impl ShardDefinition {
    /// Create a new shard definition.
    pub fn new<S: Into<String>>(id: u16, endpoint: S, labels: Vec<&str>) -> Self {
        Self {
            id: ShardId::new_unchecked(id),
            endpoint: endpoint.into(),
            labels: labels.into_iter().map(|s| s.to_string()).collect(),
            replicas: Vec::new(),
            weight: 100,
        }
    }

    /// Add replica endpoints.
    pub fn with_replicas(mut self, replicas: Vec<&str>) -> Self {
        self.replicas = replicas.into_iter().map(|s| s.to_string()).collect();
        self
    }

    /// Set the load balancing weight.
    pub fn with_weight(mut self, weight: u32) -> Self {
        self.weight = weight;
        self
    }
}

/// Method for discovering shard topology.
#[derive(Debug, Clone)]
pub enum ShardDiscovery {
    /// Static configuration (endpoints hardcoded).
    Static(Vec<String>),
    /// Discover shards via etcd.
    Etcd {
        /// etcd endpoints.
        endpoints: Vec<String>,
        /// Key prefix for shard discovery.
        prefix: String,
    },
    /// Discover shards via Consul.
    Consul {
        /// Consul address.
        address: String,
        /// Service name to discover.
        service: String,
    },
}

impl Default for ShardDiscovery {
    fn default() -> Self {
        ShardDiscovery::Static(Vec::new())
    }
}

/// Configuration for the sharding system.
#[derive(Debug, Clone)]
pub struct ShardConfig {
    /// List of shard definitions.
    pub shards: Vec<ShardDefinition>,
    /// Default shard for nodes without a matching label.
    pub default_shard: ShardId,
    /// How to discover shard topology.
    pub discovery: ShardDiscovery,
    /// Connection timeout for shard communication.
    pub connection_timeout: Duration,
    /// Request timeout for shard operations.
    pub request_timeout: Duration,
    /// Maximum number of connections per shard.
    pub max_connections_per_shard: usize,
    /// Enable automatic failover to replicas.
    pub auto_failover: bool,
    /// Health check interval.
    pub health_check_interval: Duration,
    /// Number of retries for failed operations.
    pub max_retries: u32,
    /// Base delay for exponential backoff.
    pub retry_base_delay: Duration,
    /// Path to the write-ahead log (optional).
    /// If None, an in-memory log is used (not durable).
    pub wal_path: Option<std::path::PathBuf>,
}

impl ShardConfig {
    /// Create a new shard configuration.
    pub fn new(shards: Vec<ShardDefinition>) -> Self {
        let default_shard = shards
            .first()
            .map(|s| s.id)
            .unwrap_or_else(|| ShardId::new_unchecked(0));

        Self {
            shards,
            default_shard,
            discovery: ShardDiscovery::default(),
            connection_timeout: Duration::from_secs(5),
            request_timeout: Duration::from_secs(30),
            max_connections_per_shard: 10,
            auto_failover: true,
            health_check_interval: Duration::from_secs(10),
            max_retries: 3,
            retry_base_delay: Duration::from_millis(100),
            wal_path: None,
        }
    }

    /// Create a single-shard configuration (no sharding).
    pub fn single_shard() -> Self {
        Self::new(vec![ShardDefinition::new(0, "localhost:9000", vec![])])
    }

    /// Set the default shard.
    pub fn with_default_shard(mut self, shard_id: ShardId) -> Self {
        self.default_shard = shard_id;
        self
    }

    /// Set the discovery mechanism.
    pub fn with_discovery(mut self, discovery: ShardDiscovery) -> Self {
        self.discovery = discovery;
        self
    }

    /// Set the connection timeout.
    pub fn with_connection_timeout(mut self, timeout: Duration) -> Self {
        self.connection_timeout = timeout;
        self
    }

    /// Set the request timeout.
    pub fn with_request_timeout(mut self, timeout: Duration) -> Self {
        self.request_timeout = timeout;
        self
    }

    /// Set max connections per shard.
    pub fn with_max_connections(mut self, max: usize) -> Self {
        self.max_connections_per_shard = max;
        self
    }

    /// Set the WAL path.
    pub fn with_wal_path<P: Into<std::path::PathBuf>>(mut self, path: P) -> Self {
        self.wal_path = Some(path.into());
        self
    }

    /// Build a label-to-shard mapping for fast routing.
    pub fn build_label_map(&self) -> HashMap<String, ShardId> {
        let mut map = HashMap::new();
        for shard in &self.shards {
            for label in &shard.labels {
                map.insert(label.clone(), shard.id);
            }
        }
        map
    }

    /// Get a shard definition by ID.
    pub fn get_shard(&self, id: ShardId) -> Option<&ShardDefinition> {
        self.shards.iter().find(|s| s.id == id)
    }

    /// Get all shard IDs.
    pub fn shard_ids(&self) -> Vec<ShardId> {
        self.shards.iter().map(|s| s.id).collect()
    }

    /// Get the number of shards.
    pub fn num_shards(&self) -> usize {
        self.shards.len()
    }

    /// Validate the configuration.
    pub fn validate(&self) -> Result<(), String> {
        // Check for duplicate shard IDs
        let mut seen_ids = std::collections::HashSet::new();
        for shard in &self.shards {
            if !seen_ids.insert(shard.id) {
                return Err(format!("Duplicate shard ID: {}", shard.id));
            }
        }

        // Check for duplicate label assignments
        let mut seen_labels = HashMap::new();
        for shard in &self.shards {
            for label in &shard.labels {
                if let Some(existing) = seen_labels.insert(label.clone(), shard.id) {
                    return Err(format!(
                        "Label '{}' assigned to multiple shards: {} and {}",
                        label, existing, shard.id
                    ));
                }
            }
        }

        // Verify default shard exists
        if !self.shards.iter().any(|s| s.id == self.default_shard) {
            return Err(format!(
                "Default shard {} not found in shard list",
                self.default_shard
            ));
        }

        Ok(())
    }
}

impl Default for ShardConfig {
    fn default() -> Self {
        Self::single_shard()
    }
}

/// Configuration for shard rebalancing.
#[derive(Debug, Clone)]
pub struct RebalanceConfig {
    /// Trigger rebalancing when size variance exceeds this threshold.
    /// Value is a ratio (e.g., 0.3 = 30% imbalance triggers rebalancing).
    pub imbalance_threshold: f64,
    /// Maximum nodes to migrate per batch.
    pub batch_size: usize,
    /// Minimum time between automatic rebalances.
    pub cooldown: Duration,
    /// Maximum concurrent migrations.
    pub max_concurrent_migrations: usize,
    /// Enable automatic rebalancing.
    pub auto_rebalance: bool,
    /// Delay before starting migration after dual-write phase.
    pub migration_delay: Duration,
    /// Timeout for individual migration operations.
    pub migration_timeout: Duration,
    /// Number of retries for failed migrations.
    pub migration_retries: u32,
}

impl RebalanceConfig {
    /// Create a new rebalance configuration with default values.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the imbalance threshold.
    pub fn with_imbalance_threshold(mut self, threshold: f64) -> Self {
        self.imbalance_threshold = threshold.clamp(0.0, 1.0);
        self
    }

    /// Set the batch size.
    pub fn with_batch_size(mut self, size: usize) -> Self {
        self.batch_size = size;
        self
    }

    /// Set the cooldown period.
    pub fn with_cooldown(mut self, cooldown: Duration) -> Self {
        self.cooldown = cooldown;
        self
    }

    /// Enable or disable automatic rebalancing.
    pub fn with_auto_rebalance(mut self, enabled: bool) -> Self {
        self.auto_rebalance = enabled;
        self
    }

    /// Check if a given imbalance exceeds the threshold.
    pub fn should_rebalance(&self, imbalance: f64) -> bool {
        self.auto_rebalance && imbalance > self.imbalance_threshold
    }
}

impl Default for RebalanceConfig {
    fn default() -> Self {
        Self {
            imbalance_threshold: 0.3, // 30%
            batch_size: 10_000,
            cooldown: Duration::from_secs(3600), // 1 hour
            max_concurrent_migrations: 2,
            auto_rebalance: true,
            migration_delay: Duration::from_secs(10),
            migration_timeout: Duration::from_secs(300), // 5 minutes
            migration_retries: 3,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_shard_definition_creation() {
        let def = ShardDefinition::new(0, "localhost:9000", vec!["Person", "User"]);
        assert_eq!(def.id.as_u16(), 0);
        assert_eq!(def.endpoint, "localhost:9000");
        assert_eq!(def.labels, vec!["Person", "User"]);
        assert!(def.replicas.is_empty());
        assert_eq!(def.weight, 100);
    }

    #[test]
    fn test_shard_definition_with_replicas() {
        let def = ShardDefinition::new(0, "primary:9000", vec!["Person"])
            .with_replicas(vec!["replica1:9000", "replica2:9000"])
            .with_weight(150);

        assert_eq!(def.replicas.len(), 2);
        assert_eq!(def.weight, 150);
    }

    #[test]
    fn test_shard_config_creation() {
        let config = ShardConfig::new(vec![
            ShardDefinition::new(0, "shard0:9000", vec!["Person", "User"]),
            ShardDefinition::new(1, "shard1:9000", vec!["Place", "Location"]),
        ]);

        assert_eq!(config.num_shards(), 2);
        assert_eq!(config.default_shard.as_u16(), 0);
    }

    #[test]
    fn test_shard_config_single_shard() {
        let config = ShardConfig::single_shard();
        assert_eq!(config.num_shards(), 1);
    }

    #[test]
    fn test_shard_config_label_map() {
        let config = ShardConfig::new(vec![
            ShardDefinition::new(0, "shard0:9000", vec!["Person", "User"]),
            ShardDefinition::new(1, "shard1:9000", vec!["Place", "Location"]),
        ]);

        let label_map = config.build_label_map();
        assert_eq!(label_map.get("Person").unwrap().as_u16(), 0);
        assert_eq!(label_map.get("User").unwrap().as_u16(), 0);
        assert_eq!(label_map.get("Place").unwrap().as_u16(), 1);
        assert_eq!(label_map.get("Location").unwrap().as_u16(), 1);
        assert!(!label_map.contains_key("Unknown"));
    }

    #[test]
    fn test_shard_config_get_shard() {
        let config = ShardConfig::new(vec![
            ShardDefinition::new(0, "shard0:9000", vec!["Person"]),
            ShardDefinition::new(1, "shard1:9000", vec!["Place"]),
        ]);

        let shard = config.get_shard(ShardId::new(1).unwrap());
        assert!(shard.is_some());
        assert_eq!(shard.unwrap().endpoint, "shard1:9000");

        assert!(config.get_shard(ShardId::new(99).unwrap()).is_none());
    }

    #[test]
    fn test_shard_config_validation() {
        // Valid config
        let config = ShardConfig::new(vec![
            ShardDefinition::new(0, "shard0:9000", vec!["Person"]),
            ShardDefinition::new(1, "shard1:9000", vec!["Place"]),
        ]);
        assert!(config.validate().is_ok());

        // Duplicate shard IDs
        let bad_config = ShardConfig::new(vec![
            ShardDefinition::new(0, "shard0:9000", vec!["Person"]),
            ShardDefinition::new(0, "shard1:9000", vec!["Place"]),
        ]);
        assert!(bad_config.validate().is_err());

        // Duplicate labels
        let bad_config = ShardConfig::new(vec![
            ShardDefinition::new(0, "shard0:9000", vec!["Person"]),
            ShardDefinition::new(1, "shard1:9000", vec!["Person"]),
        ]);
        assert!(bad_config.validate().is_err());
    }

    #[test]
    fn test_shard_config_default_shard_validation() {
        let mut config =
            ShardConfig::new(vec![ShardDefinition::new(0, "shard0:9000", vec!["Person"])]);

        // Set invalid default shard
        config.default_shard = ShardId::new(99).unwrap();
        assert!(config.validate().is_err());
    }

    #[test]
    fn test_shard_discovery_default() {
        let discovery = ShardDiscovery::default();
        assert!(matches!(discovery, ShardDiscovery::Static(_)));
    }

    #[test]
    fn test_rebalance_config_defaults() {
        let config = RebalanceConfig::new();
        assert!((config.imbalance_threshold - 0.3).abs() < 0.001);
        assert_eq!(config.batch_size, 10_000);
        assert!(config.auto_rebalance);
    }

    #[test]
    fn test_rebalance_config_builders() {
        let config = RebalanceConfig::new()
            .with_imbalance_threshold(0.5)
            .with_batch_size(5000)
            .with_auto_rebalance(false);

        assert!((config.imbalance_threshold - 0.5).abs() < 0.001);
        assert_eq!(config.batch_size, 5000);
        assert!(!config.auto_rebalance);
    }

    #[test]
    fn test_rebalance_config_threshold_clamping() {
        let config = RebalanceConfig::new().with_imbalance_threshold(1.5);
        assert!((config.imbalance_threshold - 1.0).abs() < 0.001);

        let config = RebalanceConfig::new().with_imbalance_threshold(-0.5);
        assert!((config.imbalance_threshold - 0.0).abs() < 0.001);
    }

    #[test]
    fn test_rebalance_config_should_rebalance() {
        let config = RebalanceConfig::new().with_imbalance_threshold(0.3);

        // Below threshold
        assert!(!config.should_rebalance(0.2));

        // Above threshold
        assert!(config.should_rebalance(0.4));

        // Disabled auto-rebalance
        let disabled = config.with_auto_rebalance(false);
        assert!(!disabled.should_rebalance(0.5));
    }
}
