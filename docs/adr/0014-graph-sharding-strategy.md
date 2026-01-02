# ADR-0014: Graph Sharding Strategy

**Status:** Proposed
**Date:** 2026-01-01
**Deciders:** GallifreyDB Core Team
**Categories:** storage, scalability, distributed

## Context

When the current-state dataset exceeds single-machine RAM (even with tiered storage for historical data), horizontal sharding becomes necessary:

**Scaling Limits:**
- Single machine: ~256GB RAM → ~1.2B current nodes
- Beyond this: Must distribute across multiple machines

**Graph Sharding Challenges:**
- **Edge cuts**: Edges crossing shard boundaries require network hops
- **Multi-hop queries**: N-hop traversal may touch N shards
- **Distributed transactions**: Writes spanning shards need coordination
- **Rebalancing**: Moving data between shards is expensive

**GallifreyDB-Specific Considerations:**
- Bi-temporal data must maintain consistency across shards
- Time-travel queries may need to reconstruct state across shards
- LLM queries often traverse relationships (multi-hop patterns)

## Decision

We will implement **domain-based partitioning with edge replication** as the primary sharding strategy:

### Sharding Architecture

```
┌─────────────────────────────────────────────────────────────────┐
│                      Shard Coordinator                           │
│   • Query routing         • Transaction coordination             │
│   • Shard discovery       • Rebalancing orchestration           │
└─────────────────────────────────────────────────────────────────┘
              │                    │                    │
              ▼                    ▼                    ▼
     ┌─────────────┐      ┌─────────────┐      ┌─────────────┐
     │   Shard 0   │      │   Shard 1   │      │   Shard 2   │
     │   People    │◄────►│   Places    │◄────►│   Events    │
     │             │      │             │      │             │
     │ • Nodes     │      │ • Nodes     │      │ • Nodes     │
     │ • Edges*    │      │ • Edges*    │      │ • Edges*    │
     │ • History   │      │ • History   │      │ • History   │
     └─────────────┘      └─────────────┘      └─────────────┘
              │                    │                    │
              └────────────────────┴────────────────────┘
                    * Cross-shard edges replicated
```

### Partitioning Strategy

**Primary: Domain-Based Partitioning**

Partition by node label/type:

```rust
pub struct ShardConfig {
    pub shards: Vec<ShardDefinition>,
    pub default_shard: ShardId,  // Fallback for unlabeled nodes
}

pub struct ShardDefinition {
    pub id: ShardId,
    pub endpoint: String,
    pub labels: Vec<String>,  // Node labels owned by this shard
}

// Example configuration
let config = ShardConfig {
    shards: vec![
        ShardDefinition {
            id: ShardId(0),
            endpoint: "shard0.gallifrey.local:9000",
            labels: vec!["Person", "User", "Account"],
        },
        ShardDefinition {
            id: ShardId(1),
            endpoint: "shard1.gallifrey.local:9000",
            labels: vec!["Place", "Location", "Address"],
        },
        ShardDefinition {
            id: ShardId(2),
            endpoint: "shard2.gallifrey.local:9000",
            labels: vec!["Event", "Transaction", "Activity"],
        },
    ],
};
```

**Rationale:**
- Queries within a domain stay local (e.g., "find all people named Alice")
- Domain experts can size shards based on data distribution
- Natural alignment with application data model
- Predictable routing without hash lookups

### Edge Replication Strategy

Cross-shard edges are stored on **both** source and target shards:

```
Person (Shard 0)  ----VISITED---->  Place (Shard 1)

Shard 0 stores: (person_id) --VISITED--> (place_id@shard1)
Shard 1 stores: (person_id@shard0) --VISITED--> (place_id)
```

**Benefits:**
- Outgoing traversal from Person: local lookup on Shard 0
- Incoming traversal to Place: local lookup on Shard 1
- No network hop for first-level traversal

**Trade-off:**
- 2x storage for cross-shard edges
- Must maintain consistency on edge updates

### Query Routing

```rust
pub struct ShardRouter {
    config: ShardConfig,
    label_to_shard: HashMap<String, ShardId>,
}

impl ShardRouter {
    /// Route a node query to the appropriate shard
    pub fn route_node(&self, label: &str) -> ShardId {
        *self.label_to_shard.get(label)
            .unwrap_or(&self.config.default_shard)
    }

    /// Route a traversal query
    pub fn route_traversal(&self, start: NodeId, start_label: &str) -> TraversalPlan {
        // Single-shard if all target labels are on same shard
        // Multi-shard if traversal crosses domains
    }
}
```

### Distributed Transaction Protocol

For writes spanning multiple shards, we use **Two-Phase Commit (2PC)**:

```
Coordinator                 Shard A                 Shard B
     │                          │                       │
     │───── PREPARE ───────────►│                       │
     │───── PREPARE ────────────────────────────────────►│
     │                          │                       │
     │◄──── PREPARED ──────────│                       │
     │◄──── PREPARED ───────────────────────────────────│
     │                          │                       │
     │───── COMMIT ────────────►│                       │
     │───── COMMIT ─────────────────────────────────────►│
     │                          │                       │
     │◄──── COMMITTED ─────────│                       │
     │◄──── COMMITTED ──────────────────────────────────│
```

**Implementation:**

