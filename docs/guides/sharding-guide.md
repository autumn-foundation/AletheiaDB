# Graph Sharding Guide

**Last Updated:** 2026-01-22
**Status:** Stable
**Related:** [ADR-0014](../adr/0014-graph-sharding-strategy.md)

## Overview

AletheiaDB's sharding system enables horizontal scalability when your dataset exceeds single-machine capacity. It uses **domain-based partitioning** with **edge replication** to maintain query performance while distributing data across multiple machines.

**Key Features:**
- **Domain-based partitioning**: Nodes partitioned by label/type for data locality
- **Edge replication**: Cross-shard edges stored on both endpoints for fast traversal
- **Two-Phase Commit (2PC)**: ACID transactions across shards
- **Circuit breakers**: Fault tolerance with automatic recovery
- **Online migration**: Move data between shards without downtime

**When to Use Sharding:**
- Dataset exceeds single-machine RAM (~256GB → ~1.2B nodes)
- Need geographic distribution
- Require isolation between domains
- Scale beyond single-node write throughput

## Quick Start

### Basic Setup

```rust
use aletheiadb::storage::sharding::{
    ShardConfig, ShardDefinition, ShardCoordinator, ShardId,
};

// Define shard topology
let config = ShardConfig::new(vec![
    ShardDefinition::new(0, "shard0:9000", vec!["Person", "User"]),
    ShardDefinition::new(1, "shard1:9000", vec!["Place", "Location"]),
    ShardDefinition::new(2, "shard2:9000", vec!["Event", "Activity"]),
]);

// Create coordinator
let coordinator = ShardCoordinator::new(config);

// Route queries
let shard = coordinator.router().route_node("Person");
assert_eq!(shard, ShardId::new(0).unwrap());
```

### Simple Query Routing

```rust
use aletheiadb::storage::sharding::ShardRouter;

let router = ShardRouter::new(&config);

// Single-shard query
let target = router.route_node("Person");
println!("Query Person nodes on shard {}", target.value());

// Multi-hop traversal planning
let plan = router.plan_traversal(
    start_node_id,
    "Person",
    &["KNOWS", "VISITED"],
);

for step in plan.steps() {
    println!("Execute on shard {}: {:?}", step.shard.value(), step);
}
```

## Configuration

### Shard Configuration

```rust
use aletheiadb::storage::sharding::{
    ShardConfig, ShardDefinition, ShardDiscovery, RebalanceConfig,
};
use std::time::Duration;

// Static configuration
let config = ShardConfig {
    shards: vec![
        ShardDefinition {
            id: ShardId::new(0).unwrap(),
            endpoint: "shard0.cluster.local:9000".to_string(),
            labels: vec!["Person".to_string(), "User".to_string()],
        },
        ShardDefinition {
            id: ShardId::new(1).unwrap(),
            endpoint: "shard1.cluster.local:9000".to_string(),
            labels: vec!["Place".to_string(), "Location".to_string()],
        },
    ],
    default_shard: ShardId::new(0).unwrap(),
    discovery: ShardDiscovery::Static,
    rebalance: RebalanceConfig {
        imbalance_threshold: 0.3,  // Trigger at 30% imbalance
        batch_size: 10_000,
        cooldown: Duration::from_secs(3600),  // 1 hour minimum between rebalances
    },
};
```

### Connection Pool Configuration

```rust
use aletheiadb::storage::sharding::{PoolConfig, ConnectionPool};
use std::time::Duration;

let pool_config = PoolConfig {
    min_connections: 2,
    max_connections: 10,
    connection_timeout: Duration::from_secs(5),
    idle_timeout: Duration::from_secs(300),
    health_check_interval: Duration::from_secs(30),
};

let pool = ConnectionPool::new(shard_id, pool_config);
```

### Circuit Breaker Configuration

```rust
use aletheiadb::storage::sharding::{CircuitBreakerConfig, CircuitBreaker};
use std::time::Duration;

let cb_config = CircuitBreakerConfig {
    failure_threshold: 5,           // Open after 5 failures
    reset_timeout: Duration::from_secs(30),  // Wait before probing
    success_threshold: 2,           // Successes to close circuit
};

let circuit_breaker = CircuitBreaker::new(cb_config);
```

### Query Executor Configuration

