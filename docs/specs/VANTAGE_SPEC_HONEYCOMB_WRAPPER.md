# 🔭 Vantage: Spec for Custom Honeycomb Client Wrapper

## 👤 **User Story:**
**As a** DevOps Engineer or SRE managing AletheiaDB in a production environment,
**I want** a custom, deeply integrated Honeycomb client wrapper that avoids experimental or unmaintained git dependencies,
**so that** I can reliably export distributed traces and span analysis for my graph database without introducing supply-chain risks, dependency conflicts, or deployment instability caused by relying on third-party forks.

## 🧐 The "So What?" Ask
**What business problem does this solve?**
Currently, AletheiaDB's observability feature relies on a git dependency for Honeycomb distributed tracing (as tracked in Issue #271 and noted in README.md). Using raw git dependencies in production-grade databases violates enterprise supply-chain security policies, creates friction for downstream package managers (like `cargo install` or building OS packages), and exposes the database to upstream API breakages. By building a custom Honeycomb client wrapper, we remove the unstable git dependency, unblocking enterprise adoption and ensuring our telemetry data pipeline is stable, auditable, and fully under our control.

**Metric Definition:**
- **Stability:** 0 git dependencies in the `Cargo.toml` dependency tree for the `observability-honeycomb` feature.
- **Latency/Throughput:** Exporting trace data must happen asynchronously and must add `<1ms` of latency to the main transaction thread.

**Gap Analysis:**
The market demands enterprise software to have clean, stable dependency trees. Relying on an unofficial or unmaintained git branch for a critical observability feature signals that the database is not production-ready. Replacing this with an in-house or stable crate-based wrapper closes this gap.

## ✅ **Acceptance Criteria:**
- Must replace the existing git dependency for Honeycomb tracing with a custom wrapper or a stable crates.io release.
- Must support exporting standard trace spans (e.g., query execution, transaction commits, WAL syncs) to the Honeycomb API.
- Must handle network failures gracefully (e.g., buffering, retries, and dropping traces on timeout) without crashing or blocking the database thread.
- Must remain configurable via standard environment variables (`HONEYCOMB_API_KEY`, `HONEYCOMB_DATASET`).

## 🚫 **Out of Scope:**
- Building a full OpenTelemetry (OTLP) exporter from scratch (we only need the Honeycomb-specific event submission for now).
- Expanding the current instrumentation to cover new subsystems; this spec only covers replacing the transport/client layer.
