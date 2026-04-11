//! Sharding support for horizontal scalability.
//!
//! This module implements graph sharding for distributing data across multiple machines
//! when the dataset exceeds single-machine capacity. It uses domain-based partitioning
//! with edge replication as described in ADR-0014.
//!
//! # Architecture
//!
//! ```text
//! ┌─────────────────────────────────────────────────────────────────┐
//! │                      Shard Coordinator                           │
//! │   • Query routing         • Transaction coordination             │
//! │   • Shard discovery       • Rebalancing orchestration           │
//! └─────────────────────────────────────────────────────────────────┘
//!               │                    │                    │
//!               ▼                    ▼                    ▼
//!      ┌─────────────┐      ┌─────────────┐      ┌─────────────┐
//!      │   Shard 0   │      │   Shard 1   │      │   Shard 2   │
//!      │   People    │◄────►│   Places    │◄────►│   Events    │
//!      └─────────────┘      └─────────────┘      └─────────────┘
//! ```
//!
//! # Key Components
//!
//! - [`ShardId`]: Strongly-typed shard identifier
//! - [`ShardConfig`]: Configuration for shard topology
//! - [`ShardRouter`]: Routes queries to appropriate shards
//! - [`ShardCoordinator`]: Coordinates distributed operations
//! - [`DistributedTransaction`]: Two-phase commit for cross-shard writes
//! - [`RebalanceManager`]: Handles shard rebalancing
//!
//! # Example
//!
//! ```rust,no_run
//! use aletheiadb::storage::sharding::{ShardConfig, ShardCoordinator, ShardDefinition};
//!
//! # fn main() {
//! let config = ShardConfig::new(vec![
//!     ShardDefinition::new(0, "shard0:9000", vec!["Person", "User"]),
//!     ShardDefinition::new(1, "shard1:9000", vec!["Place", "Location"]),
//! ]);
//!
//! let coordinator = ShardCoordinator::new(config);
//! # }
//! ```

pub mod config;
pub mod coordinator;
pub mod executor;
pub mod migration;
pub mod network;
pub mod persistent_commit_log;
pub mod rebalance;
pub mod router;
pub mod rpc_client;
pub mod simulation;
pub mod transaction;
pub mod types;

// Re-export commonly used types
pub use config::{ShardConfig, ShardDefinition, ShardDiscovery, ShardRebalanceConfig};
pub use coordinator::{
    DeadLetteredTransaction, ShardConnection, ShardCoordinator, ShardRecoveryResult,
};
pub use executor::{
    AggregationStrategy, DistributedQuery, ExecutorConfig, ExecutorError, ExecutorResult,
    ShardExecutorStats, ShardQueryResult, ShardResult, ShardedQueryExecutor,
};
pub use migration::{
    DualWriteRouter, MigrationConfig, MigrationError, MigrationExecutor, MigrationResult,
    RoutingToken, ShardMigrationStats,
};
pub use network::{
    CircuitBreaker, CircuitBreakerConfig, CircuitState, ConnectionPool, MigrationBatch,
    MigrationResponse, MockShardClient, NetworkError, NetworkResult, PoolConfig, PoolStats,
    ShardClient,
};
pub use persistent_commit_log::{
    CommitLogConfig, CommitLogEntry, CommitLogError, CommitLogResult, CommitLogStats, EntryType,
    PersistentCommitLog,
};
pub use rebalance::{MigrationPlan, MigrationState, RebalanceManager, ShardMigrationProgress};
pub use router::{ShardRouter, TraversalPlan, TraversalStep};
pub use rpc_client::{ClientStats, HttpShardClient, RpcConfig};
pub use simulation::{EdgeCutAnalysis, ShardingSimulation, SimulationResult};
#[allow(deprecated)]
pub use transaction::TwoPhaseCommitLog;
pub use transaction::{DistributedTransaction, ParticipantState, TransactionPhase};
pub use types::{RemoteEdgeRef, RemoteNodeRef, ShardId, ShardMetrics, ShardState, ShardStatus};
