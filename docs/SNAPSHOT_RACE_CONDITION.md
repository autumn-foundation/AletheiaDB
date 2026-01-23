# Snapshot Race Condition Issue

## Problem Statement

The `CheckpointManager::create_checkpoint()` method creates current and historical snapshots **sequentially**, allowing a race condition window where concurrent writes can create inconsistent checkpoint state.

**Location**: `src/storage/checkpoint.rs:207-208`

```rust
// 0. Create MVCC snapshots for isolation
// This prevents fuzzy checkpointing (mixed state from different LSNs)
let current_snapshot = current.create_snapshot(lsn);
let historical_snapshot = historical.create_snapshot(lsn); // ← RACE WINDOW
```

## Race Condition Scenario

```
Time  | Current Storage          | Historical Storage          | Checkpoint State
------|-------------------------|-----------------------------|-----------------
T0    | [Node1, Node2]          | [Version1, Version2]        | (starting)
T1    | create_snapshot() →     | (unchanged)                 | current_snapshot captured
      | captures [N1, N2]       |                             |
T2    | [N1, N2, N3] ← WRITE!  | [V1, V2, V3] ← WRITE!      | ← RACE WINDOW
T3    | (snapshot done)         | create_snapshot() →         | historical_snapshot captured
      |                         | captures [V1, V2, V3]       |
------|-------------------------|-----------------------------|-----------------
Result: current_snapshot=[N1, N2]
        historical_snapshot=[V1, V2, V3]
        → V3 references N3 which doesn't exist in current_snapshot
        → INCONSISTENT CHECKPOINT (orphaned version)
```

## Impact Assessment

| Factor | Assessment |
|--------|-----------|
| **Probability** | Low (1-10μs race window) but non-zero in production |
| **Severity** | HIGH - Checkpoint inconsistency breaks temporal integrity |
| **Current Risk** | Low - Checkpointing not yet integrated into main DB |
| **Future Risk** | HIGH - Must be fixed before production deployment |

**Consequences if unfixed:**
- Recovery from checkpoint restores inconsistent state
- Temporal queries return incorrect results
- Historical versions orphaned from current nodes
- Database integrity violated

## Current Status

**NOT A PRODUCTION BUG (yet)** because:
- Checkpointing is not integrated into `GallifreyDB` main database
- Only called from tests currently (grep showed no production usage in `src/db.rs`)
- Will become critical when background checkpointing is added

## Proposed Solution: Snapshot Coordinator

Introduce lightweight read-write lock coordination:

```rust
// In CheckpointManager
pub struct CheckpointManager {
    // ... existing fields ...

    /// Coordinates writes during snapshot creation to prevent race conditions
    snapshot_coordinator: Arc<RwLock<()>>,
}

impl CheckpointManager {
    pub fn create_checkpoint(
        &mut self,
        lsn: LSN,
        current: &CurrentStorage,
        historical: &HistoricalStorage,
    ) -> Result<CheckpointStats> {
        // Acquire write lock - blocks all writes during snapshot creation
        let _guard = self.snapshot_coordinator.write().unwrap();

        // Now both snapshots are created atomically (no concurrent writes)
        let current_snapshot = current.create_snapshot(lsn);
        let historical_snapshot = historical.create_snapshot(lsn);

        // ... rest of checkpoint logic ...
    }
}
```

**Write operations must acquire read lock:**

```rust
// In CurrentStorage
impl CurrentStorage {
    pub fn insert_node_direct(
        &self,
        node: Node,
        timestamp: Timestamp,
        coordinator: &Arc<RwLock<()>>, // NEW parameter
    ) -> Result<()> {
        // Acquire read lock (many concurrent writes OK, blocked during checkpoint)
        let _guard = coordinator.read().unwrap();

        // ... existing insert logic ...
    }
}
```

### Performance Impact

- **Normal operation**: Read lock overhead ~5-10ns per write (atomic increment)
- **During checkpoint**: Writes blocked for ~1-10ms (snapshot creation time)
- **Checkpoint frequency**: Every 5-10 minutes (configurable)
- **Overall impact**: Negligible (<0.01% throughput reduction)

## Alternative Solutions (Rejected)

| Solution | Why Rejected |
|----------|--------------|
| **Store Arc<Node> in DashMap** | Affects ALL operations, 5-15ns overhead per access |
| **Pause all writes** | Same as proposed solution but less flexible |
| **LSN-based filtering** | Nodes/edges don't carry LSN metadata |
| **Accept inconsistency** | Violates temporal integrity guarantees |

## Implementation Plan

**WHEN checkpointing is integrated into main DB:**

1. Add `SnapshotCoordinator` struct with `RwLock<()>`
2. Thread coordinator through `CurrentStorage` and `HistoricalStorage`
3. Update all write operations to acquire read lock
4. Update `create_checkpoint` to acquire write lock
5. Add integration tests verifying no race condition
6. Benchmark to confirm <0.01% performance impact

## Testing

**Test file**: `tests/snapshot_race_condition.rs`

- Documents the current sequential snapshot creation
- Provides test harness for validating fix (currently ignored)
- Will be enabled when coordinator is implemented

## References

- Code review: User message identifying race condition
- Implementation: `src/storage/checkpoint.rs:207-208`
- Test: `tests/snapshot_race_condition.rs`
- Related: `docs/MVCC_SNAPSHOT_DESIGN.md` (snapshot isolation design)

## Status

- ✅ **Issue documented** (this file)
- ✅ **Test created** (snapshot_race_condition.rs)
- ❌ **Fix not implemented** (waiting for checkpoint integration)
- ⚠️ **BLOCKER**: Must be fixed before production checkpointing deployment
