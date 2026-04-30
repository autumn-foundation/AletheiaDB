# 🔭 Vantage: Spec for Error Severity Levels

## 👤 User Story
**As an** SRE or DevOps Engineer operating AletheiaDB in production,
**I want to** categorize database errors by their severity (e.g., Expected, Warning, Critical),
**so that** I can configure my observability platforms (like Honeycomb or Prometheus) to page me only for genuine critical issues (like data corruption) and avoid alert fatigue from expected operational errors (like `NodeNotFound`).

## 🧐 The "So What?" Ask
**What business problem does this solve?**
Currently, AletheiaDB logs all errors with equal severity and categorizes them merely by type (Storage, Vector, Temporal, etc.). In high-throughput environments, encountering expected errors like `WriteConflict` (due to Snapshot Isolation) or `NodeNotFound` is routine. Because all errors look identical to monitoring systems, operations teams suffer from alert fatigue. If every error triggers a PagerDuty alert, engineers start ignoring them. By classifying errors by severity, we restore signal-to-noise ratio in our metrics. This reduces operational overhead, prevents burnout, and ensures that truly critical issues (like WAL checksum failures) are noticed immediately.

**Metric Definition:**
- **Alert Fatigue Reduction:** Reduce the volume of non-actionable alerts paged to on-call engineers by 80%.
- **Response Time:** Ensure critical errors (severity = Critical) are surfaced in observability dashboards within 5 seconds of occurrence.

**Gap Analysis:**
- *Current State:* Errors are categorized by type, not severity. SREs must manually filter logs to distinguish a routine missing node from a corrupted database.
- *Standard Libraries / Market:* Mature databases (like Postgres) have very clear severity classifications (e.g., FATAL vs ERROR vs WARNING).
- *Future State:* A defined severity classification (Expected, Warning, Critical) attached to observability metrics, making alerts deterministic.

## ✅ Acceptance Criteria
- Must introduce severity classification levels (Expected, Warning, Critical) to the observability layer.
- Must categorize all existing error types into these severity buckets (e.g., `NodeNotFound` -> Expected, `QueryTimeout` -> Warning, `CorruptedData` -> Critical).
- Must expose severity counters alongside existing category counters in the metrics API.
- Must ensure severity labels are included in tracing spans exported to Honeycomb.

## 🚫 Out of Scope
- Automatic remediation (e.g., automatically restarting the database on Critical errors).
- Complex adaptive sampling based on severity (Phase 5).
- Custom metrics registration by end-users.
