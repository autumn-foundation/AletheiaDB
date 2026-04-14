# 🔭 Vantage Spec: Universal HTTP API (The "Access" Dimension)

| Metadata | Details |
| :--- | :--- |
| **ID** | SPEC-005 |
| **Status** | 🔍 Review |
| **Owner** | Vantage (Product) |
| **Implementer** | Nova (Engineering) |
| **Priority** | P1 (High) |
| **Related Code** | `src/http/` |

## 1. 👤 User Stories

> **As a** Frontend Developer,
> **I want to** query AletheiaDB using standard JSON/HTTP,
> **So that** I can build web applications (React/Vue/Svelte) without needing to link against Rust binaries or write WASM wrappers.

> **As a** Data Scientist,
> **I want to** access the knowledge graph directly from Python (Pandas/Jupyter),
> **So that** I can perform ad-hoc analysis, visualize data, and train models using my preferred toolchain.

> **As a** Microservices Architect,
> **I want to** treat AletheiaDB as a standalone service,
> **So that** I can scale it independently and integrate it into a polyglot environment (Go, Node.js, Python services).

## 2. 🧐 The "So What?" (Business Value)

AletheiaDB is currently a "Library Database" (embedded). This is great for performance but terrible for ecosystem adoption.

**The Gap:**
- **Accessibility**: Currently, you *must* write Rust to use AletheiaDB. This excludes 90% of the developer market (JS/TS, Python).
- **Usability**: Debugging data requires writing a Rust test script (`cargo run --example`). Users expect a curl-able endpoint.
- **Integration**: Cannot easily integrate with existing dashboards (Grafana, Retool) or low-code tools.

**ROI:**
- **Adoption**: Unlocks the two largest developer communities (Web & Data Science).
- **Decoupling**: Allows the database engine to evolve independently of the application logic.
- **Observability**: Makes it trivial to write external health checks and monitors.

## 3. ✅ Acceptance Criteria

### Functional Requirements

1.  **Node Operations (CRUD)**:
    -   `GET /api/v1/nodes/:id`: Retrieve current state of a node.
    -   `POST /api/v1/nodes`: Create a new node with label and properties.
    -   `PUT /api/v1/nodes/:id`: Update node properties.
    -   `DELETE /api/v1/nodes/:id`: Delete a node (transactional).

2.  **Edge Operations**:
    -   `GET /api/v1/edges/:id`: Retrieve an edge.
    -   `POST /api/v1/edges`: Create an edge between two nodes.
    -   `GET /api/v1/nodes/:id/edges`: Get incident edges (outgoing/incoming).

3.  **Query & Search**:
    -   `POST /api/v1/query`: Execute a Cypher/AQL query string. Body: `{"query": "MATCH (n) RETURN n", "params": {}}`.
    -   `POST /api/v1/vector/search`: Find similar nodes. Body: `{"vector": [...], "k": 10, "filter": "..."}`.

4.  **Temporal Access**:
    -   All `GET` endpoints must support temporal query parameters:
        -   `?valid_time=2023-01-01T00:00:00Z`
        -   `?tx_time=2023-01-05T12:00:00Z`
    -   If provided, return the state of the entity *as of* that time.

5.  **Standardization**:
    -   **Format**: All responses must be valid JSON.
    -   **Errors**: Standard HTTP status codes (200 OK, 201 Created, 400 Bad Request, 404 Not Found, 500 Internal Error).
    -   **Pagination**: List endpoints must support `limit` and `offset` (or cursor-based pagination).

### Non-Functional Requirements
-   **Performance**: Overhead of HTTP/JSON serialization should be < 5ms per request for small payloads.
-   **Concurrency**: Must handle concurrent requests using the underlying `actix-web` async runtime.

## 4. 🚫 Out of Scope (Phase 1)

-   **Authentication/Authorization**: Assume running in a trusted VPC or behind a gateway (Nginx/Kong).
-   **GraphQL**: Phase 2. We start with REST for simplicity.
-   **Streaming/WebSockets**: No real-time subscriptions yet.
-   **Binary Protocol**: JSON only for now (for DX). Protobuf/Arrow later.

## 5. 📝 Gap Analysis (Current vs. Spec)

| Feature | Current State | Required State | Action |
| :--- | :--- | :--- | :--- |
| **Server** | `src/bin/server.rs` exists | Robust Daemon | Harden server binary |
| **Endpoints** | `/status` only | CRUD + Query | Implement Handlers |
| **Temporal** | Rust API only | via Query Params | Map params to `db.get_at()` |
| **Vector** | Rust API only | `/vector/search` | Expose HNSW search |

## 6. 📅 Execution Plan

1.  **API Schema**: Define the exact JSON request/response structures (OpenAPI draft).
2.  **Core Handlers**: Implement `NodeHandler` and `EdgeHandler` in `src/http/handlers.rs`.
3.  **Query Handler**: Wire up the Query Parser to the HTTP endpoint.
4.  **Integration**: Ensure `AletheiaDB` instance is shared safely across async web workers (`Arc<AletheiaDB>`).
5.  **Verify**: Write an integration test using `reqwest` to hit the API and verify DB changes.
