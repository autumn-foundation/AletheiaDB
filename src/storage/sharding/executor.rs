//! Distributed query executor with scatter-gather.
//!
//! This module implements the execution engine for distributed queries
//! across multiple shards. It handles:
//!
//! - Query distribution (scatter)
//! - Result aggregation (gather)
//! - Timeout handling
//! - Partial failure handling
//!
//! # Architecture
//!
//! ```text
//! ┌──────────────────────────────────────────────────────────────────┐
//! │                     Query Executor                                │
//! │  ┌────────────────────────────────────────────────────────────┐  │
//! │  │  1. Route query to shards (from ShardRouter)              │  │
//! │  │  2. Scatter: Send query to all relevant shards            │  │
//! │  │  3. Wait for responses (with timeout)                      │  │
//! │  │  4. Gather: Aggregate results from all shards             │  │
//! │  │  5. Return combined result                                 │  │
//! │  └────────────────────────────────────────────────────────────┘  │
//! └──────────────────────────────────────────────────────────────────┘
//! ```
//!
//! # ⚠️ Performance Warning: Sequential Execution
//!
//! Currently, the "Scatter" phase is implemented as a **Sequential Send-and-Wait** loop.
//!
//! - **Latency**: Scales linearly with the number of target shards ($O(N)$), not constant ($O(1)$).
//!   Total latency ≈ $\sum \text{latency}(\text{shard}_i)$.
//! - **Throughput**: Limited by the single-threaded dispatch loop.
//!
//! This implementation is suitable for small clusters or low-latency local shards but
//! may become a bottleneck in large distributed deployments. Future versions will
//! implement true parallel scatter-gather using async/await or thread pools.

use super::network::{NetworkError, NetworkResult, ShardClient};
use super::router::{ShardRouter, TraversalPlan};
use super::types::ShardId;
use crate::core::id::NodeId;
use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// Error types for query execution.
#[derive(Debug, Clone)]
#[allow(missing_docs)]
pub enum ExecutorError {
    /// All target shards failed.
    AllShardsFailed {
        query_id: u64,
        failures: Vec<(ShardId, NetworkError)>,
    },
    /// Query timed out.
    Timeout {
        query_id: u64,
        timeout: Duration,
        responded: Vec<ShardId>,
        pending: Vec<ShardId>,
    },
    /// Partial failure (some shards succeeded, some failed).
    PartialFailure {
        query_id: u64,
        successes: Vec<ShardId>,
        failures: Vec<(ShardId, NetworkError)>,
    },
    /// No shards available for query.
    NoShardsAvailable,
    /// Invalid query.
    InvalidQuery(String),
    /// Aggregation error.
    AggregationError(String),
}

impl fmt::Display for ExecutorError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ExecutorError::AllShardsFailed { query_id, failures } => {
                write!(
                    f,
                    "Query {} failed on all {} shards",
                    query_id,
                    failures.len()
                )
            }
            ExecutorError::Timeout {
                query_id,
                timeout,
                responded,
                pending,
            } => {
                write!(
                    f,
                    "Query {} timed out after {:?} ({} responded, {} pending)",
                    query_id,
                    timeout,
                    responded.len(),
                    pending.len()
                )
            }
            ExecutorError::PartialFailure {
                query_id,
                successes,
                failures,
            } => {
                write!(
                    f,
                    "Query {} partially failed ({} succeeded, {} failed)",
                    query_id,
                    successes.len(),
                    failures.len()
                )
            }
            ExecutorError::NoShardsAvailable => {
                write!(f, "No shards available for query")
            }
            ExecutorError::InvalidQuery(msg) => {
                write!(f, "Invalid query: {}", msg)
            }
            ExecutorError::AggregationError(msg) => {
                write!(f, "Aggregation error: {}", msg)
            }
        }
    }
}

impl std::error::Error for ExecutorError {}

/// Result type for query execution.
pub type ExecutorResult<T> = Result<T, ExecutorError>;

/// Configuration for the query executor.
#[derive(Debug, Clone)]
pub struct ExecutorConfig {
    /// Default timeout for queries.
    pub default_timeout: Duration,
    /// Maximum concurrent queries per shard.
    pub max_concurrent_per_shard: usize,
    /// Whether to allow partial results on timeout.
    pub allow_partial_results: bool,
    /// Retry failed shards.
    pub retry_failed_shards: bool,
    /// Maximum retries per shard.
    pub max_retries: usize,
}

