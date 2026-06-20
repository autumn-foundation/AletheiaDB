# 🔭 Vantage Spec: Management Dashboard

| Metadata | Details |
| :--- | :--- |
| **ID** | SPEC-021 |
| **Status** | 🔍 Review |
| **Owner** | Vantage (Product) |
| **Implementer** | Web/API (Engineering) |
| **Priority** | P2 (Medium) |
| **Related Code** | `src/http/server.rs`, `src/http/handlers.rs` |

## 1. 👤 User Stories

> **As a** Database Administrator,
> **I want to** visually inspect the health and status of my AletheiaDB instance,
> **So that** I can ensure the system is running smoothly without relying solely on raw logs or CLI commands.

> **As a** Developer exploring AletheiaDB,
> **I want to** be able to execute queries and visualize the resulting nodes and edges through a web UI,
> **So that** I can easily debug my graph schemas and understand the data relationships without building my own tooling.

## 2. 🧐 The "So What?" (Business Value)

Currently, AletheiaDB provides an HTTP API, an MCP server, and a CLI, but lacks a built-in graphical interface for monitoring and querying the database.

**The Gap:**
- **Developer Experience (DX):** Users must rely on raw API calls (e.g., `curl`) or external tools to view data. This increases the friction for new users trying to adopt the database.
- **Observability:** Health checks and metrics are currently only accessible via JSON endpoints. There is no out-of-the-box visual representation of the system state.

**ROI:**
- **Adoption:** A built-in dashboard drastically lowers the barrier to entry, making it easier for new users to "see" the value of a bi-temporal graph database.
- **Ecosystem Cohesion:** Leveraging the newly migrated `autumn-web` framework (with `maud` and `htmx`) provides this capability internally without needing a separate standalone SPA project.

## 3. ✅ Acceptance Criteria

### Functional Requirements

1.  **Overview Page:**
    - The server MUST host an `/admin` endpoint serving a server-side rendered HTML dashboard using `maud` and `htmx`.
    - The overview page MUST display basic system status: Uptime, Version, active features, and memory usage.
2.  **Query Runner:**
    - The dashboard MUST include a text input area where a user can enter and execute Cypher/AQL queries.
    - The query results MUST be displayed in a tabular format (for Phase 1), showing node and edge data.
3.  **Data Browser:**
    - The dashboard MUST provide a simple way to browse Nodes and Edges by label.

### Non-Functional Requirements

-   **Performance:** Dashboard rendering should add negligible overhead to the server footprint.
-   **Metric Definition:** Success = Dashboard loads in < 100ms, and query execution reflects standard API latencies.

## 4. 🚫 Out of Scope (Phase 1)

-   **Visual Graph Rendering:** Rendering complex force-directed graph visualizations (e.g., D3.js or Sigma.js canvas) is deferred. Results will be tabular initially.
-   **Authentication/Authorization:** Phase 1 assumes the dashboard runs on internal trusted networks or is protected by a reverse proxy. Native RBAC for the dashboard is out of scope.
-   **Write Operations via UI:** The dashboard is read-only. Modifying nodes or edges through the UI is out of scope.

## 5. 📝 Gap Analysis (Current vs. Spec)

| Feature | Current State | Required State | Action |
| :--- | :--- | :--- | :--- |
| **Routing** | HTTP server only hosts JSON API | Must host `/admin` HTML routes | Add `maud` routes to `autumn-web` app |
| **Dependencies** | `maud` and `htmx` not actively used | `maud` feature enabled | Enable features in `Cargo.toml` |
| **System Visibility** | `/status` endpoint returns JSON | Visual dashboard overview | Build Maud template for status page |
