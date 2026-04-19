# 🔭 Vantage: Spec for Incremental Materialized Views

## 👤 User Story
**As a** BI Analyst or AI Agent orchestrator,
**I want** to define complex temporal graph queries as continuously updating views,
**so that** I can instantly retrieve the current state of a complex, expensive query without executing a full scan or traversal over the graph upon every read request.

## 🧐 The "So What?" Ask
**What business problem does this solve?**
Currently, applications relying on heavy aggregation or multi-hop temporal traversals face significant latency per query. Running complex AQL queries over millions of historical nodes repeatedly wastes compute resources and causes dashboard or agent response times to spike. Incremental Materialized Views solve this by precomputing the results and intelligently updating only the affected portions of the view when new relevant transactions are committed. This shifts the compute cost from read-time to write-time, dropping query latency from seconds to sub-millisecond, enabling near-instantaneous complex read access.

**Success Metric Definition:**
- **Query Latency:** Reading from a materialized view takes <1ms (p99), matching the performance of a direct node lookup.
- **Write Amplification Limit:** Updating a view with a single relevant transaction increases transaction commit time by no more than 15%.
- **Correctness:** The view’s state exactly matches the state of running the full AQL query on the underlying graph at any given transaction time.

## ✅ Acceptance Criteria
- Must define an API to create a Materialized View using an existing AQL query string.
- Must incrementally update the view in the background upon relevant commits, avoiding full recalculation of the query.
- Must expose the view as a queryable entity (e.g., capable of being read via `db.read_view("view_name")`).
- Must handle updates, creations, and deletions in the underlying base tables (nodes/edges) that match the view's query pattern.

## 🚫 Out of Scope
- Time-traveling queries against the materialized view itself (e.g., asking what the view looked like 5 days ago). The MVP view only represents the "Current State" resulting from the query.
- Cross-shard distributed materialized views (Phase 2).
- Automatic index selection for view optimizations. MVP relies on explicit query definition.