impl Default for ExecutorConfig {
    fn default() -> Self {
        Self {
            default_timeout: Duration::from_secs(30),
            max_concurrent_per_shard: 100,
            allow_partial_results: false,
            retry_failed_shards: true,
            max_retries: 2,
        }
    }
}

/// A query to be executed across shards.
#[derive(Debug, Clone)]
pub struct DistributedQuery {
    /// Unique query ID.
    pub id: u64,
    /// Serialized query data.
    pub data: Vec<u8>,
    /// Target shards (None = all shards from router).
    pub target_shards: Option<Vec<ShardId>>,
    /// Query timeout (overrides default).
    pub timeout: Option<Duration>,
    /// Aggregation strategy.
    pub aggregation: AggregationStrategy,
}

impl DistributedQuery {
    /// Create a new distributed query.
    pub fn new(id: u64, data: Vec<u8>) -> Self {
        Self {
            id,
            data,
            target_shards: None,
            timeout: None,
            aggregation: AggregationStrategy::Concat,
        }
    }

    /// Set target shards.
    pub fn with_shards(mut self, shards: Vec<ShardId>) -> Self {
        self.target_shards = Some(shards);
        self
    }

    /// Set timeout.
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }

    /// Set aggregation strategy.
    pub fn with_aggregation(mut self, strategy: AggregationStrategy) -> Self {
        self.aggregation = strategy;
        self
    }
}

/// Strategy for aggregating results from multiple shards.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AggregationStrategy {
    /// Concatenate all results.
    Concat,
    /// Return first non-empty result.
    First,
    /// Merge and deduplicate by node ID.
    MergeNodes,
    /// Sum numeric results.
    Sum,
    /// Count total results.
    Count,
    /// Return all results separately by shard.
    ByShard,
}

/// Result from a single shard.
#[derive(Debug, Clone)]
pub struct ShardResult {
    /// Shard ID.
    pub shard_id: ShardId,
    /// Result data.
    pub data: Vec<u8>,
    /// Execution time on this shard.
    pub execution_time: Duration,
    /// Number of results from this shard.
    pub result_count: usize,
}

/// Aggregated result from all shards.
#[derive(Debug, Clone)]
pub struct QueryResult {
    /// Query ID.
    pub query_id: u64,
    /// Aggregated data.
    pub data: Vec<u8>,
    /// Individual shard results.
    pub shard_results: Vec<ShardResult>,
    /// Total execution time.
    pub total_time: Duration,
    /// Number of shards queried.
    pub shards_queried: usize,
    /// Number of shards that succeeded.
    pub shards_succeeded: usize,
    /// Total result count.
    pub total_results: usize,
}

impl QueryResult {
    /// Check if all shards succeeded.
    pub fn is_complete(&self) -> bool {
        self.shards_queried == self.shards_succeeded
    }

    /// Get the success rate.
    pub fn success_rate(&self) -> f64 {
        if self.shards_queried == 0 {
            1.0
        } else {
            self.shards_succeeded as f64 / self.shards_queried as f64
        }
    }
}

/// Distributed query executor.
#[derive(Debug)]
pub struct QueryExecutor<C: ShardClient> {
    /// Configuration.
    config: ExecutorConfig,
    /// Shard clients indexed by shard ID.
    clients: HashMap<ShardId, Arc<C>>,
    /// Router for query routing.
    router: ShardRouter,
    /// Query ID generator.
    next_query_id: AtomicU64,
    /// Total queries executed.
    queries_executed: AtomicU64,
    /// Total queries failed.
    queries_failed: AtomicU64,
}

impl<C: ShardClient> QueryExecutor<C> {
    /// Create a new query executor.
    pub fn new(config: ExecutorConfig, router: ShardRouter) -> Self {
        Self {
            config,
            clients: HashMap::new(),
            router,
            next_query_id: AtomicU64::new(1),
            queries_executed: AtomicU64::new(0),
            queries_failed: AtomicU64::new(0),
        }
    }

    /// Register a shard client.
    pub fn register_client(&mut self, shard_id: ShardId, client: Arc<C>) {
        self.clients.insert(shard_id, client);
    }

    /// Remove a shard client.
    pub fn unregister_client(&mut self, shard_id: ShardId) {
        self.clients.remove(&shard_id);
    }

