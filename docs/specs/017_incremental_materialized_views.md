# 🔭 Vantage Spec: Incremental Materialized Views

| Metadata | Details |
| :--- | :--- |
| **ID** | SPEC-017 |
| **Status** | 📝 Draft |
| **Owner** | Vantage (Product) |
| **Implementer** | Core (Engineering) |
| **Priority** | P1 (High) |
| **Related Code** | `src/query/views/` (Proposed) |

## 1. 👤 User Stories

> **As a** BI Analyst or AI Agent orchestrator,
> **I want to** define complex temporal graph queries as continuously updating views,
> **So that** I can instantly retrieve the current state of a complex, expensive query without executing a full scan or traversal over the graph upon every read request.

## 2. 🧐 The "So What?" (Business Value)

Currently, applications relying on heavy aggregation or multi-hop temporal traversals face significant latency per query. Running complex AQL queries over millions of historical nodes repeatedly wastes compute resources and causes dashboard or agent response times to spike.

**The Gap:**
- **Read Latency:** Complex aggregations and traversals are evaluated on the fly.
- **Compute Inefficiency:** Repeatedly evaluating the same query over mostly-unchanged data wastes CPU cycles.

**ROI:**
- **Instant Insights:** Incremental Materialized Views solve this by precomputing the results and intelligently updating only the affected portions of the view when new relevant transactions are committed. This shifts the compute cost from read-time to write-time.
- **Performance:** Drops query latency from seconds to sub-millisecond, enabling near-instantaneous complex read access.

## 3. ✅ Acceptance Criteria

### Functional Requirements

1.  **View Definition API**:
    -   Must define an API to create a Materialized View using an existing AQL query string.
2.  **Incremental Maintenance**:
    -   Must incrementally update the view in the background upon relevant commits, avoiding full recalculation of the query.
    -   Must handle updates, creations, and deletions in the underlying base tables (nodes/edges) that match the view's query pattern.
3.  **Read API**:
    -   Must expose the view as a queryable entity (e.g., capable of being read via `db.read_view("view_name")`).

### Non-Functional Requirements
-   **Metric Definition:**
    -   **Query Latency:** Reading from a materialized view takes <1ms (p99), matching the performance of a direct node lookup.
    -   **Write Amplification Limit:** Updating a view with a single relevant transaction increases transaction commit time by no more than 15%.
    -   **Correctness:** The view’s state exactly matches the state of running the full AQL query on the underlying graph at any given transaction time.

## 4. 🚫 Out of Scope (Phase 1)

-   **Temporal Views**: Time-traveling queries against the materialized view itself (e.g., asking what the view looked like 5 days ago). The MVP view only represents the "Current State" resulting from the query.
-   **Distributed Views**: Cross-shard distributed materialized views (Phase 2).
-   **Auto-Optimization**: Automatic index selection for view optimizations. MVP relies on explicit query definition.
