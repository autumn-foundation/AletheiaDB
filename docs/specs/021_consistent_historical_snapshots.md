# 🔭 Vantage Spec: Consistent Historical Snapshots

| Metadata | Details |
| :--- | :--- |
| **ID** | SPEC-021 |
| **Status** | 📝 Draft |
| **Owner** | Vantage (Product) |
| **Implementer** | Core (Engineering) |
| **Priority** | P1 (High) |
| **Related Code** | `tests/snapshot_race_condition.rs` |

## 1. 👤 User Stories

> **As a** Data Analyst,
> **I want to** query historical versions of the graph using `as_of` without encountering orphaned data or corrupted states,
> **So that** my backtesting and point-in-time reports are accurate and mathematically valid.

> **As a** Database Administrator,
> **I want** the system to coordinate write operations when generating snapshots,
> **So that** concurrent writes do not interleave with the snapshotting process, ensuring the database is not put into an inconsistent state.

> **As an** Auditor,
> **I want** absolute certainty that historical snapshots represent a true and complete state of the system at a specific time,
> **So that** compliance and financial reporting can be trusted.

## 2. 🧐 The "So What?" (Business Value)

Currently, AletheiaDB exhibits a race condition where snapshots are created sequentially without write coordination. As documented in `tests/snapshot_race_condition.rs` (`test_snapshots_created_sequentially_without_coordination`), this can lead to orphaned versions (a historical version referencing a node that does not exist or isn't consistent in the snapshot).

**The Gap:**
- **Data Integrity Risk:** Point-in-time queries (`as_of`) might return incorrect or structurally broken subgraphs due to interleaved writes.
- **Reliability:** This race condition breaks the core promise of a bi-temporal database (that historical views are perfectly preserved and accurate).

**ROI:**
- **Trust & Compliance:** Guarantees that historical queries return accurate, mathematically sound results, which is vital for use cases like financial auditing and backtesting.
- **System Stability:** Removing the race condition prevents subtle, hard-to-reproduce bugs in production, reducing support burden and improving overall system resilience.

## 3. ✅ Acceptance Criteria

### Functional Requirements
1. **Write Coordination:**
   - The snapshot generation process MUST coordinate with active write transactions to ensure that it captures a transactionally consistent state.
   - Snapshots MUST NOT capture partial writes or interleaved changes that occur during the snapshot generation process.

2. **No Orphaned Versions:**
   - Historical snapshots MUST be completely structurally valid. A historical edge MUST NOT reference a historical node that does not exist within the context of that exact snapshot time.
   - The `TODO: After fix, add validation that current and historical snapshots are consistent (no orphaned versions)` in `tests/snapshot_race_condition.rs` MUST be addressed by adding the necessary assertions.

### Non-Functional Requirements
- **Performance:** Write coordination for snapshotting MUST NOT severely degrade the throughput of ongoing write transactions (e.g., avoid long global write locks if possible, utilizing MVCC mechanisms instead).
- **Metric Definition:** Success = Snapshot creation completes without corrupting concurrent writes, and queries against the created snapshot return 0 orphaned relations.

## 4. 🚫 Out of Scope (Phase 1)

- **Distributed Snapshots:** Coordinating consistent snapshots across multiple distributed shards (Phase 2).
- **Incremental Snapshots:** Generating partial snapshots (diffs). This spec focuses on fixing the consistency of the current snapshot mechanism.

## 5. 📝 Gap Analysis (Current vs. Spec)

| Feature | Current State | Required State | Action |
| :--- | :--- | :--- | :--- |
| **Snapshot Coordination** | Uncoordinated, sequential | Coordinated with writes | Introduce synchronization/MVCC barriers for snapshots |
| **Snapshot Integrity** | Subject to orphaned versions | Guaranteed consistent | Validate node/edge references within the snapshot |
| **Testing** | Documented limitation | Explicit assertions of correctness | Update `snapshot_race_condition.rs` to enforce consistency |