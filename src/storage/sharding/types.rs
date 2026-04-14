//! Core types for the sharding system.

use crate::core::error::StorageError;
use crate::core::id::{EdgeId, NodeId};
use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

/// Maximum valid shard ID value.
/// Allows for up to 65535 shards, which is sufficient for most deployments.
pub const MAX_SHARD_ID: u16 = u16::MAX - 1;

/// Unique identifier for a shard.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ShardId(u16);

impl ShardId {
    /// Create a new ShardId with validation.
    ///
    /// Returns an error if the ID exceeds MAX_SHARD_ID.
    #[inline]
    pub fn new(id: u16) -> Result<Self, StorageError> {
        if id > MAX_SHARD_ID {
            return Err(StorageError::InvalidId {
                id: id as u64,
                id_type: "shard",
            });
        }
        Ok(ShardId(id))
    }

    /// Create a new ShardId without validation (for internal use only).
    #[inline]
    pub(crate) const fn new_unchecked(id: u16) -> Self {
        ShardId(id)
    }

    /// Get the inner u16 value.
    #[inline]
    pub const fn as_u16(self) -> u16 {
        self.0
    }

    /// Get the value as u64 for compatibility with other ID types.
    #[inline]
    pub const fn as_u64(self) -> u64 {
        self.0 as u64
    }
}

impl fmt::Display for ShardId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Shard({})", self.0)
    }
}

impl TryFrom<u16> for ShardId {
    type Error = StorageError;

    fn try_from(id: u16) -> Result<Self, Self::Error> {
        ShardId::new(id)
    }
}

/// Reference to a node on a remote shard.
///
/// Used for cross-shard edge replication where we need to track
/// the location of the remote endpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RemoteNodeRef {
    /// The node ID on the remote shard.
    pub node_id: NodeId,
    /// The shard where this node resides.
    pub shard_id: ShardId,
}

impl RemoteNodeRef {
    /// Create a new remote node reference.
    pub fn new(node_id: NodeId, shard_id: ShardId) -> Self {
        Self { node_id, shard_id }
    }
}

impl fmt::Display for RemoteNodeRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}@{}", self.node_id, self.shard_id)
    }
}

/// Reference to an edge that crosses shard boundaries.
///
/// Cross-shard edges are replicated on both the source and target shards.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RemoteEdgeRef {
    /// The edge ID.
    pub edge_id: EdgeId,
    /// The shard where this edge's source node resides.
    pub source_shard: ShardId,
    /// The shard where this edge's target node resides.
    pub target_shard: ShardId,
}

impl RemoteEdgeRef {
    /// Create a new remote edge reference.
    pub fn new(edge_id: EdgeId, source_shard: ShardId, target_shard: ShardId) -> Self {
        Self {
            edge_id,
            source_shard,
            target_shard,
        }
    }

    /// Returns true if this edge crosses shard boundaries.
    pub fn is_cross_shard(&self) -> bool {
        self.source_shard != self.target_shard
    }
}

impl fmt::Display for RemoteEdgeRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.is_cross_shard() {
            write!(
                f,
                "{}({}->{})",
                self.edge_id, self.source_shard, self.target_shard
            )
        } else {
            write!(f, "{}@{}", self.edge_id, self.source_shard)
        }
    }
}

/// Status of a shard in the cluster.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShardStatus {
    /// Shard is healthy and accepting requests.
    Healthy,
    /// Shard is degraded but still accepting requests.
    Degraded,
    /// Shard is unavailable (connection failed or timeout).
    Unavailable,
    /// Shard is in the process of being added to the cluster.
    Joining,
    /// Shard is being removed from the cluster (draining data).
    Leaving,
    /// Shard is being rebalanced (migrating data).
    Rebalancing,
}

impl fmt::Display for ShardStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ShardStatus::Healthy => write!(f, "Healthy"),
            ShardStatus::Degraded => write!(f, "Degraded"),
            ShardStatus::Unavailable => write!(f, "Unavailable"),
            ShardStatus::Joining => write!(f, "Joining"),
            ShardStatus::Leaving => write!(f, "Leaving"),
            ShardStatus::Rebalancing => write!(f, "Rebalancing"),
        }
    }
}

