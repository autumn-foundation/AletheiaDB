# GallifreyDB Metrics Reference

This document details the Prometheus metrics exposed by GallifreyDB when the `observability-prometheus` feature is enabled.

## Histogram Metrics (Latency)

These metrics measure the duration of operations. Buckets are configured from 100µs to 10s to capture both high-performance vector searches and complex graph traversals.

| Metric Name | Description | Labels | SLO Target |
|-------------|-------------|--------|------------|
| `gallifreydb_operation_duration_seconds` | Latency of high-level database operations. | `op`: The operation name (e.g., `get_node_at_time`, `find_similar`, `execute_query`). | < 1ms for point lookups (p99)<br>< 10ms for vector search (p99) |
| `gallifreydb_transaction_commit_duration_seconds` | Latency of the transaction commit phase (including WAL flush). | None | < 5ms (Sync durability)<br>< 1ms (Group commit) |
| `gallifreydb_vector_search_duration_seconds` | Latency of internal HNSW index operations (pure search time). | `op`: `search` or `search_with_filter`. | < 5ms (p99) |

### Buckets
`[0.0001, 0.0005, 0.001, 0.005, 0.01, 0.025, 0.05, 0.1, 0.5, 1.0, 5.0, 10.0]`

## Counter Metrics (Throughput & Errors)

| Metric Name | Description | Labels |
|-------------|-------------|--------|
| `gallifreydb_transaction_operations_total` | Count of write operations within transactions. | `type`: `node` or `edge`<br>`op`: `create`, `update`, `delete` |
| `gallifreydb_lock_poison_total` | **CRITICAL**: Number of times a lock was poisoned (thread panic while holding lock). | None |
| `gallifreydb_timestamp_violations_total` | **CRITICAL**: Number of timestamp monotonicity violations. | None |
| `gallifreydb_wal_checksum_failures_total` | **CRITICAL**: Number of WAL corruption events. | None |
| `gallifreydb_write_conflicts_total` | Number of write-write conflicts detected (Snapshot Isolation). | None |
| `gallifreydb_errors_total` | Total count of errors returned to users. | `category`: `storage`, `temporal`, `query`, `transaction`, `vector`, `io`, `other` |

## Usage

Metrics are exposed via the `observability-prometheus` feature. Configure the bind address via `PROMETHEUS_BIND_ADDR` environment variable.

```bash
export PROMETHEUS_BIND_ADDR="0.0.0.0:9090"
./gallifrey-server
```

Then scrape `http://localhost:9090/metrics`.
