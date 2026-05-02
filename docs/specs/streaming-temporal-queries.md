# Streaming Temporal Queries Specification

## The "So What?" Ask
**What business problem does this solve?**
Currently, queries against AletheiaDB retrieve entire snapshots of historical data or compute full diffs between two points in time. As datasets grow and temporal queries span larger intervals, this approach leads to massive memory overhead and unacceptable latency. By enabling streaming temporal queries, clients can incrementally process historical changes as they are retrieved, allowing applications—especially LLMs and real-time analytical dashboards—to reason over long histories without running out of memory or waiting for a massive batch operation to complete.

## Gap Analysis
Looking at the market, temporal databases like XTDB and Datomic excel at historical snapshot queries but often require pulling significant data into client memory for deep historical analysis. Streaming technologies like Kafka or Redpanda handle real-time event streams beautifully but lack graph-traversal capabilities and historical query features. AletheiaDB can bridge this gap by offering streamable results for *historical graph queries*, providing a unique advantage for applications needing both temporal reasoning and memory-safe processing of large datasets.

## 👤 User Story
"As a Data Analyst building real-time compliance dashboards, I want to stream historical changes to financial entities over time, so that I can process massive audit logs incrementally without exhausting memory or waiting minutes for a single monolithic query to return."

## ✅ Acceptance Criteria
- **Incremental Delivery:** The database must be able to return temporal query results as a stream of events rather than a single batch response. Results must be delivered in strictly chronological order based on transaction time.
- **Memory Efficiency:** The memory footprint on the database node for a streaming query should remain relatively constant, regardless of the time window size.
- **Cancellation & Backpressure:** The streaming API must support cancellation by the client and backpressure, ensuring the database doesn't generate events faster than the client can consume them.
- **Latency Success Metric:** Time-to-first-event for a streaming temporal query must be under 50ms, with sustained event delivery maintaining sub-millisecond per-event generation overhead.

## 🚫 Out of Scope
- **Streaming Current-State Queries:** This feature is strictly focused on temporal queries (historical changes). Streaming standard graph traversals over the current state is not part of this specification.
- **Complex Aggregation Functions:** Initially, the streaming results will be individual change records (deltas) or versioned snapshots. Complex windowed aggregations (like moving averages over time) processed server-side are deferred to a later phase.
- **Push-based Publish/Subscribe (Pub/Sub):** This specification covers pulling historical data as a stream in response to a query, not real-time push subscriptions to ongoing live changes.
