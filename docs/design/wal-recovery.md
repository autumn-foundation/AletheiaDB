# WAL Recovery Design Document

**Status**: Design Complete
**Version**: 1.0
**Date**: 2026-01-12
**Author**: Claude Code (Issue #286)

## Table of Contents

1. [Overview](#overview)
2. [Recovery Algorithm](#recovery-algorithm)
3. [Crash Scenarios & Guarantees](#crash-scenarios--guarantees)
4. [Bi-Temporal Semantics](#bi-temporal-semantics)
5. [Testing Strategy](#testing-strategy)
6. [Performance Targets](#performance-targets)
7. [Implementation Phases](#implementation-phases)

## Overview

This document defines the WAL (Write-Ahead Log) recovery mechanism for AletheiaDB, ensuring data durability and correct bi-temporal semantics after crashes. The recovery system must:

1. **Restore database state** from checkpoints and WAL
2. **Maintain ACID guarantees** based on durability mode
3. **Preserve bi-temporal semantics** for all operations
4. **Initialize ID generators** to prevent collisions
5. **Recover vector indexes** with configuration

### Key Principles

- **Idempotency**: Replaying the same WAL entry multiple times produces the same result
- **Temporal Correctness**: Delete operations must close previous versions' transaction_time BEFORE creating tombstones
- **ID Safety**: All IDs must be validated and tracked to prevent overflow/DoS attacks
- **Graceful Degradation**: Partial recovery from corrupted WAL segments

## Recovery Algorithm

### Startup Sequence

```
┌─────────────────────────────────────────────────────────┐
│                   Database Startup                       │
└────────────────────┬────────────────────────────────────┘
                     │
                     ▼
        ┌────────────────────────┐
        │  Find Latest Checkpoint │
        │  checkpoint_*.dat       │
        └────────┬───────────────┘
                 │
        ┌────────▼────────┐
        │ Load Checkpoint │
        │  - LSN: N       │
        │  - State        │
        │  - Vector Config│
        └────────┬────────┘
                 │
                 ▼
    ┌────────────────────────────┐
    │  Read WAL from LSN(N+1)   │
    │  - Parse entries           │
    │  - Verify checksums        │
    └────────┬───────────────────┘
             │
             ▼
    ┌─────────────────────────────┐
    │  Replay Operations Loop     │
    │  - CreateNode/CreateEdge    │
    │  - UpdateNode/UpdateEdge    │
    │  - DeleteNode/DeleteEdge    │
    │  - Track max IDs            │
    └────────┬────────────────────┘
             │
             ▼
    ┌──────────────────────────────┐
    │  Initialize ID Generators    │
    │  - NodeId: max + 1           │
    │  - EdgeId: max + 1           │
    │  - VersionId: max + 1        │
    └────────┬─────────────────────┘
             │
             ▼
    ┌──────────────────────────────┐
    │  Restore Vector Indexes      │
    │  - Re-enable if configured   │
    │  - Rebuild HNSW index        │
    └────────┬─────────────────────┘
             │
             ▼
    ┌──────────────────────────────┐
    │  Database Ready              │
    │  - Current storage restored  │
    │  - Historical storage intact │
    │  - Indexes rebuilt           │
    └──────────────────────────────┘
```

### Operation Replay Order

WAL entries are **already sorted by LSN** (thanks to concurrent WAL's sorted flush), so we replay in order:

```rust
for entry in wal.read_from(start_lsn) {
    match entry.operation {
        WalOperation::CreateNode { .. } => replay_create_node(...),
        WalOperation::CreateEdge { .. } => replay_create_edge(...),
        WalOperation::UpdateNode { .. } => replay_update_node(...),
        WalOperation::UpdateEdge { .. } => replay_update_edge(...),
        WalOperation::DeleteNode { .. } => replay_delete_node(...),
        WalOperation::DeleteEdge { .. } => replay_delete_edge(...),
        WalOperation::Checkpoint { .. } => {
            // Track checkpoint LSN for recovery progress
        }
    }

    // Track max IDs
    max_node_id = max_node_id.max(node_id.as_u64());
    max_edge_id = max_edge_id.max(edge_id.as_u64());
    max_version_id = max_version_id.max(version_id.as_u64());
}
```

### Operation Dependencies

```
CreateNode/CreateEdge → No dependencies (base operations)
UpdateNode/UpdateEdge → Requires previous version exists
DeleteNode/DeleteEdge → ⚠️ CRITICAL: Must close previous version FIRST
```

### ID Generator State Reconstruction

After replaying all operations, initialize ID generators:

```rust
let node_id_gen = IdGenerator::with_start(max_node_id + 1);
let edge_id_gen = IdGenerator::with_start(max_edge_id + 1);
let version_id_gen = IdGenerator::with_start(max_version_id + 1);
```

**Why `max + 1`?**
Ensures next created entity gets a unique ID that doesn't collide with any existing entity.

### Error Handling

```rust
match replay_operation(entry) {
    Ok(()) => continue,
    Err(RecoveryError::CorruptedEntry { lsn, reason }) => {
        // Log corruption
        error!("WAL entry {} corrupted: {}", lsn, reason);

        // Attempt partial recovery (stop at corruption)
        warn!("Stopping recovery at LSN {}", lsn);
        break;
    }
    Err(RecoveryError::InvalidId { id, reason }) => {
        // Critical: ID validation failed (DoS attack?)
        error!("Invalid ID during recovery: {} - {}", id, reason);
        return Err(RecoveryError::InvalidId { id, reason });
    }
    Err(e) => return Err(e),
}
```

## Crash Scenarios & Guarantees

### Scenario Matrix

| Scenario | Checkpoint Exists? | WAL Entries Since? | Recovery Behavior | Data Loss? |
|----------|-------------------|-------------------|-------------------|-----------|
| **Crash before checkpoint** | ❌ No | All operations | Replay entire WAL from LSN 0 | Depends on durability mode |
| **Crash after checkpoint** | ✅ Yes | N entries | Replay from checkpoint LSN + 1 | Depends on durability mode |
| **Crash during WAL write** | ✅ Yes/No | Partial entry | Detect corruption, skip incomplete | Last incomplete operation lost |
| **Crash during checkpoint** | ✅ Yes (previous) | N entries | Use previous checkpoint, replay all | No loss (checkpoints atomic) |
| **Corrupted WAL segment** | ✅ Yes | Some corrupted | Recover until corruption point | Operations after corruption lost |

### Durability Guarantees by Mode

#### Synchronous Mode (fsync on every commit)

```
┌────────────┐     ┌─────────┐     ┌──────────┐
│ Operation  │────▶│   WAL   │────▶│  fsync   │────▶ Return Success
│            │     │  Write  │     │          │
└────────────┘     └─────────┘     └──────────┘
```

**Guarantee**: If operation returns success, data survives any crash (disk failure only).

**Recovery**:
- ✅ All successful operations present in WAL
- ❌ Operations that did NOT return success are lost (expected)

#### GroupCommit Mode (batched fsync with waiting)

```
┌────────────┐     ┌─────────┐     ┌────────────┐
│ Operation  │────▶│  Append │────▶│ Wait Epoch │────▶ Return Success
│            │     │ to WAL  │     │  Flushed   │
└────────────┘     └─────────┘     └────────────┘
                                           ▲
                                           │
                                   ┌───────┴────────┐
                                   │ Background:    │
                                   │ sort → write → │
                                   │ fsync → notify │
                                   └────────────────┘
```

**Guarantee**: If operation returns success, data has been fsynced (ACID compliant).

**Recovery**:
- ✅ All successful operations present in WAL
- ❌ Operations still in buffers (not yet returned) are lost

#### Async Mode (no waiting, eventual durability)

```
┌────────────┐     ┌─────────┐     Return Success
│ Operation  │────▶│  Append │────▶ (immediately)
│            │     │ to WAL  │
└────────────┘     └─────────┘
                        │
                        ▼
                ┌───────────────┐
                │ Background:   │
                │ flush every   │
                │ 10-100ms      │
                └───────────────┘
```

**Guarantee**: NO durability guarantee (eventual consistency).

**Recovery**:
- ⚠️ Operations in buffer (not yet flushed) are LOST
- Only operations that were flushed before crash survive

### Crash Timeline Example

```
Time: T0────T1────T2────T3────T4────T5────┐CRASH
                                            │
WAL:  [1]  [2]  [3]  [CP]  [4]  [5]  [6]  │
      Node Node Node      Node Node Node    │
       A    B    C         D    E    F      │
                                            │
Checkpoint at T3 (LSN 3): Contains A, B, C │
WAL after checkpoint: D, E, F              │
                                            │
Recovery: Load checkpoint → Replay 4,5,6   │
Result: All 6 nodes present ✅             │
```

## Bi-Temporal Semantics

### ⚠️ CRITICAL: Delete Operation Ordering

The most critical aspect of recovery is correctly handling delete operations to maintain bi-temporal integrity.

#### The Problem

Without correct ordering, time-travel queries will return deleted entities:

```rust
// ❌ WRONG (without closing previous version first)
db.create_node(node_id, props);           // V1: tx_time [T1, ∞)
// ... crash and recover ...
db.delete_node(node_id);                   // Tombstone: tx_time [T2, ∞)
// V1 still has tx_time [T1, ∞) - overlaps with deletion!

db.as_of(T1).get_node(node_id);           // ❌ Returns node (WRONG!)
db.as_of(T2).get_node(node_id);           // ❌ Returns node (WRONG!)
```

#### The Solution

**ALWAYS close previous version's transaction_time BEFORE creating tombstone:**

```rust
// ✅ CORRECT (close previous version first)
db.create_node(node_id, props);           // V1: tx_time [T1, ∞)
// ... crash and recover ...
db.delete_node(node_id);
  // Step 1: Close V1's transaction_time
  historical.close_node_version_transaction_time(V1, T2);  // V1: tx_time [T1, T2)
  // Step 2: Create tombstone
  historical.add_node_version(node_id, V2, tombstone_temporal, ...);  // Tombstone: tx_time [T2, ∞), valid_time [T2, T2)

db.as_of(T1).get_node(node_id);           // ✅ Returns node (CORRECT!)
db.as_of(T2).get_node(node_id);           // ✅ Returns None (CORRECT!)
```

### Temporal Invariants to Preserve

1. **Transaction Time Monotonicity**
   ```rust
   for version in entity.versions() {
       assert!(version.transaction_time().start() <= next_version.transaction_time().start());
   }
   ```

2. **No Transaction Time Gaps**
   ```rust
   for window in entity.versions().windows(2) {
       let prev = &window[0];
       let curr = &window[1];
       assert_eq!(prev.transaction_time().end(), Some(curr.transaction_time().start()));
   }
   ```

3. **Delete Closes Previous Version**
   ```rust
   if operation.is_delete() {
       let prev_version = historical.get_current_version(entity_id);
       assert!(prev_version.transaction_time().is_closed());
       assert_eq!(prev_version.transaction_time().end(), Some(delete_timestamp));
   }
   ```

4. **Tombstone Valid Time is Closed**
   ```rust
   if version.is_tombstone() {
       assert!(version.valid_time().is_closed());
       assert_eq!(version.valid_time().end(), Some(version.transaction_time().start()));
   }
   ```

### Replay Implementation Pattern

```rust
fn replay_delete_node(
    node_id: NodeId,
    tombstone_version_id: VersionId,
    temporal: BiTemporalInterval,
    current: &mut CurrentStorage,
    historical: &mut HistoricalStorage,
    temporal_indexes: &TemporalIndexes,
) -> Result<(), RecoveryError> {
    // 1. Validate IDs (prevent DoS)
    node_id.validate()?;
    tombstone_version_id.validate()?;

    // 2. Get commit timestamp from tombstone's temporal interval
    let commit_timestamp = temporal.transaction_time().start();

    // 3. ⚠️ CRITICAL: Close previous version FIRST
    if let Some(current_version_id) = historical.get_current_node_version(node_id) {
        historical.close_node_version_transaction_time(current_version_id, commit_timestamp)?;
    }

    // 4. Create tombstone with closed valid_time
    let tombstone_temporal = BiTemporalInterval::current(commit_timestamp)
        .close_valid_time(commit_timestamp);

    // 5. Add tombstone to historical storage
    let node = current.get_node(node_id)?;  // Get before deleting
    historical.add_node_version(
        node_id,
        tombstone_version_id,
        tombstone_temporal,
        node.label,
        node.properties.clone(),
    )?;

    // 6. Index tombstone
    temporal_indexes.insert_node_version(node_id, tombstone_version_id, tombstone_temporal)?;

    // 7. Remove from current storage (LAST step)
    current.delete_node_direct(node_id, commit_timestamp)?;

    Ok(())
}
```

## Testing Strategy

### Unit Tests (Issue #293)

Test each replay handler in isolation:

```rust
mod replay_create_tests {
    #[test] fn test_replay_create_node_basic()
    #[test] fn test_replay_create_node_with_properties()
    #[test] fn test_replay_create_node_with_vector()
    #[test] fn test_replay_create_node_invalid_id()  // DoS prevention
    #[test] fn test_replay_create_node_tracks_max_id()

    #[test] fn test_replay_create_edge_basic()
    #[test] fn test_replay_create_edge_with_properties()
    #[test] fn test_replay_create_edge_missing_nodes()  // Error handling
    #[test] fn test_replay_create_edge_invalid_id()
    #[test] fn test_replay_create_edge_tracks_max_id()
}

mod replay_update_tests {
    #[test] fn test_replay_update_node_basic()
    #[test] fn test_replay_update_node_multiple_versions()
    #[test] fn test_replay_update_node_closes_previous_transaction_time()
    #[test] fn test_replay_update_node_nonexistent()  // Error handling

    #[test] fn test_replay_update_edge_basic()
    #[test] fn test_replay_update_edge_multiple_versions()
}

mod replay_delete_tests {
    #[test] fn test_replay_delete_node_basic()
    #[test] fn test_replay_delete_node_closes_previous_version_first()  // CRITICAL
    #[test] fn test_replay_delete_node_tombstone_valid_time_closed()
    #[test] fn test_replay_delete_node_time_travel_queries()  // Integration
    #[test] fn test_replay_delete_node_nonexistent()  // Graceful handling

    #[test] fn test_replay_delete_edge_basic()
    #[test] fn test_replay_delete_edge_closes_previous_version_first()
}

mod replay_id_tracking_tests {
    #[test] fn test_id_generator_recovery_node_ids()
    #[test] fn test_id_generator_recovery_edge_ids()
    #[test] fn test_id_generator_recovery_version_ids()
    #[test] fn test_no_id_collision_after_recovery()
}
```

### Integration Tests (Issue #294)

Test complete crash scenarios end-to-end:

```rust
mod crash_scenarios {
    #[test] fn test_crash_before_checkpoint()
    // Create 100 nodes/edges → Crash (no checkpoint) → Recover → Verify all present

    #[test] fn test_crash_after_checkpoint()
    // Create 50 nodes → Checkpoint → Create 50 more → Crash → Recover → Verify all 100

    #[test] fn test_crash_during_wal_write()
    // Create nodes → Corrupt last WAL entry → Recover → Verify partial recovery

    #[test] fn test_multiple_crashes()
    // Create → Crash → Recover → Create → Crash → Recover → Verify cumulative state

    #[test] fn test_complex_workflow()
    // Create → Update → Delete → Create more → Crash → Recover → Verify state

    #[test] fn test_large_dataset_recovery()
    // 10K nodes, 50K edges → Crash → Recover → Measure time (<10s target)

    #[test] fn test_recovery_with_vector_index()
    // Nodes with embeddings → Crash → Recover → Verify vector index rebuilt
}
```

### Property-Based Tests (Issue #295)

Use `proptest` to verify temporal invariants hold:

```rust
proptest! {
    #[test]
    fn test_recovery_preserves_temporal_invariants(
        operations in prop::collection::vec(arbitrary_operation(), 1..100)
    ) {
        // Execute random operations → Crash → Recover

        // Verify invariants:
        // 1. Transaction time monotonic
        for node in db.all_nodes() {
            verify_transaction_time_monotonic(node.id());
        }

        // 2. No transaction time gaps
        for node in db.all_nodes() {
            verify_no_transaction_time_gaps(node.id());
        }

        // 3. Delete closes previous version
        for tombstone in db.all_tombstones() {
            verify_delete_closes_previous_version(tombstone);
        }

        // 4. Tombstone valid time closed
        for tombstone in db.all_tombstones() {
            assert!(tombstone.valid_time().is_closed());
        }
    }
}
```

### Performance Benchmarks (Issue #296)

```rust
criterion_group!(benches,
    bench_recovery_small,     // 100 nodes, 500 edges → <100ms
    bench_recovery_medium,    // 10K nodes, 50K edges → <5s
    bench_recovery_large,     // 100K nodes, 500K edges → <30s
    bench_recovery_wal_only,  // No checkpoint, 100K ops → <10s
    bench_recovery_vector,    // 10K nodes w/ 384-dim → <10s (includes re-indexing)
);
```

### Coverage Requirements

Following AletheiaDB standards (TESTING.md):
- **Minimum 85% line coverage**
- **Minimum 88% function coverage**
- **Minimum 88% region coverage**

## Performance Targets

| Dataset Size | Operations | Target Recovery Time | Notes |
|--------------|-----------|---------------------|-------|
| Small | 100 nodes, 500 edges | <100ms | Startup latency critical |
| Medium | 10K nodes, 50K edges | <5 seconds | Typical application size |
| Large | 100K nodes, 500K edges | <30 seconds | Enterprise scale |
| WAL-only | 100K operations, no checkpoint | <10 seconds | Worst case (no checkpoint) |
| With Vectors | 10K nodes, 384-dim embeddings | <10 seconds | Includes HNSW rebuild |

### Performance Breakdown

```
Recovery Time = Checkpoint Load + WAL Parse + Replay + ID Init + Index Rebuild

Checkpoint Load:  ~10-50ms   (memory-mapped read)
WAL Parse:        ~1ms/1K ops (binary format, CRC32 verification)
Replay:           ~5µs/op     (direct storage insertion)
ID Init:          ~1ms        (atomic initialization)
Index Rebuild:    ~100ms/10K  (HNSW construction from vectors)
```

### Optimization Strategies

1. **Parallel Checkpoint Loading**: Memory-map large checkpoints
2. **Batched Replay**: Apply operations in batches to reduce lock overhead
3. **Lazy Index Rebuild**: Build indexes on-demand for first query
4. **Incremental Checkpoints**: Only serialize changed entities (future work)

## Implementation Phases

### Phase 1: Foundation (Issues #286-287)

- [x] Design document (this document)
- [ ] Basic WAL replay loop structure
- [ ] ID tracking infrastructure
- [ ] Error handling framework

### Phase 2: Core Operations (Issues #288-290)

- [ ] CreateNode/CreateEdge replay handlers
- [ ] UpdateNode/UpdateEdge replay handlers
- [ ] ⚠️ CRITICAL: DeleteNode/DeleteEdge replay with bi-temporal semantics

### Phase 3: ID & Vector Recovery (Issues #291-292)

- [ ] ID generator initialization
- [ ] Vector index recovery and rebuild

### Phase 4: Testing (Issues #293-296)

- [ ] Unit tests for all replay handlers
- [ ] Integration tests for crash scenarios
- [ ] Property-based tests for temporal invariants
- [ ] Performance benchmarks

### Phase 5: Documentation (Issues #297-299)

- [ ] Update docs/WAL.md with recovery section
- [ ] Add recovery examples to user guide
- [ ] Update CLAUDE.md architecture section

## References

- **Current Implementation**: `src/storage/checkpoint.rs` (`CheckpointManager::recover` + `replay_wal`)
- **Write Transaction Pattern**: `src/api/transaction/write_tx.rs:668-950` (apply_changes)
- **WAL Format**: `docs/WAL.md` (binary format specification)
- **Bi-Temporal Semantics**: Issue #290, Issue #12 (time-travel correctness)
- **Concurrent WAL**: ADR-0020 (sorted flush guarantees)
- **ID Validation**: `docs/CODING_STANDARDS.md` (DoS prevention)

## Acceptance Criteria

- [x] Recovery algorithm flowchart complete
- [x] Crash scenarios documented with guarantees
- [x] Bi-temporal semantics explained with examples
- [x] Testing strategy defined for all levels
- [x] Performance targets specified
- [x] Implementation phases outlined

---

**Next Steps**: Proceed to Issue #287 (Implement Basic WAL Replay Loop)
