# ADR-0007: Write-Ahead Log for Durability

**Status:** Accepted
**Date:** 2024-12-31
**Deciders:** GallifreyDB Core Team
**Categories:** storage, durability

## Context

ACID databases require **Durability**: committed transactions must survive crashes and power failures. Two primary approaches exist:

1. **Force at commit**: Write all data to disk on every commit
   - High durability but slow commits
   - Many random writes

2. **Write-Ahead Logging (WAL)**: Write operations to sequential log first
   - Fast sequential writes
   - Recover by replaying log

For GallifreyDB:
- Knowledge graphs can have bursty updates
- Transaction commit latency matters for interactive use
- Data integrity is critical (LLM reasoning depends on accurate history)

## Decision

We will implement a **Write-Ahead Log (WAL)** for durability:

### WAL Structure

```rust
pub struct WriteAheadLog {
    /// File handle for log
    file: File,

    /// Buffered writer for performance
    writer: BufWriter<File>,

    /// Current Log Sequence Number
    current_lsn: LSN,

    /// Configuration
    config: WalConfig,
}

/// Log Sequence Number - globally unique, monotonically increasing
#[derive(Copy, Clone, Eq, PartialEq, Ord, PartialOrd)]
pub struct LSN(u64);
```

### Log Entry Format

```rust
pub struct WalEntry {
    /// Unique sequence number
    pub lsn: LSN,

    /// When this entry was written
    pub timestamp: Timestamp,

    /// The operation being logged
    pub operation: WalOperation,

    /// CRC32 checksum for integrity
    pub checksum: u32,
}

pub enum WalOperation {
    CreateNode {
        tx_id: TxId,
        node_id: NodeId,
        label: String,
        properties: PropertyMap,
        valid_time: TimeRange,
    },
    CreateEdge {
        tx_id: TxId,
        edge_id: EdgeId,
        source: NodeId,
        target: NodeId,
        label: String,
        properties: PropertyMap,
        valid_time: TimeRange,
    },
    UpdateNode {
        tx_id: TxId,
        node_id: NodeId,
        properties: PropertyMap,
        valid_time: TimeRange,
    },
    UpdateEdge {
        tx_id: TxId,
        edge_id: EdgeId,
        properties: PropertyMap,
        valid_time: TimeRange,
    },
    DeleteNode {
        tx_id: TxId,
        node_id: NodeId,
    },
    DeleteEdge {
        tx_id: TxId,
        edge_id: EdgeId,
    },
    BeginTransaction {
        tx_id: TxId,
    },
    CommitTransaction {
        tx_id: TxId,
    },
    AbortTransaction {
        tx_id: TxId,
    },
}
```

### Write Path

```
Transaction Commit:
1. Write WAL entries for all buffered operations
2. Write Commit marker
3. fsync() WAL file ← durability point
4. Apply changes to in-memory storage
5. Return success to client
```

```rust
impl WriteAheadLog {
    pub fn append(&mut self, entry: WalEntry) -> Result<LSN> {
        let serialized = entry.serialize()?;
        self.writer.write_all(&serialized)?;
        Ok(entry.lsn)
    }

    pub fn sync(&mut self) -> Result<()> {
        self.writer.flush()?;
        self.file.sync_all()?;  // fsync
        Ok(())
    }
}
```

### Recovery Process

```rust
impl WriteAheadLog {
    pub fn recover<F>(&mut self, mut apply: F) -> Result<RecoveryStats>
    where
        F: FnMut(WalOperation) -> Result<()>
    {
        let mut stats = RecoveryStats::default();
        let mut active_tx: HashSet<TxId> = HashSet::new();

        for entry in self.read_entries()? {
            // Verify checksum
            if !entry.verify_checksum() {
                return Err(Error::CorruptedWal);
            }

            match &entry.operation {
                WalOperation::BeginTransaction { tx_id } => {
                    active_tx.insert(*tx_id);
                }
                WalOperation::CommitTransaction { tx_id } => {
                    active_tx.remove(tx_id);
                }
                WalOperation::AbortTransaction { tx_id } => {
                    active_tx.remove(tx_id);
                    // Skip applying operations for aborted transactions
                }
                op if active_tx.contains(&op.tx_id()) => {
                    // Part of uncommitted transaction, will be rolled back
                }
                op => {
                    // Apply committed operation
                    apply(op.clone())?;
                    stats.operations_replayed += 1;
                }
            }
        }

        // Any remaining active_tx are uncommitted - implicitly rolled back
        stats.transactions_rolled_back = active_tx.len();

        Ok(stats)
    }
}
```