```rust
impl ShardCoordinator {
    pub async fn distributed_write(&self, ops: Vec<Operation>) -> Result<()> {
        let tx_id = self.next_tx_id();

        // Group operations by shard
        let by_shard = self.group_by_shard(ops);
        let shard_ids: Vec<_> = by_shard.keys().cloned().collect();

        // Phase 1: Prepare
        let prepare_results = futures::join_all(
            by_shard.iter().map(|(shard, ops)| {
                self.shards[*shard].prepare(tx_id, ops)
            })
        ).await;

        // Check all prepared successfully
        if prepare_results.iter().any(|r| r.is_err()) {
            // Abort all prepared shards
            self.abort_all(tx_id, &shard_ids).await;
            return Err(StorageError::DistributedTransactionFailed);
        }

        // CRITICAL: Log commit decision BEFORE sending commits
        // This enables recovery if coordinator crashes during Phase 2
        self.log_commit_decision(tx_id, &shard_ids).await?;

        // Phase 2: Commit with retry for failures
        for shard_id in &shard_ids {
            loop {
                match self.shards[*shard_id].commit(tx_id).await {
                    Ok(_) => break,
                    Err(e) if e.is_retryable() => {
                        // Shard temporarily unavailable, retry
                        tokio::time::sleep(RETRY_DELAY).await;
                        continue;
                    }
                    Err(e) => {
                        // Non-retryable error - log for manual intervention
                        // The commit decision is logged, so recovery will retry
                        self.log_commit_failure(tx_id, *shard_id, &e).await;
                        return Err(e.into());
                    }
                }
            }
        }

        // Clean up commit log after all shards confirmed
        self.clear_commit_decision(tx_id).await;

        Ok(())
    }
}
```

**Recovery Notes:**

On coordinator startup, scan the commit decision log:
- For each logged `COMMIT` decision, retry sending commit to any shards that haven't acknowledged
- For each logged `PREPARE` without a decision, send abort to all participants
- This ensures eventual consistency even if coordinator crashes mid-transaction

### Rebalancing

When shards become unbalanced or new shards are added:

1. **Monitor**: Track shard sizes and query patterns
2. **Plan**: Identify nodes to migrate (minimize edge cuts)
3. **Dual-write**: New writes go to both old and new location
4. **Migrate**: Copy historical data in background
5. **Cutover**: Update routing table atomically
6. **Cleanup**: Remove data from old shard

```rust
pub struct RebalanceConfig {
    /// Trigger rebalancing when size variance exceeds this
    pub imbalance_threshold: f64,  // default: 0.3 (30%)

    /// Maximum nodes to migrate per batch
    pub batch_size: usize,  // default: 10000

    /// Minimum time between rebalances
    pub cooldown: Duration,  // default: 1 hour
}
```

## Consequences

### Positive

- **Horizontal scalability**: Add shards as data grows
- **Domain locality**: Queries within domain stay fast
- **Predictable routing**: No hash ring complexity
- **Edge replication**: Fast single-hop traversal across shards

### Negative

- **Operational complexity**: Multiple nodes to manage
- **Network latency**: Cross-shard queries add ~1ms per hop
- **2PC overhead**: Distributed writes slower than local
- **Edge storage overhead**: 2x for cross-shard edges (can be higher in power-law graphs where hub nodes have many cross-shard connections)
- **Rebalancing disruption**: Some impact during migrations
- **Temporal ordering complexity**: Transaction time must remain monotonic across shards (requires coordinator-assigned timestamps or hybrid logical clocks)

### Neutral

- Each shard is a full GallifreyDB instance with tiered storage
- Bi-temporal semantics preserved within and across shards
- WAL per shard, no global WAL needed

## Alternatives Considered

### Alternative 1: Hash-Based Partitioning

Assign nodes to shards based on hash(node_id) % num_shards.

**Rejected because:**
- No data locality - related nodes scattered randomly
- Every multi-hop traversal crosses shards
- Rebalancing requires moving ~1/N data when adding shard

### Alternative 2: Community Detection

Use graph algorithms to find dense subgraphs, shard by community.

**Rejected because:**
- Expensive to compute (O(E) or worse)
- Requires full graph analysis before any sharding
- Communities change over time, requiring frequent recomputation
- Better suited as optimization on top of domain-based partitioning

### Alternative 3: Hierarchical Sharding

Shard by relationship depth from "anchor" nodes.

**Rejected because:**
- Requires identifying stable anchor nodes
- Depth from anchor changes as graph grows
- Complex to reason about shard placement

## Implementation Notes

### Shard Discovery

Use a configuration service (etcd, Consul) or static config:

```rust
pub enum ShardDiscovery {
    Static(Vec<ShardEndpoint>),
    Etcd { endpoints: Vec<String>, prefix: String },
    Consul { address: String, service: String },
}
```

### Failure Handling

- **Shard failure**: Queries to that shard fail, others continue
- **Coordinator failure**: New coordinator elected (Raft)
- **Network partition**: Transactions spanning partition abort

### Future Enhancements

1. **Read replicas**: Add read-only replicas per shard
2. **Automatic sharding**: Infer domains from label distribution
3. **Query planning**: Optimize multi-shard query execution
4. **Shard splitting**: Subdivide large shards automatically

## References

- GitHub Issues: [#123](https://github.com/madmax983/GallifreyDB/issues/123), [#124](https://github.com/madmax983/GallifreyDB/issues/124), [#125](https://github.com/madmax983/GallifreyDB/issues/125), [#126](https://github.com/madmax983/GallifreyDB/issues/126)
- Project: [GallifreyDB Scalability Roadmap](https://github.com/users/madmax983/projects/4)
- ADR-0013: Tiered Storage Architecture (prerequisite)
- Facebook TAO: [Paper](https://www.usenix.org/system/files/conference/atc13/atc13-bronson.pdf)
- Google Spanner: [Paper](https://research.google/pubs/pub39966/)