    /// Generate a new query ID.
    pub fn next_query_id(&self) -> u64 {
        self.next_query_id.fetch_add(1, Ordering::SeqCst)
    }

    /// Execute a query across shards.
    ///
    /// This method iterates through the target shards **sequentially** and collects
    /// results.
    ///
    /// # Partial Results and Timeouts
    ///
    /// If `allow_partial_results` is enabled and the query times out:
    /// - Results collected *so far* are returned.
    /// - Shards that have not yet been contacted (due to being later in the iteration order)
    ///   are marked as `pending` in the timeout error (or silently omitted if results are returned).
    ///
    /// This means the order of `target_shards` matters: earlier shards are prioritized.
    pub fn execute(&self, query: DistributedQuery) -> ExecutorResult<QueryResult> {
        let start = Instant::now();
        let timeout = query.timeout.unwrap_or(self.config.default_timeout);

        // Determine target shards
        let target_shards = match &query.target_shards {
            Some(shards) => shards.clone(),
            None => self.clients.keys().copied().collect(),
        };

        if target_shards.is_empty() {
            return Err(ExecutorError::NoShardsAvailable);
        }

        // Scatter: Execute query on all target shards
        let mut results: Vec<ShardResult> = Vec::new();
        let mut failures: Vec<(ShardId, NetworkError)> = Vec::new();

        for shard_id in &target_shards {
            if start.elapsed() >= timeout {
                // Timed out before sending to all shards
                let pending: Vec<_> = target_shards
                    .iter()
                    .filter(|s| !results.iter().any(|r| r.shard_id == **s))
                    .copied()
                    .collect();

                if self.config.allow_partial_results && !results.is_empty() {
                    break;
                }

                return Err(ExecutorError::Timeout {
                    query_id: query.id,
                    timeout,
                    responded: results.iter().map(|r| r.shard_id).collect(),
                    pending,
                });
            }

            let result = self.execute_on_shard(*shard_id, &query);

            match result {
                Ok(shard_result) => {
                    results.push(shard_result);
                }
                Err(err) => {
                    failures.push((*shard_id, err));
                }
            }
        }

        // Check for complete failure
        if results.is_empty() {
            self.queries_failed.fetch_add(1, Ordering::Relaxed);
            return Err(ExecutorError::AllShardsFailed {
                query_id: query.id,
                failures,
            });
        }

        // Check for partial failure
        if !failures.is_empty() && !self.config.allow_partial_results {
            self.queries_failed.fetch_add(1, Ordering::Relaxed);
            return Err(ExecutorError::PartialFailure {
                query_id: query.id,
                successes: results.iter().map(|r| r.shard_id).collect(),
                failures,
            });
        }

        // Gather: Aggregate results
        let aggregated = self.aggregate_results(&query, &results)?;

        let total_results: usize = results.iter().map(|r| r.result_count).sum();
        let total_time = start.elapsed();

        self.queries_executed.fetch_add(1, Ordering::Relaxed);

        Ok(QueryResult {
            query_id: query.id,
            data: aggregated,
            shard_results: results.clone(),
            total_time,
            shards_queried: target_shards.len(),
            shards_succeeded: results.len(),
            total_results,
        })
    }

    /// Execute a graph traversal across shards.
    pub fn execute_traversal(
        &self,
        _start_node: NodeId,
        start_label: &str,
        target_labels: &[&str],
    ) -> ExecutorResult<QueryResult> {
        let plan = self.router.route_traversal(start_label, target_labels);

        // Build query from traversal plan
        let query_id = self.next_query_id();
        let query_data = self.serialize_traversal_plan(&plan);

        let target_shards: Vec<_> = plan
            .steps
            .iter()
            .map(|s| s.shard_id)
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .collect();

        let query = DistributedQuery::new(query_id, query_data)
            .with_shards(target_shards)
            .with_aggregation(AggregationStrategy::MergeNodes);

        self.execute(query)
    }

    /// Get executor statistics.
    pub fn stats(&self) -> ExecutorStats {
        ExecutorStats {
            queries_executed: self.queries_executed.load(Ordering::Relaxed),
            queries_failed: self.queries_failed.load(Ordering::Relaxed),
            registered_clients: self.clients.len(),
        }
    }

    // ==================== Private Methods ====================