## Consequences

### Positive

- **Durability**: Committed data survives crashes
- **Fast commits**: Sequential writes are fast
- **Point-in-time recovery**: Can replay to any LSN
- **Atomic transactions**: All-or-nothing via commit markers
- **Audit trail**: Complete operation history

### Negative

- **Storage overhead**: Log files grow until checkpointed
- **Recovery time**: Proportional to log size
- **fsync overhead**: Necessary for true durability
- **Complexity**: Must handle corrupted entries, partial writes

### Neutral

- Standard database technique (PostgreSQL, SQLite, etc.)
- Checkpointing can truncate old log entries
- Log compression possible for long-term retention

## Alternatives Considered

### Alternative 1: Force-at-Commit (No WAL)

Write all modified pages to disk on every commit.

**Rejected because:**
- Many random writes per commit
- High latency for multi-operation transactions
- Poor performance for graph operations (many small writes)

### Alternative 2: Shadow Paging

Maintain shadow copies of modified pages.

**Rejected because:**
- High storage overhead
- Complex page management
- WAL is simpler and well-understood

### Alternative 3: In-Memory Only

No persistence, accept data loss on crash.

**Rejected because:**
- Unacceptable for production use
- LLM reasoning requires reliable data
- Knowledge accumulation would be lost

### Alternative 4: External WAL (e.g., Kafka)

Use external log system for durability.

**Considered for future because:**
- Adds deployment dependency
- Overkill for single-node
- Could enable distributed version later

## Implementation Notes

### Checksum Calculation

```rust
impl WalEntry {
    pub fn calculate_checksum(&self) -> u32 {
        let mut hasher = crc32fast::Hasher::new();
        hasher.update(&self.lsn.0.to_le_bytes());
        hasher.update(&self.timestamp.to_le_bytes());
        hasher.update(&self.operation.serialize());
        hasher.finalize()
    }

    pub fn verify_checksum(&self) -> bool {
        self.checksum == self.calculate_checksum()
    }
}
```

### Configuration

```rust
pub struct WalConfig {
    /// Path to WAL file
    pub path: PathBuf,

    /// Whether to fsync on every commit
    pub sync_on_commit: bool,

    /// Buffer size for BufWriter
    pub buffer_size: usize,

    /// Maximum WAL size before checkpoint
    pub max_size: u64,
}
```

### Performance Targets

| Operation | Target |
|-----------|--------|
| WAL append (buffered) | <10µs |
| WAL sync (fsync) | <5ms (SSD) |
| Recovery (1M operations) | <30s |

### File Format

```
┌────────────────────────────────────────┐
│ WAL Header (version, checksum_type)   │
├────────────────────────────────────────┤
│ Entry 1: [lsn][timestamp][op][crc32]  │
├────────────────────────────────────────┤
│ Entry 2: [lsn][timestamp][op][crc32]  │
├────────────────────────────────────────┤
│ ...                                    │
└────────────────────────────────────────┘
```

## References

- [ARIES: A Transaction Recovery Method](https://cs.stanford.edu/people/chr101/cs345/aries.pdf)
- [PostgreSQL Write Ahead Log](https://www.postgresql.org/docs/current/wal-intro.html)
- [SQLite Write-Ahead Logging](https://sqlite.org/wal.html)
- ADR-0003: MVCC with Snapshot Isolation
