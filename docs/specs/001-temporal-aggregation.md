# Temporal Aggregation & Grouping

**Author:** Vantage (Product Manager)
**Status:** Draft

## 1. Context

Currently, AletheiaDB supports powerful bi-temporal and vector queries. However, it lacks the ability to **aggregate** results. Users can filter and traverse, but they cannot answer questions like:

- "What is the average transaction value per day?"
- "How many new users signed up each month?"
- "What is the maximum price of a product in category X over the last year?"

This forces users to fetch all raw data and perform aggregation client-side, which is inefficient for large datasets.

## 2. User Story

**As a** Business Analyst or Data Scientist,
**I want to** group query results by time windows and calculate aggregates (AVG, SUM, COUNT, MIN, MAX),
**So that** I can analyze trends and patterns over time directly within the database, without transferring large volumes of raw data.

## 3. Business Value

- **Efficiency:** Reduces network traffic and client-side processing by pushing aggregation to the database engine.
- **Scalability:** Allows analysis of large historical datasets that would be too big to fit in client memory.
- **Usability:** Simplifies analytical queries, making AletheiaDB a viable solution for time-series analysis and BI dashboards.

## 4. Proposed Solution

Introduce a `GROUP BY` clause and aggregation functions to the AQL query language.

### Syntax

```cypher
MATCH (n:Label)
[WHERE predicate]
[BETWEEN start_time AND end_time]
RETURN grouping_key, AGG_FUNC(property)
GROUP BY grouping_key
[ORDER BY aggregate DESC]
[LIMIT n]
```

### Supported Aggregations

| Function | Description |
|----------|-------------|
| `COUNT(*)` | Count number of rows |
| `COUNT(n.prop)` | Count non-null values of property |
| `SUM(n.prop)` | Sum of numerical property values |
| `AVG(n.prop)` | Average of numerical property values |
| `MIN(n.prop)` | Minimum value of property |
| `MAX(n.prop)` | Maximum value of property |

### Temporal Grouping

Introduce a special function `time.window(duration)` for grouping by time buckets.

```cypher
MATCH (n:Transaction)
BETWEEN '2023-01-01' AND '2023-12-31'
RETURN time.window('1 day') as day, SUM(n.amount) as total_amount
GROUP BY day
ORDER BY day ASC
```

## 5. Acceptance Criteria

- [ ] Parser supports `GROUP BY` clause.
- [ ] Parser supports aggregation functions in `RETURN` clause.
- [ ] Query engine executes `GROUP BY` on node properties (e.g., `n.category`).
- [ ] Query engine executes `GROUP BY` on time windows (`time.window`).
- [ ] Aggregation functions (`COUNT`, `SUM`, `AVG`, `MIN`, `MAX`) return correct results.
- [ ] `ORDER BY` works on aggregated columns.
- [ ] Documentation updated with examples.

## 6. Out of Scope

- **Distributed Aggregation:** Phase 2 will address aggregation across sharded clusters.
- **Window Functions:** Complex window functions (e.g., `LAG`, `LEAD`, `RANK` over partition) are deferred to a future release.
- **Streaming Aggregation:** Real-time continuous aggregation on incoming data streams is out of scope for this initial implementation.

## 7. Migration Strategy

This is a purely additive feature. No migration required for existing data or queries.

---
**Vantage** 🔭
*Focus on Utility.*
