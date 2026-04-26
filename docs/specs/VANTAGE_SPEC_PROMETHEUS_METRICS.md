# 🔭 Vantage: Spec for Comprehensive Prometheus Metrics Suite

## 👤 User Story
**As an** SRE / DevOps Engineer maintaining a production AletheiaDB cluster,
**I want** a comprehensive suite of Prometheus metrics covering query performance, vector search latencies, temporal index health, and resource utilization,
**so that** I can build reliable Grafana dashboards, set up alerts for degradations, and pinpoint whether performance regressions stem from graph traversal, vector similarity, or temporal operations.

## 🧐 The "So What?" Ask
**What business problem does this solve?**
Currently, AletheiaDB has a stub Prometheus implementation that only exports high-level application state. In production, customers need granular visibility into specific database subsystems to run safely at scale. Without rich metrics, troubleshooting a slow query involves manual log analysis and profiling, which is untenable in high-throughput enterprise environments.

**Metric Definition:**
- **Visibility:** Over 90% of database subsystem operations (WAL, HNSW Index, Graph Traversal, Current/Historical Storage) are instrumented with Prometheus counters/histograms.
- **Latency:** Exporting metrics via the HTTP endpoint must add `<1ms` of overhead per request under a load of 1,000 scrapes/second.

**Gap Analysis:**
Looking at the market, industry standard graph databases (like Neo4j) and vector databases (like Qdrant or Milvus) provide extensive, out-of-the-box Prometheus metrics that track query latency histograms, cache hit ratios, and memory allocation down to the subsystem level. We currently fall short of this enterprise standard, leaving operations teams blind during traffic spikes.

## ✅ Acceptance Criteria
- Must replace the existing Prometheus stub with a fully integrated metrics endpoint.
- Must expose Histograms for: Graph Traversal Latency, Vector Similarity Search Latency, and Hybrid Query execution times.
- Must expose Counters for: Read/Write transactions, Lock Poisons, WAL Checksum Failures, Cache Hits/Misses, and HNSW Node Insertions.
- Must expose Gauges for: Current Node Count, Historical Node Versions Count, Edge Count, Memory Allocation, and Active Transactions.
- Must be configurable (e.g., binding address, enabled/disabled state) via the standard configuration files.

## 🚫 Out of Scope
- OpenTelemetry (OTLP) exporting — this spec is strictly focused on Prometheus format.
- Built-in AlertManager integration — alerts will be configured externally by users within Prometheus/Grafana.
- Creating the actual Grafana JSON dashboards (though an example dashboard is highly encouraged later).