```rust
use aletheiadb::storage::sharding::{ExecutorConfig, QueryExecutor};
use std::time::Duration;

let executor_config = ExecutorConfig {
    timeout: Duration::from_secs(30),
    max_concurrent_shards: 8,
    retry_count: 3,
    retry_delay: Duration::from_millis(100),
};

let executor = QueryExecutor::new(executor_config);
```

## Usage Patterns

### Pattern 1: Single-Shard Queries

Queries within a domain are routed to a single shard for maximum performance.

```rust
use aletheiadb::storage::sharding::{ShardRouter, QueryExecutor, DistributedQuery};

// Route node lookup
let shard = router.route_node("Person");

// Execute locally on single shard
let query = DistributedQuery::new()
    .target_shards(vec![shard])
    .query_data(b"MATCH (p:Person {name: 'Alice'}) RETURN p");

let result = executor.execute(&query, &clients)?;
```

### Pattern 2: Cross-Shard Traversal

Multi-hop queries that cross domain boundaries use scatter-gather execution.

```rust
use aletheiadb::storage::sharding::{
    DistributedQuery, AggregationStrategy, QueryExecutor,
};

// Find all places visited by people named "Alice"
// Person (Shard 0) -> VISITED -> Place (Shard 1)

// Step 1: Find Alice on Person shard
let step1 = DistributedQuery::new()
    .target_shards(vec![ShardId::new(0).unwrap()])
    .query_data(b"MATCH (p:Person {name: 'Alice'}) RETURN p.id");

let alice_ids = executor.execute(&step1, &clients)?;

// Step 2: Find visited places (uses replicated edges)
let step2 = DistributedQuery::new()
    .target_shards(vec![ShardId::new(1).unwrap()])
    .query_data(&format!(
        "MATCH (p)-[:VISITED]->(place:Place) WHERE p.id IN {:?} RETURN place",
        alice_ids
    ).into_bytes())
    .aggregation(AggregationStrategy::MergeNodes);

let places = executor.execute(&step2, &clients)?;
```

### Pattern 3: Distributed Transactions

Write operations spanning multiple shards use Two-Phase Commit.

```rust
use aletheiadb::storage::sharding::{
    DistributedTransaction, ShardCoordinator, PersistentCommitLog,
};

// Create persistent commit log for crash recovery
let commit_log = PersistentCommitLog::open("data/commit_log")?;

// Begin distributed transaction
let tx = coordinator.begin_transaction()?;

// Add operations for multiple shards
tx.add_operation(ShardId::new(0).unwrap(), create_person_op);
tx.add_operation(ShardId::new(1).unwrap(), create_place_op);
tx.add_operation(ShardId::new(0).unwrap(), create_visited_edge_op);
tx.add_operation(ShardId::new(1).unwrap(), create_visited_edge_replica_op);

// Execute with 2PC
match coordinator.execute_distributed(&tx, &commit_log).await {
    Ok(()) => println!("Transaction committed"),
    Err(e) => println!("Transaction failed: {}", e),
}
```

### Pattern 4: Aggregation Queries

Queries that aggregate data across shards use appropriate aggregation strategies.

```rust
use aletheiadb::storage::sharding::{DistributedQuery, AggregationStrategy};

// Count all nodes across all shards
let count_query = DistributedQuery::new()
    .target_shards(all_shards.clone())
    .query_data(b"MATCH (n) RETURN count(n)")
    .aggregation(AggregationStrategy::Sum);

let total_count = executor.execute(&count_query, &clients)?;

// Collect all results (concatenate)
let collect_query = DistributedQuery::new()
    .target_shards(all_shards.clone())
    .query_data(b"MATCH (n) WHERE n.score > 90 RETURN n LIMIT 10")
    .aggregation(AggregationStrategy::Concat);

// First non-empty result (existence check)
let exists_query = DistributedQuery::new()
    .target_shards(all_shards)
    .query_data(b"MATCH (n {id: 12345}) RETURN n")
    .aggregation(AggregationStrategy::First);
```

## Migration and Rebalancing

### Online Migration

Move data between shards without downtime using the migration executor.

