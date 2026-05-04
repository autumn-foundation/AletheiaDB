# 🔭 Vantage Spec: Consistent Snapshots

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
> **I want to** back up my database without halting all write operations,
> **So that** I can achieve zero downtime while ensuring my backups are perfectly consistent across all internal systems.

> **As a** Data Engineer running continuous ETL pipelines,
> **I want** point-in-time reads and snapshots to always reflect a mathematically valid state,
> **So that** concurrent writes do not cause partial updates to leak into my analytical queries or replicas.

## 2. 🧐 The "So What?" (Business Value)

Currently, AletheiaDB creates current and historical snapshots independently and sequentially without proper write coordination. This creates a race condition window where concurrent writes can alter the state between the two snapshot operations, resulting in inconsistent backups or point-in-time reads.

**The Gap:**
- **Standard Market Expectation (e.g., PostgreSQL, RocksDB):** Snapshots are guaranteed to be atomic across the entire database state.
- **Current State:** AletheiaDB allows writes to occur between current and historical snapshot creation, leading to torn state.

**ROI:**
- **Reliability:** Eliminates the risk of corrupted backups and inconsistent read replicas.
- **Enterprise Readiness:** Fulfills a fundamental ACID requirement for transactional consistency during concurrent workloads.
- **Operational Confidence:** Operators can run backups during peak hours without fearing data anomalies.

## 3. ✅ Acceptance Criteria

### Functional Requirements
1.  **Atomic Snapshot Creation:**
    - The process of capturing the current state and the historical state MUST be strictly coordinated.
    - Concurrent writes MUST NOT be able to insert, update, or delete data such that the current snapshot and historical snapshot reflect different states for the same logical time.
2.  **No Deadlocks:**
    - The snapshot coordination mechanism MUST NOT introduce deadlocks under high write concurrency.

### Non-Functional Requirements
-   **Performance/Throughput:** The coordination required for snapshot creation MUST NOT block writers for more than 5ms under standard loads.
-   **Metric Definition:** Success = `test_concurrent_write_during_snapshot_creation` passes 100% of the time, and creating a snapshot on a 10GB database blocks writes for < 5ms.

## 4. 🚫 Out of Scope (Phase 1)

-   **Distributed Snapshots:** Coordinating snapshots across multiple physical shards or nodes in a distributed cluster. This specification only applies to a single-node instance.
-   **Incremental Backups:** We are addressing the atomic capture of the state, not the efficient export of differences between two snapshots.

## 5. 📝 Gap Analysis (Current vs. Spec)

| Feature | Current State | Required State | Action |
| :--- | :--- | :--- | :--- |
| **Snapshot Atomicity** | Sequential, vulnerable to race conditions | Atomic, coordinated capture | Implement snapshot coordination |
| **Data Consistency** | Possible torn state between current/historical | Guaranteed consistent state | Update snapshot generation logic |