/// Current state of a shard including health metrics.
#[derive(Debug, Clone)]
pub struct ShardState {
    /// The shard identifier.
    pub shard_id: ShardId,
    /// Current status.
    pub status: ShardStatus,
    /// Number of nodes stored on this shard.
    pub node_count: u64,
    /// Number of edges stored on this shard (including replicated cross-shard edges).
    pub edge_count: u64,
    /// Number of cross-shard edges (edges that connect to nodes on other shards).
    pub cross_shard_edge_count: u64,
    /// Estimated storage size in bytes.
    pub storage_bytes: u64,
    /// Last successful heartbeat time.
    pub last_heartbeat: Option<Instant>,
}

impl ShardState {
    /// Create a new shard state.
    pub fn new(shard_id: ShardId) -> Self {
        Self {
            shard_id,
            status: ShardStatus::Healthy,
            node_count: 0,
            edge_count: 0,
            cross_shard_edge_count: 0,
            storage_bytes: 0,
            last_heartbeat: None,
        }
    }

    /// Calculate the cross-shard edge ratio.
    ///
    /// Returns the proportion of edges that cross shard boundaries.
    /// A lower ratio indicates better partitioning.
    pub fn cross_shard_ratio(&self) -> f64 {
        if self.edge_count == 0 {
            0.0
        } else {
            self.cross_shard_edge_count as f64 / self.edge_count as f64
        }
    }
}

/// Metrics for monitoring shard performance.
#[derive(Debug, Default)]
pub struct ShardMetrics {
    /// Total queries routed to this shard.
    pub queries_total: AtomicU64,
    /// Queries that required cross-shard coordination.
    pub cross_shard_queries: AtomicU64,
    /// Total write operations.
    pub writes_total: AtomicU64,
    /// Writes that required distributed transactions.
    pub distributed_writes: AtomicU64,
    /// Total bytes transferred to/from this shard.
    pub bytes_transferred: AtomicU64,
    /// Query latency sum in microseconds (for averaging).
    pub query_latency_sum_us: AtomicU64,
    /// Query latency count (for averaging).
    pub query_latency_count: AtomicU64,
}

