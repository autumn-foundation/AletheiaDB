# 🔭 Vantage: Spec for Temporal Aggregation & Grouping

| Metadata | Details |
| :--- | :--- |
| **ID** | SPEC-008 |
| **Status** | 🔍 Review |
| **Owner** | Vantage (Product) |
| **Implementer** | Nova (Engineering) |
| **Priority** | P1 (High) |
| **Related Code** | `src/query/` |

## 1. 👤 User Stories

> **As a** Business Analyst or Data Scientist,
> **I want to** group query results by time windows and calculate aggregates (AVG, SUM, COUNT, MIN, MAX),
> **So that** I can analyze trends and patterns over time directly within the database, without transferring large volumes of raw data.

## 2. 🧐 The "So What?" (Business Value)

Currently, AletheiaDB supports powerful bi-temporal and vector queries. However, it lacks the ability to **aggregate** results. Users can filter and traverse, but they cannot answer questions like:

- "What is the average transaction value per day?"
- "How many new users signed up each month?"
- "What is the maximum price of a product in category X over the last year?"

This forces users to fetch all raw data and perform aggregation client-side, which is inefficient for large datasets.

**ROI:**
- **Efficiency:** Reduces network traffic and client-side processing by pushing aggregation to the database engine.
- **Scalability:** Allows analysis of large historical datasets that would be too big to fit in client memory.
- **Usability:** Simplifies analytical queries, making AletheiaDB a viable solution for time-series analysis and BI dashboards.

## 3. ✅ Acceptance Criteria

### Functional Requirements

1. **GROUP BY Clause**:
   - Parser supports `GROUP BY` clause.
   - Query engine executes `GROUP BY` on node properties (e.g., `n.category`).

2. **Aggregation Functions**:
   - Parser supports aggregation functions in `RETURN` clause.
   - Support `COUNT(*)`, `COUNT(n.prop)`, `SUM(n.prop)`, `AVG(n.prop)`, `MIN(n.prop)`, `MAX(n.prop)`.
   - Functions return correct results based on grouped sets.

3. **Temporal Grouping**:
   - Query engine executes `GROUP BY` on time windows (`time.window`).
   - Example syntax support: `RETURN time.window('1 day') as day, SUM(n.amount) as total_amount GROUP BY day`.

4. **Ordering**:
   - `ORDER BY` works correctly on aggregated columns.

### Non-Functional Requirements
- **Performance:** Aggregations should stream efficiently and not cause OOM on large result sets.
- **Documentation:** Documentation updated with syntax and examples.

## 4. 🚫 Out of Scope (Phase 1)

- **Distributed Aggregation:** Phase 2 will address aggregation across sharded clusters.
- **Window Functions:** Complex window functions (e.g., `LAG`, `LEAD`, `RANK` over partition) are deferred to a future release.
- **Streaming Aggregation:** Real-time continuous aggregation on incoming data streams is out of scope for this initial implementation.

## 5. 📝 Gap Analysis (Current vs. Spec)

| Feature | Current State | Required State | Action |
| :--- | :--- | :--- | :--- |
| **Parser** | Missing `GROUP BY` | Supports `GROUP BY` & aggregates | Update AQL grammar & AST |
| **Executor** | Individual rows | Aggregated streams | Implement grouping logic |
| **Temporal** | Point-in-time / Ranges | Time buckets | Implement `time.window` logic |

## 6. 📅 Execution Plan

1. **Parser Updates**: Extend AQL parser to recognize `GROUP BY` and standard aggregation functions.
2. **Query Plan**: Update IR and Query Planner to include an `Aggregate` operator.
3. **Execution**: Implement the physical grouping and reduction operations in the executor.
4. **Time Windows**: Add `time.window()` utility mapping timestamps to intervals.
5. **Testing**: Add unit and integration tests covering various aggregation scenarios.
