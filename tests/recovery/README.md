# Recovery Test Suite

This directory contains comprehensive integration tests for AletheiaDB's WAL (Write-Ahead Log) replay and recovery system.

## Overview

The recovery tests verify that the database can correctly restore its state after a crash or restart by replaying operations from the WAL. These tests are critical for ensuring ACID guarantees and data durability.

## Test Organization

Tests are organized by operation type, with each file covering a specific aspect of recovery:

### `replay_create_tests.rs` (Issue #288)
**Purpose:** Verify CreateNode and CreateEdge operations are correctly replayed during recovery.

**Test Coverage:**
- ✅ Basic node creation with label preservation
- ✅ Basic edge creation with source/target preservation
- ✅ Property handling (strings, integers, booleans, vectors)
- ✅ Vector embedding indexing during replay
- ✅ Bi-temporal interval preservation
- ✅ Node/Edge ID tracking
- ✅ Multiple creates in sequence
- ✅ Historical storage version creation

**Key Tests:**
- `test_replay_create_node_basic` - Single node creation
- `test_replay_create_node_with_properties` - Property preservation
- `test_replay_create_node_with_vector` - Vector indexing during recovery
- `test_replay_create_edge_basic` - Edge creation with relationships

---

### `replay_update_tests.rs` (Issue #289)
**Purpose:** Verify UpdateNode and UpdateEdge operations correctly create version chains during recovery.

**Test Coverage:**
- ✅ Node/edge property updates
- ✅ Label changes
- ✅ Version chain creation in historical storage
- ✅ Transaction time management
- ✅ Vector embedding updates and re-indexing
- ✅ Multiple updates to same entity
- ✅ Mixed creates and updates

**Key Tests:**
- `test_replay_update_node_basic` - Simple property update
- `test_replay_update_node_label_change` - Label modification
- `test_replay_update_node_with_vector` - Vector re-indexing
- `test_replay_multiple_updates_same_node` - Version chain building

---

### `replay_delete_tests.rs` (Issue #290)
**Purpose:** Verify DeleteNode and DeleteEdge operations correctly create tombstones and support time-travel queries.

**Test Coverage:**
- ✅ Node/edge deletion from current storage
- ✅ Tombstone creation in historical storage
- ✅ Transaction time closure for previous versions
- ✅ Time-travel query support after deletion
- ✅ Vector de-indexing during deletion
- ✅ Multiple deletes in sequence
- ✅ Mixed create/update/delete operations

**Key Tests:**
- `test_replay_delete_node_basic` - Simple deletion
- `test_replay_delete_node_after_update` - Deletion of updated entity
- `test_replay_delete_with_vector` - Vector index cleanup
- `test_replay_mixed_creates_updates_deletes` - Complex operation sequences

---

### `replay_id_tracking_tests.rs` (Issue #291)
**Purpose:** Verify ID generators are correctly initialized after recovery to prevent ID conflicts.

**Test Coverage:**
- ✅ Node ID generator initialization from max observed ID
- ✅ Edge ID generator initialization from max observed ID
- ✅ Version ID generator initialization from max observed ID
- ✅ Handling gaps in ID sequences
- ✅ Independence of different ID generators
- ✅ Deleted entity ID handling (no reuse)
- ✅ Empty WAL initialization (start from 0)

**Key Tests:**
- `test_recover_initializes_node_id_generator` - Node ID continuity
- `test_recover_initializes_edge_id_generator` - Edge ID continuity
- `test_recover_initializes_version_id_generator` - Version ID continuity
- `test_recover_handles_gaps_in_ids` - Non-sequential ID handling
- `test_recover_all_generators_independent` - Generator independence

---

### `replay_loop_tests.rs` (Issue #287)
**Purpose:** Verify the core WAL replay loop correctly processes all operation types and tracks state.

**Test Coverage:**
- ✅ Empty WAL handling (graceful no-op)
- ✅ Multiple operation types in single recovery
- ✅ Checkpoint marker processing
- ✅ LSN (Log Sequence Number) tracking
- ✅ Max ID tracking across all operation types
- ✅ Non-sequential version ID handling
- ✅ Recovery from specific checkpoint LSN
- ✅ Invalid operation rejection

**Key Tests:**
- `test_recover_with_empty_wal` - Empty WAL scenario
- `test_recover_with_multiple_operation_types` - Mixed operations
- `test_recover_from_checkpoint_lsn` - Partial replay from checkpoint
- `test_recover_handles_checkpoint_marker` - Checkpoint tracking

---

## Running Tests

### Run All Recovery Tests
```bash
cargo test --test recovery
```

### Run Specific Test Module
```bash
# CreateNode/CreateEdge tests
cargo test --test recovery replay_create_tests

# UpdateNode/UpdateEdge tests
cargo test --test recovery replay_update_tests

# DeleteNode/DeleteEdge tests
cargo test --test recovery replay_delete_tests

# ID tracking tests
cargo test --test recovery replay_id_tracking_tests

# Replay loop tests
cargo test --test recovery replay_loop_tests
```

