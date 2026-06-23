# 🔭 Vantage: Spec for Admin Dashboard

## 👤 User Story
**As a** Database Operator,
**I want** a built-in web dashboard to view database health, metrics, and active transactions,
**so that** I can monitor AletheiaDB without configuring external observability tools.

## 🧐 The "So What?" Ask
**What business problem does this solve?**
AletheiaDB lacks an out-of-the-box way to easily visualize its internal state and health, which forces operators to either rely on raw logs or spend time integrating third-party monitoring before they can confidently run it in production. This dashboard lowers the barrier to adoption and provides instant observability.

**Success Metric Definition:**
- **Performance:** Success = `/admin` loads in <50ms.
- **Accuracy:** Success = Accurately reflects the current `NodeCount` and `EdgeCount` without degrading database performance.

## ✅ Acceptance Criteria
- Must serve an HTML dashboard on `/admin` routes.
- Must use server-side rendering (e.g., Maud) + HTMX for interactivity.
- Must display basic metrics: node count, edge count, memory usage, and current active transactions.
- Must have zero JavaScript build step dependencies (no React/NPM).

## 🚫 Out of Scope
- Write operations (e.g., executing queries, deleting nodes) from the dashboard (Phase 1 is Read-only).
- Metric history graphs (Prometheus integration is Phase 2).