    fn execute_on_shard(
        &self,
        shard_id: ShardId,
        query: &DistributedQuery,
    ) -> NetworkResult<ShardResult> {
        let client = self
            .clients
            .get(&shard_id)
            .ok_or(NetworkError::ShardUnavailable(shard_id))?;

        let start = Instant::now();
        let data = client.query(query.id, &query.data)?;
        let execution_time = start.elapsed();

        // Parse result count from data (assume first 4 bytes = count)
        let result_count = if data.len() >= 4 {
            u32::from_le_bytes([data[0], data[1], data[2], data[3]]) as usize
        } else {
            0
        };

        Ok(ShardResult {
            shard_id,
            data,
            execution_time,
            result_count,
        })
    }

    fn aggregate_results(
        &self,
        query: &DistributedQuery,
        results: &[ShardResult],
    ) -> ExecutorResult<Vec<u8>> {
        match query.aggregation {
            AggregationStrategy::Concat => {
                // Simple concatenation
                let mut aggregated = Vec::new();
                for result in results {
                    aggregated.extend(&result.data);
                }
                Ok(aggregated)
            }
            AggregationStrategy::First => {
                // Return first non-empty result
                for result in results {
                    if !result.data.is_empty() {
                        return Ok(result.data.clone());
                    }
                }
                Ok(Vec::new())
            }
            AggregationStrategy::MergeNodes => {
                // Merge and deduplicate (simplified - real impl would parse nodes)
                let mut aggregated = Vec::new();
                let mut seen_len = 0;
                for result in results {
                    if result.data.len() > seen_len {
                        aggregated = result.data.clone();
                        seen_len = result.data.len();
                    }
                }
                Ok(aggregated)
            }
            AggregationStrategy::Sum => {
                // Sum numeric results
                let mut total: u64 = 0;
                for result in results {
                    if result.data.len() >= 8 {
                        let value = u64::from_le_bytes([
                            result.data[0],
                            result.data[1],
                            result.data[2],
                            result.data[3],
                            result.data[4],
                            result.data[5],
                            result.data[6],
                            result.data[7],
                        ]);
                        total += value;
                    }
                }
                Ok(total.to_le_bytes().to_vec())
            }
            AggregationStrategy::Count => {
                // Count total results
                let total: usize = results.iter().map(|r| r.result_count).sum();
                Ok((total as u64).to_le_bytes().to_vec())
            }
            AggregationStrategy::ByShard => {
                // Return results separately (serialize shard -> data mapping)
                let mut aggregated = Vec::new();
                aggregated.extend_from_slice(&(results.len() as u32).to_le_bytes());
                for result in results {
                    aggregated.extend_from_slice(&result.shard_id.as_u16().to_le_bytes());
                    aggregated.extend_from_slice(&(result.data.len() as u32).to_le_bytes());
                    aggregated.extend(&result.data);
                }
                Ok(aggregated)
            }
        }
    }

    fn serialize_traversal_plan(&self, plan: &TraversalPlan) -> Vec<u8> {
        // Simplified serialization
        let mut data = Vec::new();
        data.extend_from_slice(&(plan.steps.len() as u32).to_le_bytes());
        for step in &plan.steps {
            data.extend_from_slice(&step.shard_id.as_u16().to_le_bytes());
            data.extend_from_slice(&(step.edge_labels.len() as u32).to_le_bytes());
            for label in &step.edge_labels {
                data.extend_from_slice(&(label.len() as u32).to_le_bytes());
                data.extend_from_slice(label.as_bytes());
            }
            data.push(if step.may_cross_shard { 1 } else { 0 });
        }
        data
    }
}

/// Statistics for the query executor.
#[derive(Debug, Clone)]
pub struct ExecutorStats {
    /// Total queries executed.
    pub queries_executed: u64,
    /// Total queries failed.
    pub queries_failed: u64,
    /// Number of registered clients.
    pub registered_clients: usize,
}

