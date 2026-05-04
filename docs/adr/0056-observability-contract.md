# ADR-0056: Backend-Agnostic Observability Contract

**Status:** Accepted
**Date:** 2026-05-04
**Deciders:** AletheiaDB Core Team
**Categories:** observability, api, release-hardening

## Context

The previous observability surface mixed three concerns inside the database
crate: `tracing` subscriber setup, vendor-specific export, and a built-in scrape
endpoint. That made the release story muddy and forced the library to own
operational choices that belong in the host process.

Autumn Harvest uses a cleaner model: the crate defines telemetry contracts,
emits spans through `tracing`, and exposes a metrics trait with no-op defaults.
Exporters stay outside the core crate.

## Decision

AletheiaDB owns only the observability contract:

- Span names are stable constants in `src/observability/mod.rs`.
- Span attributes use OTel database semantic keys where applicable, with
  `aletheiadb.*` keys for database-specific data.
- Metrics are reported through `MetricsRecorder`.
- `TelemetryConfig` can install a process-wide metrics recorder, but does not
  install `tracing` subscribers.
- The optional `metrics-rs` feature forwards samples to the `metrics` facade.
- Vendor export, built-in scrape serving, and observability-Tracy feature flags
  are removed from the core crate.

## Span Catalogue

| Span constant | Span name | Purpose |
|---------------|-----------|---------|
| `SPAN_QUERY_EXECUTE` | `aletheiadb.query.execute` | Query parse/plan/execute work |
| `SPAN_QUERY_HYBRID` | `aletheiadb.query.hybrid` | Graph + vector + temporal composition |
| `SPAN_VECTOR_INDEX` | `aletheiadb.vector.index` | Vector index configuration changes |
| `SPAN_VECTOR_SEARCH` | `aletheiadb.vector.search` | Vector similarity search |
| `SPAN_TEMPORAL_QUERY` | `aletheiadb.temporal.query` | Public temporal API queries |
| `SPAN_STORAGE_HISTORICAL_QUERY` | `aletheiadb.storage.historical.query` | Historical storage reconstruction |
| `SPAN_TRANSACTION_COMMIT` | `aletheiadb.transaction.commit` | Write transaction commit |

## Metrics Catalogue

| Metric constant | Metric name | Instrument | Labels |
|-----------------|-------------|------------|--------|
| `METRIC_ERRORS` | `aletheiadb.errors` | Counter | `category` |
| `METRIC_WRITE_CONFLICTS` | `aletheiadb.write_conflicts` | Counter | none |
| `METRIC_CRITICAL_EVENTS` | `aletheiadb.critical_events` | Counter | `event` |
| `METRIC_TRANSACTION_COMMITS` | `aletheiadb.transaction.commits` | Counter | `durability_mode`, `status` |
| `METRIC_TRANSACTION_COMMIT_DURATION` | `aletheiadb.transaction.commit.duration` | Histogram | `durability_mode`, `status` |
| `METRIC_TRANSACTION_OPERATIONS` | `aletheiadb.transaction.operations` | Histogram | `durability_mode`, `status` |

## Cardinality Rules

- Node, edge, transaction, and version IDs are span attributes only.
- Metric labels must be bounded enums or configuration names.
- Error category values are limited to `storage`, `temporal`, `query`,
  `transaction`, `vector`, `io`, and `other`.

## Consequences

Positive:

- Applications can route telemetry to any OpenTelemetry-compatible backend.
- The database crate no longer owns vendor clients or exporter lifecycles.
- The public release surface is smaller and easier to support.

Trade-offs:

- Users must install their own `tracing` subscriber and metrics exporter.
- The core crate provides contracts and call sites, not dashboards.
