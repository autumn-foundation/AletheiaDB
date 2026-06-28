# 🔭 Vantage: Spec for Management Dashboard

## 👤 User Story
**As a** Database Administrator or Developer,
**I want to** access a built-in web dashboard to view database status, run queries interactively, and explore nodes/edges,
**so that** I don't have to rely exclusively on the CLI or third-party tools to inspect my graph data and metrics.

## 🧐 The "So What?" Ask
**What business problem does this solve?**
Currently, interacting with AletheiaDB requires using the CLI, Python SDK, or writing raw HTTP queries. This creates a steep learning curve for new users and slows down operational debugging for existing users. A built-in management dashboard, served directly by the database process, provides instant visibility into the system's state without requiring additional setup.

**Metric Definition:**
- **Adoption:** 50% of active instances have the `/admin` routes accessed at least once per week.
- **Performance:** Dashboard pages load in under 100ms.

**Gap Analysis:**
- *Current State:* Users must use CLI tools or write scripts to query the graph and check status.
- *Standard Libraries / Market:* Neo4j has Neo4j Browser, ArangoDB has its web interface.
- *Future State:* A native, zero-config web interface using Maud + htmx served directly from the AletheiaDB HTTP server.

## ✅ Acceptance Criteria
- Must serve a web-based dashboard accessible via the `/admin` route when the `http-server` feature is enabled.
- Must include a "Status Overview" page displaying basic database metrics.
- Must implement server-side rendering using `maud` and dynamic interactions via `htmx`.
- Must function correctly without a separate Node.js/SPA build step.

## 🚫 Out of Scope
- Advanced visual graph exploration (node-link diagrams) - Phase 2.
- User authentication and role-based access control - Phase 3.
- Full Cypher query editor with autocomplete - Phase 2.