```rust
use aletheiadb::storage::sharding::{
    MigrationExecutor, MigrationConfig, DualWriteRouter,
};

// Configure migration
let migration_config = MigrationConfig {
    batch_size: 10_000,
    verify_checksums: true,
    parallel_batches: 4,
};

let executor = MigrationExecutor::new(migration_config);

// Create migration plan: Move "User" label from shard 0 to shard 2
let migration_id = executor.create_migration(
    vec!["User".to_string()],
    ShardId::new(0).unwrap(),  // source
    ShardId::new(2).unwrap(),  // target
)?;

// Start migration (enters dual-write mode)
executor.start(migration_id)?;

// Monitor progress
loop {
    let stats = executor.get_stats(migration_id)?;
    println!(
        "Progress: {}/{} nodes migrated ({:.1}%)",
        stats.nodes_migrated,
        stats.total_nodes,
        stats.progress_percent()
    );

    if stats.is_complete() {
        break;
    }
    std::thread::sleep(Duration::from_secs(5));
}

// Complete migration (updates routing, cleans up)
executor.complete(migration_id)?;
```

### Dual-Write Router

During migration, the dual-write router ensures writes go to both old and new locations.

```rust
use aletheiadb::storage::sharding::DualWriteRouter;

let router = DualWriteRouter::new();

// Register active migration
router.register_migration("User", ShardId::new(0).unwrap(), ShardId::new(2).unwrap());

// Route writes during migration
let targets = router.route_write("User", ShardId::new(0).unwrap());
assert_eq!(targets, vec![ShardId::new(0).unwrap(), ShardId::new(2).unwrap()]);

// Non-migrating labels route normally
let targets = router.route_write("Person", ShardId::new(0).unwrap());
assert_eq!(targets, vec![ShardId::new(0).unwrap()]);
```

### Rebalancing

Automatic rebalancing monitors shard sizes and triggers migrations when imbalanced.

```rust
use aletheiadb::storage::sharding::{RebalanceManager, MigrationPlan};

let manager = RebalanceManager::new(rebalance_config);

// Check if rebalancing is needed
if let Some(plan) = manager.check_balance(&shard_metrics)? {
    println!("Rebalance plan:");
    for migration in &plan.migrations {
        println!(
            "  Move {} from shard {} to shard {}",
            migration.label, migration.source, migration.target
        );
    }

    // Execute plan
    manager.execute_plan(plan)?;
}
```

## Performance Tuning

### Query Optimization

**1. Minimize Cross-Shard Hops**

```rust
// Bad: Scatter to all shards
let query = DistributedQuery::new()
    .target_shards(all_shards)  // Hits every shard
    .query_data(b"MATCH (p:Person {name: 'Alice'}) RETURN p");

// Good: Route to correct shard
let shard = router.route_node("Person");
let query = DistributedQuery::new()
    .target_shards(vec![shard])  // Single shard
    .query_data(b"MATCH (p:Person {name: 'Alice'}) RETURN p");
```

**2. Use Appropriate Aggregation**

```rust
// For existence checks, use First (stops early)
let query = DistributedQuery::new()
    .aggregation(AggregationStrategy::First);

// For collecting unique nodes, use MergeNodes (deduplicates)
let query = DistributedQuery::new()
    .aggregation(AggregationStrategy::MergeNodes);
```

**3. Batch Operations**

```rust
// Bad: Individual operations
for id in node_ids {
    coordinator.delete_node(id)?;
}

// Good: Batch by shard
let by_shard = group_by_shard(&node_ids, &router);
for (shard, ids) in by_shard {
    coordinator.delete_nodes_batch(shard, ids)?;
}
```

### Connection Tuning

| Workload | min_connections | max_connections | Notes |
|----------|-----------------|-----------------|-------|
| Low traffic | 1 | 5 | Minimize resources |
| Medium traffic | 2 | 10 | Default |
| High traffic | 5 | 20 | More parallelism |
| Burst traffic | 2 | 50 | Handle spikes |

### Circuit Breaker Tuning

| Environment | failure_threshold | reset_timeout | Notes |
|-------------|-------------------|---------------|-------|
| Development | 3 | 10s | Fast feedback |
| Production | 5 | 30s | Default |
| Unstable network | 10 | 60s | More tolerance |

## Monitoring and Debugging

### Metrics

