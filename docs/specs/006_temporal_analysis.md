# 🔭 Vantage Spec: Temporal Analysis (Chronos)

| Metadata | Details |
| :--- | :--- |
| **ID** | SPEC-006 |
| **Status** | 🔍 Review |
| **Owner** | Vantage (Product) |
| **Implementer** | Nova (Engineering) |
| **Priority** | P2 (Core Differentiator) |
| **Related Code** | `src/experimental/chronos.rs` (Experimental) |

## 1. 👤 User Stories

> **As a** Supply Chain Risk Manager,
> **I want to** measure the "volatility" of supplier nodes and the "stability" of logistics paths over the last year,
> **So that** I can identify reliable partners and diversify away from suppliers with frequent status changes or unstable connections.

> **As a** Fraud Investigator,
> **I want to** replay the exact state of a transaction graph at the moment a suspicious payment occurred (`find_path_at_time`),
> **So that** I can prove whether a valid path between the sender and a known blacklisted entity existed *at that specific microsecond*.

## 2. 🧐 The "So What?" (Business Value)

GallifreyDB's "Time Travel" feature (querying *at* a time) is powerful, but passive.
**Temporal Analysis** turns this into an active analytical tool.

**The Gap:**
Knowing *that* a graph changed is not enough. We need to know *how much* it changes and *how reliable* its structures are.
-   A path that exists now but didn't exist 5 minutes ago is "Unstable".
-   A node that changes properties every second is "Volatile".

**ROI:**
-   **Predictive Power**: Volatility metrics are leading indicators of risk (churn, failure, fraud).
-   **Auditability**: "Snapshot Pathfinding" provides mathematically provable historical connectivity, critical for compliance (KYC/AML).
-   **Stickiness**: These metrics are hard to replicate in standard databases without massive ETL jobs.

## 3. ✅ Acceptance Criteria

### Functional Requirements
1.  **Snapshot Pathfinding**:
    -   `find_path_at_time(start, end, valid_time, tx_time)`: Must return a path only if *every edge and node* in the path was valid at the specified bi-temporal coordinate.
    -   Must handle "phantom paths" (paths that look like they exist because we overlay all history, but never actually existed simultaneously).

2.  **Volatility Metric**:
    -   `node_volatility(node_id, time_window)`: Returns `Updates / Second`.
    -   Must account for the density of changes within the window.

3.  **Path Stability Metric**:
    -   `path_stability(path, time_window)`: Returns a score `0.0` to `1.0`.
    -   Score = (Total duration where *all* edges in path were simultaneously valid) / (Window duration).
    -   1.0 = Path existed continuously for the entire window.
    -   0.0 = Path never existed as a connected whole (even if parts existed).

### Non-Functional Requirements
-   **Performance**: Path stability calculation should not require re-fetching full node history if cached time-indexes are available.
-   **Accuracy**: Must use exact microsecond precision from the bi-temporal timestamps.

## 4. 🚫 Out of Scope (Phase 1)

-   **Trend Forecasting**: "Will this path break soon?" (See `Dreamer` / Semantic Trajectory).
-   **Continuous Pathfinding**: Finding the "longest living path" between A and B (optimizing for stability instead of length).

## 5. 📝 Gap Analysis (Current vs. Spec)

| Feature | Current State | Required State | Action |
| :--- | :--- | :--- | :--- |
| **Code** | `src/experimental/chronos.rs` | Production Module (`src/analysis/temporal.rs`) | Promote from Experimental |
| **Pathfinding** | Implemented (BFS) | A* with Temporal Heuristics | Optimize |
| **Metrics** | Implemented (Basic) | Optimized | Optimize interval intersection logic |
| **API** | Rust Struct | Query Language (`STABILITY OF PATH ...`) | Expose in GQL |

## 6. 📅 Execution Plan

1.  **Review**: Validate `src/experimental/chronos.rs` against these requirements.
2.  **Optimize**: The current interval intersection in `path_stability` is O(N*M), optimize for sorted intervals.
3.  **Promote**: Move to core `gallifreydb` crate under `analysis` module.
4.  **Expose**: Add GQL keywords for `VOLATILITY` and `STABILITY`.
