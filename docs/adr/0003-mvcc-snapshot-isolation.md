# ADR-0003: MVCC with Snapshot Isolation

**Status:** Accepted
**Date:** 2024-12-31
**Deciders:** GallifreyDB Core Team
**Categories:** transaction, concurrency

## Context

GallifreyDB requires concurrent access from multiple transactions while maintaining ACID guarantees. The transaction isolation level significantly impacts both correctness and performance:

- **Read Committed**: Allows non-repeatable reads, insufficient for complex graph traversals
- **Repeatable Read**: Prevents phantom reads but complex to implement
- **Serializable**: Strongest guarantee but significant performance overhead
- **Snapshot Isolation (SI)**: Each transaction sees consistent snapshot, good balance

For a graph database serving LLM queries, we need:
1. Consistent view of the graph during multi-hop traversals
2. High read concurrency (many LLM queries simultaneously)
3. Reasonable write performance for knowledge updates
4. No dirty reads or write-write conflicts

## Decision

We will implement **Multi-Version Concurrency Control (MVCC)** with **Snapshot Isolation (SI)**:

### Transaction Model

```rust
pub struct WriteTransaction {
    tx_id: TxId,
    start_timestamp: Timestamp,
    snapshot: TransactionSnapshot,
    buffer: WriteBuffer,
    // ... storage references
}

pub struct TransactionSnapshot {
    pub snapshot_timestamp: Timestamp,
    pub active_transactions: HashSet<TxId>,
}
```

### Visibility Rules

A version is visible to a transaction if:

```rust
fn is_visible(&self, version: &Version, snapshot: &TransactionSnapshot) -> bool {
    match version.commit_timestamp {
        None => false,  // Uncommitted - never visible to others
        Some(commit_ts) => {
            // Committed before our snapshot AND
            // not created by a transaction that was active when we started
            commit_ts < snapshot.snapshot_timestamp &&
            !snapshot.active_transactions.contains(&version.created_by_tx)
        }
    }
}
```

### Isolation Guarantees

| Anomaly | Prevented? | Notes |
|---------|------------|-------|
| Dirty Read | Yes | Only committed data visible |
| Non-Repeatable Read | Yes | Snapshot is frozen at start |
| Phantom Read | Yes | Snapshot prevents new data visibility |
| Write Skew | No | Acceptable for graph operations |
| Lost Update | Yes | Write-write conflict detection |

### Write-Write Conflict Detection

```rust
// At commit time, detect conflicts
fn detect_conflicts(&self) -> Result<()> {
    for entity_id in self.buffer.modified_entities() {
        let current_version = self.get_current_version(entity_id)?;
        if current_version > self.snapshot_version(entity_id) {
            return Err(TransactionError::ConflictDetected {
                reason: format!("Entity {} modified by concurrent transaction", entity_id)
            });
        }
    }
    Ok(())
}
```

### Read-Your-Writes

Transactions see their own uncommitted changes:

```rust
fn get_node(&self, id: NodeId) -> Result<Node> {
    // First check write buffer
    if let Some(node) = self.buffer.get_node(id) {
        return Ok(node);
    }
    // Fall back to committed data
    self.current.get_node(id)
}
```

## Consequences

### Positive

- **High read concurrency**: Readers never block readers or writers
- **Consistent snapshots**: Complex graph traversals see consistent state
- **No read locks**: Lock-free reads improve latency
- **Natural fit for temporal database**: Versions align with temporal model
- **Simple mental model**: Each transaction sees database "as of" start time

### Negative

- **Write skew possible**: Two transactions can make conflicting writes to different entities
- **Version overhead**: Must maintain multiple versions of data
- **Garbage collection needed**: Old versions must eventually be cleaned up
- **Memory pressure**: Active transactions keep versions alive

### Neutral

- Aligns with PostgreSQL's isolation model
- Standard approach for modern databases
- Well-understood trade-offs

## Alternatives Considered

### Alternative 1: Pessimistic Locking (2PL)

Use read and write locks with two-phase locking protocol.

**Rejected because:**
- Readers block writers and vice versa
- Deadlock potential requires detection/prevention
- Poor fit for read-heavy LLM query workload
- Graph traversals would hold many locks

### Alternative 2: Serializable Snapshot Isolation (SSI)

Extend SI to detect and prevent write skew.

**Rejected for now because:**
- Additional complexity and overhead
- Write skew is acceptable for our use case (graph updates are typically independent)
- Can be added later if needed

### Alternative 3: Optimistic Concurrency Control (OCC)

Validate at commit time without versions.

**Rejected because:**
- Higher abort rates under contention
- Less efficient for read-heavy workloads
- MVCC versions already needed for temporal features

## Implementation Notes

### Transaction Lifecycle

```
1. Begin Transaction
   ├─ Generate TxId (atomic counter)
   ├─ Capture snapshot timestamp
   ├─ Record active transactions
   └─ Register as active

2. Execute Operations
   ├─ Reads: Check buffer → Check committed (with visibility)
   └─ Writes: Buffer in memory

3. Commit
   ├─ Acquire commit timestamp (monotonic)
   ├─ Detect write-write conflicts
   ├─ Log to WAL
   ├─ Apply to storage atomically
   └─ Mark as committed

4. Rollback (on error)
   ├─ Discard write buffer
   └─ Unregister from active set
```

### Key Components

- `TxIdGenerator`: Lock-free atomic counter
- `TxVisibilityManager`: Tracks active and committed transactions
- `WriteBuffer`: In-memory pending changes
- `TransactionSnapshot`: Frozen view at transaction start

## References

- [A Critique of ANSI SQL Isolation Levels](https://www.microsoft.com/en-us/research/publication/a-critique-of-ansi-sql-isolation-levels/)
- [PostgreSQL MVCC](https://www.postgresql.org/docs/current/mvcc-intro.html)
- [Serializable Snapshot Isolation in PostgreSQL](https://drkp.net/papers/ssi-vldb12.pdf)
- ADR-0001: Hybrid Storage Architecture
- ADR-0007: Write-Ahead Log for Durability