impl ShardMetrics {
    /// Create new shard metrics.
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a query.
    pub fn record_query(&self, cross_shard: bool) {
        self.queries_total.fetch_add(1, Ordering::Relaxed);
        if cross_shard {
            self.cross_shard_queries.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Record a write operation.
    pub fn record_write(&self, distributed: bool) {
        self.writes_total.fetch_add(1, Ordering::Relaxed);
        if distributed {
            self.distributed_writes.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Record query latency in microseconds.
    pub fn record_latency(&self, latency_us: u64) {
        self.query_latency_sum_us
            .fetch_add(latency_us, Ordering::Relaxed);
        self.query_latency_count.fetch_add(1, Ordering::Relaxed);
    }

    /// Get average query latency in microseconds.
    pub fn average_latency_us(&self) -> f64 {
        let count = self.query_latency_count.load(Ordering::Relaxed);
        if count == 0 {
            0.0
        } else {
            self.query_latency_sum_us.load(Ordering::Relaxed) as f64 / count as f64
        }
    }

    /// Get the cross-shard query ratio.
    pub fn cross_shard_query_ratio(&self) -> f64 {
        let total = self.queries_total.load(Ordering::Relaxed);
        if total == 0 {
            0.0
        } else {
            self.cross_shard_queries.load(Ordering::Relaxed) as f64 / total as f64
        }
    }

    /// Get the distributed write ratio.
    pub fn distributed_write_ratio(&self) -> f64 {
        let total = self.writes_total.load(Ordering::Relaxed);
        if total == 0 {
            0.0
        } else {
            self.distributed_writes.load(Ordering::Relaxed) as f64 / total as f64
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_shard_id_creation() {
        let id = ShardId::new(42).unwrap();
        assert_eq!(id.as_u16(), 42);
        assert_eq!(id.as_u64(), 42);
    }

    #[test]
    fn test_shard_id_validation() {
        // Valid IDs should be accepted
        assert!(ShardId::new(0).is_ok());
        assert!(ShardId::new(1000).is_ok());
        assert!(ShardId::new(MAX_SHARD_ID).is_ok());

        // MAX_SHARD_ID + 1 should be rejected
        let result = ShardId::new(MAX_SHARD_ID + 1);
        assert!(result.is_err());
        if let Err(StorageError::InvalidId { id_type, .. }) = result {
            assert_eq!(id_type, "shard");
        } else {
            panic!("Expected InvalidId error");
        }
    }

    #[test]
    fn test_shard_id_display() {
        let id = ShardId::new(5).unwrap();
        assert_eq!(format!("{}", id), "Shard(5)");
    }

    #[test]
    fn test_shard_id_try_from_u16() {
        let id: ShardId = 42u16.try_into().unwrap();
        assert_eq!(id.as_u16(), 42);

        // Invalid ID should fail
        let result: Result<ShardId, _> = (MAX_SHARD_ID + 1).try_into();
        assert!(result.is_err());
    }

    #[test]
    fn test_remote_node_ref() {
        let node_id = NodeId::new(100).unwrap();
        let shard_id = ShardId::new(1).unwrap();
        let remote = RemoteNodeRef::new(node_id, shard_id);

        assert_eq!(remote.node_id, node_id);
        assert_eq!(remote.shard_id, shard_id);
        assert_eq!(format!("{}", remote), "Node(100)@Shard(1)");
    }

    #[test]
    fn test_remote_edge_ref() {
        let edge_id = EdgeId::new(50).unwrap();
        let shard0 = ShardId::new(0).unwrap();
        let shard1 = ShardId::new(1).unwrap();

        // Cross-shard edge
        let cross = RemoteEdgeRef::new(edge_id, shard0, shard1);
        assert!(cross.is_cross_shard());
        assert_eq!(format!("{}", cross), "Edge(50)(Shard(0)->Shard(1))");

        // Same-shard edge
        let same = RemoteEdgeRef::new(edge_id, shard0, shard0);
        assert!(!same.is_cross_shard());
        assert_eq!(format!("{}", same), "Edge(50)@Shard(0)");
    }

    #[test]
    fn test_shard_status_display() {
        assert_eq!(format!("{}", ShardStatus::Healthy), "Healthy");
        assert_eq!(format!("{}", ShardStatus::Degraded), "Degraded");
        assert_eq!(format!("{}", ShardStatus::Unavailable), "Unavailable");
        assert_eq!(format!("{}", ShardStatus::Joining), "Joining");
        assert_eq!(format!("{}", ShardStatus::Leaving), "Leaving");
        assert_eq!(format!("{}", ShardStatus::Rebalancing), "Rebalancing");
    }

    #[test]
    fn test_shard_state() {
        let shard_id = ShardId::new(0).unwrap();
        let mut state = ShardState::new(shard_id);

        assert_eq!(state.shard_id, shard_id);
        assert_eq!(state.status, ShardStatus::Healthy);
        assert_eq!(state.node_count, 0);
        assert_eq!(state.edge_count, 0);
        assert_eq!(state.cross_shard_edge_count, 0);

        // Test cross-shard ratio
        assert_eq!(state.cross_shard_ratio(), 0.0);

        state.edge_count = 100;
        state.cross_shard_edge_count = 30;
        assert!((state.cross_shard_ratio() - 0.3).abs() < 0.001);
    }

    #[test]
    fn test_shard_metrics() {
        let metrics = ShardMetrics::new();

        // Record queries
        metrics.record_query(false);
        metrics.record_query(true);
        metrics.record_query(false);

        assert_eq!(metrics.queries_total.load(Ordering::Relaxed), 3);
        assert_eq!(metrics.cross_shard_queries.load(Ordering::Relaxed), 1);
        assert!((metrics.cross_shard_query_ratio() - 0.333).abs() < 0.01);

        // Record writes
        metrics.record_write(false);
        metrics.record_write(true);

        assert_eq!(metrics.writes_total.load(Ordering::Relaxed), 2);
        assert_eq!(metrics.distributed_writes.load(Ordering::Relaxed), 1);
        assert!((metrics.distributed_write_ratio() - 0.5).abs() < 0.001);

        // Record latency
        metrics.record_latency(100);
        metrics.record_latency(200);
        metrics.record_latency(300);

        assert!((metrics.average_latency_us() - 200.0).abs() < 0.001);
    }

    #[test]
    fn test_shard_metrics_default_ratios() {
        let metrics = ShardMetrics::new();

        // With no data, ratios should be 0
        assert_eq!(metrics.cross_shard_query_ratio(), 0.0);
        assert_eq!(metrics.distributed_write_ratio(), 0.0);
        assert_eq!(metrics.average_latency_us(), 0.0);
    }
}
