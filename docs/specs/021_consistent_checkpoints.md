# 🔭 Vantage Spec: Consistent Checkpoint Snapshots

## 1. 👤 User Stories

> **As a** Database Administrator,
> **I want** checkpoints to represent a single, globally consistent point in time across both current and historical storage,
> **So that** in the event of a crash, my restored database is mathematically consistent and contains no orphaned temporal records.

> **As an** Operations Engineer,
> **I want** the database to take consistent backups without blocking read/write traffic for long periods,
> **So that** I can maintain high availability while still meeting my recovery point objectives (RPO).

## 2. 🧐 The "So What?" (Business Value)

Currently, AletheiaDB creates checkpoints by snapshotting `CurrentStorage` and `HistoricalStorage` sequentially. There is a race condition window where concurrent writes can be applied to historical storage *after* the current snapshot is taken, but *before* the historical snapshot completes.

**The Gap:**
- **Data Integrity:** This race condition results in a checkpoint where historical versions reference nodes that do not exist in the current snapshot (orphaned temporal records). Upon recovery, time-travel queries will return corrupted or impossible states.
- **Reliability:** A database whose recovery mechanism randomly corrupts data under high write load fundamentally violates ACID guarantees.

**ROI:**
- **Trust & Reliability:** Guarantees that backups and recovery points are 100% consistent, preventing catastrophic data corruption during disaster recovery.
- **Operational Confidence:** Operators can run continuous write workloads without fear that background checkpointing will silently corrupt their backups.

## 3. ✅ Acceptance Criteria

### Functional Requirements

1.  **Atomic Snapshotting:**
    - The system MUST coordinate the creation of snapshots across both `CurrentStorage` and `HistoricalStorage` such that no write operations can land between the two snapshots.
    - A checkpoint at Log Sequence Number (LSN) X MUST contain all writes up to and including LSN X, and NO writes from LSN X+1 or higher, across both storage engines.

2.  **No Orphaned Records:**
    - A restored database from a checkpoint MUST NEVER contain historical versions that reference non-existent current nodes or edges.
    - The `test_snapshots_created_sequentially_without_coordination` test MUST be removed or updated to verify coordinated, atomic snapshotting.

### Non-Functional Requirements

-   **Performance:** The write-lock or synchronization window required to coordinate the two snapshots MUST be minimal (e.g., < 1ms), ensuring that write throughput is not significantly impacted during checkpointing.
-   **Metric Definition:** Success = 0 orphaned records found when validating a checkpoint taken during a load test of 10,000 concurrent writes/sec.

## 4. 🚫 Out of Scope (Phase 1)

-   **Distributed Snapshots:** Coordinating snapshots across multiple shards or physical nodes is deferred to the Sharding (Phase 2) milestone.
-   **Incremental Backups:** While consistent, checkpoints will still be full snapshots. Incremental snapshotting (only capturing pages changed since the last checkpoint) is out of scope.