### Run Individual Test
```bash
cargo test --test recovery test_replay_create_node_basic
```

### Run with Output
```bash
cargo test --test recovery -- --nocapture
```

### Run in Parallel (default)
```bash
cargo test --test recovery -- --test-threads=8
```

### Run Sequentially (for debugging)
```bash
cargo test --test recovery -- --test-threads=1
```

## Test Statistics

| Module | Tests | Operations Covered |
|--------|-------|-------------------|
| `replay_create_tests` | 8 | CreateNode, CreateEdge |
| `replay_update_tests` | 7 | UpdateNode, UpdateEdge |
| `replay_delete_tests` | 6 | DeleteNode, DeleteEdge |
| `replay_id_tracking_tests` | 7 | ID generator recovery |
| `replay_loop_tests` | 10 | WAL replay loop, checkpoints |
| **Total** | **38** | **All recovery operations** |

## Coverage Requirements

These tests contribute to the overall project coverage requirements:
- **Line Coverage:** ≥85% (currently 86.45%)
- **Function Coverage:** ≥88% (currently 89.10%)
- **Region Coverage:** ≥88% (currently 88.91%)

## Key Concepts

### WAL (Write-Ahead Log)
The WAL records all database operations before they're applied to storage. During recovery, the WAL is replayed to restore the database to its last consistent state.

### Replay
The process of re-executing WAL operations during recovery. Each operation handler must be idempotent and correctly update both current and historical storage.

### Tombstones
Special version entries created during deletion that mark when an entity was deleted. Tombstones enable time-travel queries to see entities that existed in the past but have been deleted.

### ID Generators
Atomic counters for NodeId, EdgeId, and VersionId. After recovery, generators must be initialized to `max_observed_id + 1` to prevent ID conflicts with existing entities.

### Bi-temporal Semantics
AletheiaDB tracks both **valid time** (when facts were true in reality) and **transaction time** (when facts were recorded). Recovery must preserve both temporal dimensions.

## Testing Strategy

### Given-When-Then Pattern
All tests follow the Given-When-Then (GWT) pattern for clarity:

```rust
#[test]
fn test_replay_create_node_basic() -> Result<()> {
    // Given: WAL with single CreateNode operation
    let wal = setup_wal_with_create_node();

    // When: recover()
    let (current, historical, lsn) = manager.recover(&wal)?;

    // Then: Node exists in current storage
    assert_eq!(current.node_count(), 1);

    Ok(())
}
```

### Temporary Directories
All tests use `tempfile::TempDir` to create isolated test environments that are automatically cleaned up.

### Error Handling
Tests use `Result<()>` return types and the `?` operator for clean error propagation. Failed assertions provide clear error messages.

## Integration with CI/CD

These tests run automatically in the CI/CD pipeline:
1. On every commit to feature branches
2. On pull request creation/update
3. Before merging to trunk

**Acceptance Criteria:**
- ✅ All 38 tests must pass
- ✅ No clippy warnings
- ✅ Coverage thresholds maintained
- ✅ Tests complete in <2 minutes

## Related Issues

- **Issue #287** - WAL Replay Loop Implementation
- **Issue #288** - CreateNode/CreateEdge Replay Handlers
- **Issue #289** - UpdateNode/UpdateEdge Replay Handlers
- **Issue #290** - DeleteNode/DeleteEdge Replay Handlers
- **Issue #291** - ID Generator Recovery
- **Issue #292** - Vector Index Recovery
- **Issue #293** - Test Organization (this reorganization)

## Future Enhancements

Potential improvements to the recovery test suite:

1. **Property-Based Testing**
   - Use `proptest` to generate random operation sequences
   - Verify invariants hold for any valid sequence

2. **Performance Benchmarks**
   - Measure recovery time for various WAL sizes
   - Track regression in recovery performance

3. **Fault Injection**
   - Test recovery with corrupted WAL entries
   - Verify error handling and partial recovery

4. **Concurrent Recovery**
   - Test recovery while reads/writes are happening
   - Verify isolation and consistency

5. **Large-Scale Recovery**
   - Test recovery with millions of operations
   - Verify memory efficiency

## Contributing

When adding new recovery tests:

1. **Choose the correct module** based on the operation type
2. **Follow the GWT pattern** for test structure
3. **Use descriptive test names** that explain what's being tested
4. **Include comments** explaining the "why" of assertions
5. **Clean up resources** using `TempDir` or similar
6. **Update this README** if adding new test categories

## Questions?

For questions about the recovery system or tests:
- See [docs/WAL.md](../../docs/WAL.md) for WAL architecture
- See [docs/ARCHITECTURE.md](../../docs/ARCHITECTURE.md) for bi-temporal design
- See [src/storage/checkpoint.rs](../../src/storage/checkpoint.rs) for recovery implementation