```rust
use aletheiadb::storage::sharding::{ShardMetrics, PoolStats, ExecutorStats};

// Shard-level metrics
let metrics = coordinator.get_shard_metrics(shard_id)?;
println!("Shard {} metrics:", shard_id.value());
println!("  Nodes: {}", metrics.node_count);
println!("  Edges: {}", metrics.edge_count);
println!("  Size: {} MB", metrics.size_bytes / 1_000_000);

// Connection pool stats
let pool_stats = pool.stats();
println!("Pool stats:");
println!("  Active: {}", pool_stats.active_connections);
println!("  Idle: {}", pool_stats.idle_connections);
println!("  Failed: {}", pool_stats.failed_connections);

// Query executor stats
let exec_stats = executor.stats();
println!("Executor stats:");
println!("  Queries: {}", exec_stats.total_queries);
println!("  Avg latency: {:?}", exec_stats.avg_latency);
println!("  Errors: {}", exec_stats.error_count);
```

### Commit Log Inspection

```rust
use aletheiadb::storage::sharding::PersistentCommitLog;

let log = PersistentCommitLog::open("data/commit_log")?;

// Check for pending transactions (need recovery)
let pending = log.get_pending_transactions()?;
for (tx_id, entry) in pending {
    println!("Pending tx {}: {:?}", tx_id.as_u64(), entry.entry_type);
}

// Get log stats
let stats = log.stats();
println!("Commit log stats:");
println!("  Entries: {}", stats.entries_written);
println!("  Bytes: {}", stats.bytes_written);
println!("  Current LSN: {}", stats.current_lsn);
```

### Circuit Breaker State

```rust
use aletheiadb::storage::sharding::CircuitState;

let state = circuit_breaker.state();
match state {
    CircuitState::Closed => println!("Circuit closed - normal operation"),
    CircuitState::Open => println!("Circuit OPEN - failing fast"),
    CircuitState::HalfOpen => println!("Circuit half-open - probing"),
}
```

## Error Handling

### Common Errors

```rust
use aletheiadb::storage::sharding::{NetworkError, ExecutorError, MigrationError};

// Network errors
match client.query(query_id, data) {
    Ok(result) => process(result),
    Err(NetworkError::Timeout) => {
        // Retry with backoff
    }
    Err(NetworkError::ConnectionFailed(_)) => {
        // Check circuit breaker, may need to fail over
    }
    Err(NetworkError::CircuitOpen) => {
        // Shard unhealthy, route to replica or fail
    }
    Err(e) => return Err(e.into()),
}

// Executor errors
match executor.execute(&query, &clients) {
    Ok(result) => process(result),
    Err(ExecutorError::AllShardsFailed) => {
        // Complete failure, no results available
    }
    Err(ExecutorError::PartialFailure { results, errors }) => {
        // Some shards succeeded, decide how to proceed
        if results.len() >= quorum {
            process_partial(results)
        } else {
            return Err(...)
        }
    }
    Err(ExecutorError::Timeout) => {
        // Query took too long
    }
}

// Migration errors
match executor.start(migration_id) {
    Ok(()) => println!("Migration started"),
    Err(MigrationError::SourceUnavailable) => {
        // Source shard down, cannot start
    }
    Err(MigrationError::TargetUnavailable) => {
        // Target shard down, cannot start
    }
    Err(MigrationError::AlreadyInProgress) => {
        // Another migration running for same label
    }
}
```

### Recovery Procedures

**1. Coordinator Crash Recovery**

```rust
// On startup, recover pending 2PC transactions
let log = PersistentCommitLog::open("data/commit_log")?;
let pending = log.recover_pending()?;

for (tx_id, entry) in pending {
    match entry.entry_type {
        EntryType::Committed => {
            // Decision was to commit - retry commit on all participants
            for shard in entry.participants {
                retry_until_success(|| clients[shard].commit(tx_id));
            }
        }
        EntryType::Preparing => {
            // No decision made - abort all participants
            for shard in entry.participants {
                let _ = clients[shard].abort(tx_id);
            }
        }
        _ => {}
    }
    log.clear_entry(tx_id)?;
}
```

**2. Shard Recovery**

```rust
// When a shard comes back online
pool.mark_healthy(shard_id);
circuit_breaker.reset();

// Check for any stuck migrations
let migrations = executor.get_migrations_involving(shard_id)?;
for migration in migrations {
    if migration.state == MigrationState::Migrating {
        // Resume data transfer
        executor.resume(migration.id)?;
    }
}
```

## Best Practices

### Shard Design