impl ExecutorStats {
    /// Calculate success rate.
    pub fn success_rate(&self) -> f64 {
        let total = self.queries_executed + self.queries_failed;
        if total == 0 {
            1.0
        } else {
            self.queries_executed as f64 / total as f64
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::sharding::config::{ShardConfig, ShardDefinition};
    use crate::storage::sharding::network::MockShardClient;

    fn make_shard_id(id: u16) -> ShardId {
        ShardId::new(id).unwrap()
    }

    fn test_config() -> ShardConfig {
        ShardConfig::new(vec![
            ShardDefinition::new(0, "shard0:9000", vec!["Person", "User", "Account"]),
            ShardDefinition::new(1, "shard1:9000", vec!["Place", "Location", "Address"]),
            ShardDefinition::new(2, "shard2:9000", vec!["Event", "Transaction", "Activity"]),
        ])
    }

    fn test_router() -> ShardRouter {
        ShardRouter::new(test_config())
    }

    // ==================== ExecutorError Tests ====================

    #[test]
    fn test_executor_error_display() {
        let err = ExecutorError::NoShardsAvailable;
        assert!(format!("{}", err).contains("No shards"));

        let err = ExecutorError::InvalidQuery("bad".into());
        assert!(format!("{}", err).contains("Invalid"));

        let err = ExecutorError::AllShardsFailed {
            query_id: 1,
            failures: vec![],
        };
        assert!(format!("{}", err).contains("failed on all"));
    }

    // ==================== DistributedQuery Tests ====================

    #[test]
    fn test_distributed_query_creation() {
        let query = DistributedQuery::new(1, vec![1, 2, 3]);
        assert_eq!(query.id, 1);
        assert_eq!(query.data, vec![1, 2, 3]);
        assert!(query.target_shards.is_none());
    }

    #[test]
    fn test_distributed_query_builders() {
        let query = DistributedQuery::new(1, vec![])
            .with_shards(vec![make_shard_id(0), make_shard_id(1)])
            .with_timeout(Duration::from_secs(10))
            .with_aggregation(AggregationStrategy::Sum);

        assert_eq!(query.target_shards.unwrap().len(), 2);
        assert_eq!(query.timeout.unwrap(), Duration::from_secs(10));
        assert_eq!(query.aggregation, AggregationStrategy::Sum);
    }

    // ==================== QueryResult Tests ====================

    #[test]
    fn test_query_result_complete() {
        let result = QueryResult {
            query_id: 1,
            data: vec![],
            shard_results: vec![],
            total_time: Duration::from_millis(100),
            shards_queried: 3,
            shards_succeeded: 3,
            total_results: 10,
        };

        assert!(result.is_complete());
        assert!((result.success_rate() - 1.0).abs() < 0.001);
    }

    #[test]
    fn test_query_result_partial() {
        let result = QueryResult {
            query_id: 1,
            data: vec![],
            shard_results: vec![],
            total_time: Duration::from_millis(100),
            shards_queried: 3,
            shards_succeeded: 2,
            total_results: 5,
        };

        assert!(!result.is_complete());
        assert!((result.success_rate() - 0.666).abs() < 0.01);
    }

    // ==================== QueryExecutor Tests ====================

    #[test]
    fn test_executor_creation() {
        let executor: QueryExecutor<MockShardClient> =
            QueryExecutor::new(ExecutorConfig::default(), test_router());

        assert_eq!(executor.stats().registered_clients, 0);
        assert_eq!(executor.stats().queries_executed, 0);
    }

    #[test]
    fn test_executor_register_client() {
        let mut executor: QueryExecutor<MockShardClient> =
            QueryExecutor::new(ExecutorConfig::default(), test_router());

        let client = Arc::new(MockShardClient::new(make_shard_id(0)));
        executor.register_client(make_shard_id(0), client);

        assert_eq!(executor.stats().registered_clients, 1);
    }

    #[test]
    fn test_executor_unregister_client() {
        let mut executor: QueryExecutor<MockShardClient> =
            QueryExecutor::new(ExecutorConfig::default(), test_router());

        let client = Arc::new(MockShardClient::new(make_shard_id(0)));
        executor.register_client(make_shard_id(0), client);
        executor.unregister_client(make_shard_id(0));

        assert_eq!(executor.stats().registered_clients, 0);
    }

    #[test]
    fn test_executor_no_shards() {
        let executor: QueryExecutor<MockShardClient> =
            QueryExecutor::new(ExecutorConfig::default(), test_router());

        let query = DistributedQuery::new(1, vec![]);
        let result = executor.execute(query);

        assert!(matches!(result, Err(ExecutorError::NoShardsAvailable)));
    }

    #[test]
    fn test_executor_single_shard_success() {
        let mut executor: QueryExecutor<MockShardClient> =
            QueryExecutor::new(ExecutorConfig::default(), test_router());

        let client = Arc::new(MockShardClient::new(make_shard_id(0)));
        executor.register_client(make_shard_id(0), client);

        let query = DistributedQuery::new(1, vec![1, 2, 3]).with_shards(vec![make_shard_id(0)]);

        let result = executor.execute(query).unwrap();
        assert_eq!(result.shards_queried, 1);
        assert_eq!(result.shards_succeeded, 1);
        assert!(result.is_complete());
    }

    #[test]
    fn test_executor_multiple_shards_success() {
        let mut executor: QueryExecutor<MockShardClient> =
            QueryExecutor::new(ExecutorConfig::default(), test_router());

        for i in 0..3 {
            let client = Arc::new(MockShardClient::new(make_shard_id(i)));
            executor.register_client(make_shard_id(i), client);
        }

        let query = DistributedQuery::new(1, vec![]).with_shards(vec![
            make_shard_id(0),
            make_shard_id(1),
            make_shard_id(2),
        ]);

        let result = executor.execute(query).unwrap();
        assert_eq!(result.shards_queried, 3);
        assert_eq!(result.shards_succeeded, 3);
    }

    #[test]
    fn test_executor_partial_failure() {
        let mut executor: QueryExecutor<MockShardClient> =
            QueryExecutor::new(ExecutorConfig::default(), test_router());

        let client0 = Arc::new(MockShardClient::new(make_shard_id(0)));
        let client1 = Arc::new(MockShardClient::new(make_shard_id(1)));
        client1.set_healthy(false);

        executor.register_client(make_shard_id(0), client0);
        executor.register_client(make_shard_id(1), client1);

        let query =
            DistributedQuery::new(1, vec![]).with_shards(vec![make_shard_id(0), make_shard_id(1)]);

        let result = executor.execute(query);
        assert!(matches!(result, Err(ExecutorError::PartialFailure { .. })));
    }

    #[test]
    fn test_executor_partial_allowed() {
        let config = ExecutorConfig {
            allow_partial_results: true,
            ..Default::default()
        };
        let mut executor: QueryExecutor<MockShardClient> =
            QueryExecutor::new(config, test_router());

        let client0 = Arc::new(MockShardClient::new(make_shard_id(0)));
        let client1 = Arc::new(MockShardClient::new(make_shard_id(1)));
        client1.set_healthy(false);

        executor.register_client(make_shard_id(0), client0);
        executor.register_client(make_shard_id(1), client1);

        let query =
            DistributedQuery::new(1, vec![]).with_shards(vec![make_shard_id(0), make_shard_id(1)]);

        let result = executor.execute(query).unwrap();
        assert_eq!(result.shards_queried, 2);
        assert_eq!(result.shards_succeeded, 1);
    }

    #[test]
    fn test_executor_all_failed() {
        let mut executor: QueryExecutor<MockShardClient> =
            QueryExecutor::new(ExecutorConfig::default(), test_router());

        let client0 = Arc::new(MockShardClient::new(make_shard_id(0)));
        client0.set_healthy(false);

        executor.register_client(make_shard_id(0), client0);

        let query = DistributedQuery::new(1, vec![]).with_shards(vec![make_shard_id(0)]);

        let result = executor.execute(query);
        assert!(matches!(result, Err(ExecutorError::AllShardsFailed { .. })));
    }

    // ==================== Aggregation Tests ====================

    #[test]
    fn test_aggregation_concat() {
        let mut executor: QueryExecutor<MockShardClient> =
            QueryExecutor::new(ExecutorConfig::default(), test_router());

        let client0 = Arc::new(MockShardClient::new(make_shard_id(0)));
        let client1 = Arc::new(MockShardClient::new(make_shard_id(1)));

        executor.register_client(make_shard_id(0), client0);
        executor.register_client(make_shard_id(1), client1);

        let query = DistributedQuery::new(1, vec![])
            .with_shards(vec![make_shard_id(0), make_shard_id(1)])
            .with_aggregation(AggregationStrategy::Concat);

        let result = executor.execute(query).unwrap();
        assert!(result.is_complete());
    }

    #[test]
    fn test_aggregation_count() {
        let mut executor: QueryExecutor<MockShardClient> =
            QueryExecutor::new(ExecutorConfig::default(), test_router());

        let client = Arc::new(MockShardClient::new(make_shard_id(0)));
        executor.register_client(make_shard_id(0), client);

        let query = DistributedQuery::new(1, vec![])
            .with_shards(vec![make_shard_id(0)])
            .with_aggregation(AggregationStrategy::Count);

        let result = executor.execute(query).unwrap();
        assert_eq!(result.data.len(), 8); // u64
    }

    #[test]
    fn test_aggregation_by_shard() {
        let mut executor: QueryExecutor<MockShardClient> =
            QueryExecutor::new(ExecutorConfig::default(), test_router());

        let client0 = Arc::new(MockShardClient::new(make_shard_id(0)));
        let client1 = Arc::new(MockShardClient::new(make_shard_id(1)));

        executor.register_client(make_shard_id(0), client0);
        executor.register_client(make_shard_id(1), client1);

        let query = DistributedQuery::new(1, vec![])
            .with_shards(vec![make_shard_id(0), make_shard_id(1)])
            .with_aggregation(AggregationStrategy::ByShard);

        let result = executor.execute(query).unwrap();

        // Verify structure: [count: u32] ([shard_id: u16] [len: u32] [data...])*
        let mut offset = 0;

        // Count
        let shard_count = u32::from_le_bytes([
            result.data[offset],
            result.data[offset + 1],
            result.data[offset + 2],
            result.data[offset + 3],
        ]);
        offset += 4;
        assert_eq!(shard_count, 2);

        // Result 1
        let shard_id_1 = u16::from_le_bytes([result.data[offset], result.data[offset + 1]]);
        offset += 2;
        // MockClient returns empty vec by default for query
        let len_1 = u32::from_le_bytes([
            result.data[offset],
            result.data[offset + 1],
            result.data[offset + 2],
            result.data[offset + 3],
        ]);
        offset += 4;
        assert_eq!(len_1, 0);

        // Result 2
        let shard_id_2 = u16::from_le_bytes([result.data[offset], result.data[offset + 1]]);
        offset += 2;
        let len_2 = u32::from_le_bytes([
            result.data[offset],
            result.data[offset + 1],
            result.data[offset + 2],
            result.data[offset + 3],
        ]);
        // offset += 4; // Not needed for further checks
        assert_eq!(len_2, 0);

        // Check IDs are valid (0 and 1, order depends on iteration)
        assert!(shard_id_1 == 0 || shard_id_1 == 1);
        assert!(shard_id_2 == 0 || shard_id_2 == 1);
        assert_ne!(shard_id_1, shard_id_2);
    }

    // ==================== ExecutorStats Tests ====================

    #[test]
    fn test_executor_stats() {
        let mut executor: QueryExecutor<MockShardClient> =
            QueryExecutor::new(ExecutorConfig::default(), test_router());

        let client = Arc::new(MockShardClient::new(make_shard_id(0)));
        executor.register_client(make_shard_id(0), client);

        // Execute a successful query
        let query = DistributedQuery::new(1, vec![]).with_shards(vec![make_shard_id(0)]);
        let _ = executor.execute(query);

        let stats = executor.stats();
        assert_eq!(stats.queries_executed, 1);
        assert_eq!(stats.queries_failed, 0);
        assert!((stats.success_rate() - 1.0).abs() < 0.001);
    }

    #[test]
    fn test_executor_stats_with_failures() {
        let mut executor: QueryExecutor<MockShardClient> =
            QueryExecutor::new(ExecutorConfig::default(), test_router());

        let client = Arc::new(MockShardClient::new(make_shard_id(0)));
        client.set_healthy(false);
        executor.register_client(make_shard_id(0), client);

        // Execute a failing query
        let query = DistributedQuery::new(1, vec![]).with_shards(vec![make_shard_id(0)]);
        let _ = executor.execute(query);

        let stats = executor.stats();
        assert_eq!(stats.queries_failed, 1);
    }

    #[test]
    fn test_query_id_generation() {
        let executor: QueryExecutor<MockShardClient> =
            QueryExecutor::new(ExecutorConfig::default(), test_router());

        let id1 = executor.next_query_id();
        let id2 = executor.next_query_id();
        let id3 = executor.next_query_id();

        assert_eq!(id1, 1);
        assert_eq!(id2, 2);
        assert_eq!(id3, 3);
    }
}
