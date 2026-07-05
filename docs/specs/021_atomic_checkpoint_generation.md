# 🔭 Vantage Spec: Atomic Checkpoint Generation

| Metadata | Details |
| :--- | :--- |
| **ID** | SPEC-021 |
| **Status** | 📝 Draft |
| **Owner** | Vantage (Product) |
| **Implementer** | Core (Engineering) |
| **Priority** | P1 (High) |
| **Related Code** | `tests/snapshot_race_condition.rs` |

## 1. 👤 User Stories

> **As a** Database Administrator,
> **I want to** ensure that periodic checkpoints capture a perfectly consistent, single-point-in-time snapshot of the database,
> **So that** if the system crashes and recovers from a checkpoint, the graph data is not corrupted by partial writes that were interleaved during the checkpoint creation process.

> **As a** Data Engineer running continuous ETL pipelines,
> **I want** the database to handle heavy write loads concurrently with background checkpointing,
> **So that** our ingestion throughput is not artificially throttled or halted just because a backup or snapshot is being written to disk.

## 2. 🧐 The "So What?" (Business Value)

Currently, creating a checkpoint involves capturing a snapshot of the `CurrentStorage` and then capturing a snapshot of the `HistoricalStorage`. This process lacks strict write coordination. As documented in `tests/snapshot_race_condition.rs`, there is a critical race condition: a concurrent write transaction can commit data to the current storage *after* the current snapshot is taken, but *before* the historical snapshot is taken.

**The Gap:**
- **Data Integrity:** This race condition results in "orphaned versions"—the checkpoint may contain a historical version of a node that does not correspond to any valid node state in the current snapshot, violating the bi-temporal model's core consistency guarantees.
- **Reliability:** Restoring from a corrupted checkpoint can lead to unpredictable application behavior and silent data anomalies.

**ROI:**
- **Trust & Reliability:** Guarantees absolute data consistency at rest, reinforcing the database's ACID properties.
- **Resilience:** Enables confident disaster recovery without the risk of restoring into a mathematically invalid graph state.

## 3. ✅ Acceptance Criteria

### Functional Requirements

1.  **Atomic Snapshot Acquisition:**
    - The `CurrentStorage` and `HistoricalStorage` snapshots MUST be acquired atomically relative to concurrent write transactions.
    - It must be impossible for a write transaction to commit its changes to only one of the storage engines while the checkpoint snapshot is being constructed.
2.  **No Orphaned Versions:**
    - A newly generated checkpoint MUST NOT contain historical versions that reference nodes or edges missing from the current state snapshot (unless explicitly intended by a valid transaction).
3.  **Concurrency Preservation:**
    - The checkpointing mechanism MUST NOT introduce global write locks that significantly pause transaction processing. The atomic coordination should only briefly lock or synchronize the specific boundary where the snapshots are instantiated.

### Non-Functional Requirements

-   **Metric Definition:** Success = The test `test_concurrent_write_during_snapshot_creation` in `tests/snapshot_race_condition.rs` runs 1,000 times without failing or producing a checkpoint with mismatched node counts.
-   **Performance:** The atomic coordination overhead during snapshot acquisition must be `< 1ms`.

## 4. 🚫 Out of Scope (Phase 1)

-   **Incremental Checkpointing:** Generating delta checkpoints rather than full snapshots. This spec focuses solely on the atomicity of the existing full checkpoint process.
-   **Distributed Checkpoint Coordination:** Coordinating snapshots across multiple shards in a distributed cluster (Sharding). Phase 1 only applies to single-machine, unified storage.

## 5. 📝 Gap Analysis (Current vs. Spec)

| Feature | Current State | Required State | Action |
| :--- | :--- | :--- | :--- |
| **Snapshot Coordination** | Uncoordinated (sequential) | Atomic (synchronized) | Implement a transaction-aware snapshot barrier or multi-storage lock. |
| **Data Consistency** | Race condition exists | Perfectly consistent | Fix the race window identified in `test_snapshots_created_sequentially_without_coordination`. |
| **Test Coverage** | Test documents the bug | Test enforces correctness | Update the test to assert no orphaned versions and fail if the race condition occurs. |