1. **Choose labels carefully**: Group frequently co-queried labels on the same shard
2. **Balance shard sizes**: Aim for roughly equal data per shard
3. **Plan for growth**: Leave room for additional labels in each shard
4. **Consider access patterns**: Place hot data on faster hardware

### Transaction Design

1. **Minimize cross-shard transactions**: Design schema to reduce distributed writes
2. **Keep transactions short**: Long-running 2PC blocks resources
3. **Use local transactions when possible**: Single-shard writes are faster
4. **Implement idempotency**: Retries may cause duplicate operations

### Migration Best Practices

1. **Migrate during low traffic**: Less dual-write overhead
2. **Monitor progress**: Watch for stalls or errors
3. **Have rollback plan**: Know how to reverse if needed
4. **Verify after completion**: Run consistency checks

### Operational Practices

1. **Monitor circuit breakers**: Alert on state changes
2. **Set appropriate timeouts**: Too short causes false failures
3. **Use connection pools**: Avoid connection churn
4. **Log commit decisions**: Essential for crash recovery

## Troubleshooting

### High Latency

**Symptoms:** Queries taking longer than expected

**Diagnosis:**
```rust
let stats = executor.stats();
println!("Avg scatter time: {:?}", stats.avg_scatter_time);
println!("Avg gather time: {:?}", stats.avg_gather_time);
println!("Slowest shard: {:?}", stats.slowest_shard);
```

**Solutions:**
- Check network connectivity to slow shards
- Verify circuit breakers aren't causing retries
- Consider adding read replicas for hot shards
- Review query patterns for unnecessary cross-shard hops

### Transaction Failures

**Symptoms:** 2PC transactions failing frequently

**Diagnosis:**
```rust
let log = PersistentCommitLog::open("data/commit_log")?;
let stats = log.stats();
println!("Abort rate: {}%", stats.abort_rate() * 100.0);
println!("Timeout rate: {}%", stats.timeout_rate() * 100.0);
```

**Solutions:**
- Check for network partitions
- Verify shard health
- Review transaction timeouts
- Consider schema changes to reduce cross-shard writes

### Migration Stalls

**Symptoms:** Migration progress stops

**Diagnosis:**
```rust
let stats = executor.get_stats(migration_id)?;
println!("State: {:?}", stats.state);
println!("Last progress: {:?}", stats.last_progress_time);
println!("Error count: {}", stats.error_count);
```

**Solutions:**
- Check source and target shard health
- Review batch size (may be too large)
- Check for lock contention
- Verify network throughput

## API Reference

### Core Types

| Type | Description |
|------|-------------|
| `ShardId` | Strongly-typed shard identifier |
| `ShardConfig` | Shard topology configuration |
| `ShardDefinition` | Individual shard definition |
| `ShardCoordinator` | Main coordinator for sharding operations |
| `ShardRouter` | Routes queries to appropriate shards |

### Network Types

| Type | Description |
|------|-------------|
| `ShardClient` | Trait for shard communication |
| `ConnectionPool` | Manages connections to shards |
| `CircuitBreaker` | Fault tolerance circuit breaker |
| `PoolConfig` | Connection pool configuration |

### Transaction Types

| Type | Description |
|------|-------------|
| `DistributedTransaction` | Cross-shard transaction |
| `PersistentCommitLog` | Durable 2PC commit log |
| `CommitLogEntry` | Individual commit log entry |
| `TransactionPhase` | Current phase of transaction |

### Query Types

| Type | Description |
|------|-------------|
| `QueryExecutor` | Executes distributed queries |
| `DistributedQuery` | Query targeting multiple shards |
| `AggregationStrategy` | How to combine shard results |
| `QueryResult` | Result from distributed query |

### Migration Types

| Type | Description |
|------|-------------|
| `MigrationExecutor` | Manages data migrations |
| `DualWriteRouter` | Routes writes during migration |
| `MigrationConfig` | Migration settings |
| `MigrationState` | Current state of migration |

## See Also

- [ADR-0014: Graph Sharding Strategy](../adr/0014-graph-sharding-strategy.md) - Architecture decision
- [ADR-0013: Tiered Storage](../adr/0013-tiered-storage-architecture.md) - Storage architecture
- [Configuration Guide](../CONFIGURATION.md) - General configuration
- [Architecture Overview](../ARCHITECTURE.md) - System architecture
