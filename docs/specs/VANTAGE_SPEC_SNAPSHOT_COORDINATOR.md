# 🔭 Vantage: Spec for Snapshot Coordinator

| Metadata | Details |
| :--- | :--- |
| **ID** | SPEC-021 |
| **Status** | 🔍 Review |
| **Owner** | Vantage (Product) |
| **Implementer** | Core (Engineering) |
| **Priority** | P1 (High) |
| **Related Code** | `src/storage/checkpoint.rs` |

## 1. 👤 User Stories

> **As a** Database Operator,
> **I want to** ensure that periodic checkpoints capture a strictly consistent view of the database,
> **So that** when I recover from a crash or restart the server, the temporal graph does not contain corrupted or orphaned historical versions.

> **As an** AI Application Developer,
> **I want to** rely on the bi-temporal history queries returning correct data,
> **So that** my reasoning LLM doesn't base decisions on inconsistent "phantom" historical snapshots caused by internal race conditions.

## 2. 🧐 The "So What?" (Business Value)

Currently, AletheiaDB creates checkpoints by sequentially snapshotting the current storage and then the historical storage. As documented in `docs/SNAPSHOT_RACE_CONDITION.md`, a small (1-10µs) race condition window exists where concurrent writes can be captured in the historical snapshot but missed by the current snapshot.

**The Gap:**
- **Data Integrity Risk:** Concurrent writes during the checkpoint window can create orphaned historical versions (a historical version that points to a node that doesn't exist in the current snapshot).
- **Recovery Corruption:** Recovering from an inconsistent checkpoint violates AletheiaDB's temporal integrity guarantees.

**ROI:**
- **Rock-Solid Reliability:** Fixing this guarantees ACID consistency for periodic checkpoints, which is mandatory before deploying background checkpointing to production.
- **Trust:** Developers and operators can trust that their bi-temporal graph will never exhibit torn state.

## 3. ✅ Acceptance Criteria

### Functional Requirements

1.  **Atomic Snapshot Creation**:
    - The `CheckpointManager` must create both the `current_snapshot` and `historical_snapshot` atomically with respect to write operations.
    - No write operation can occur *between* the creation of the two snapshots.
2.  **Snapshot Coordinator Implementation**:
    - Introduce a lightweight `SnapshotCoordinator` (e.g., using an `RwLock<()>`) to coordinate writes and checkpoints.
    - All write operations must acquire a read lock from the coordinator (allowing concurrent writes during normal operation).
    - `CheckpointManager::create_checkpoint()` must acquire a write lock, blocking all writes solely for the duration of the snapshot creation.
3.  **Test Validation**:
    - The existing `test_concurrent_write_during_snapshot_creation` in `tests/snapshot_race_condition.rs` must be enabled and pass reliably without reporting orphaned versions.

### Non-Functional Requirements

-   **Performance Metrics (Success Definition):**
    - The overhead added to normal write operations by the read lock must be negligible (< 10ns per write).
    - The overall throughput reduction must be `< 0.01%`.
    - The write lock acquired during checkpointing must block writes for `< 10ms`.

## 4. 🚫 Out of Scope (Phase 1)

-   **Background Checkpoint Implementation**: This spec only covers fixing the race condition in the checkpoint creation logic. It does not cover the scheduling or background threading of the checkpoints themselves.
-   **Alternative Consistency Models**: We will stick to the proposed `RwLock` Snapshot Coordinator solution. Complex, lock-free LSN filtering mechanisms are deferred unless performance dictates otherwise.
