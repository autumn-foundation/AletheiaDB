# Vantage: Spec for Metrics Contract Coverage

## User Story

**As a** production operator,
**I want** comprehensive bounded-cardinality metrics covering query performance,
vector search latencies, temporal index health, and resource utilization,
**so that** I can operate AletheiaDB safely at scale using my own metrics
exporter and dashboard stack.

## Background

AletheiaDB now exposes a backend-agnostic metrics contract. The core crate
should define metric names, label sets, and call sites; the host process owns
exporters through `metrics-rs`, OpenTelemetry, or another adapter.

## Acceptance Criteria

- Over 90% of database subsystem operations (WAL, HNSW index, graph traversal,
  current storage, historical storage) have contract-defined counters or
  histograms where instrumentation overhead is acceptable.
- Metric labels remain bounded. Node IDs, edge IDs, transaction IDs, and version
  IDs are forbidden as metric labels.
- The `metrics-rs` adapter forwards all contract samples without changing names
  or label sets.
- Exporter setup stays outside the core crate.

## Out of Scope

- Built-in scrape endpoints.
- Vendor-specific exporters.
- Alert manager configuration.
