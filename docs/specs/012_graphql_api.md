# 🔭 Vantage Spec: GraphQL API Layer (The "Queryability" Dimension)

| Metadata | Details |
| :--- | :--- |
| **ID** | SPEC-012 |
| **Status** | 📝 Draft |
| **Owner** | Vantage (Product) |
| **Implementer** | Nova (Engineering) |
| **Priority** | P2 (Medium) |
| **Related Code** | `src/graphql/` (To be created) |

## 1. 👤 User Stories

> **As a** Frontend Engineer building an LLM reasoning dashboard,
> **I want to** query AletheiaDB's graph using GraphQL to fetch exactly the nodes and edges I need in a single request,
> **So that** I don't have to over-fetch data or manage complex AQL string construction in my React components.

> **As a** Platform Data Scientist,
> **I want to** explore the database schema and query capabilities via an interactive GraphQL Playground (like GraphiQL),
> **So that** I can intuitively discover available entities, vector search capabilities, and temporal features without needing to read extensive API documentation.

> **As an** AI Agent Developer,
> **I want to** pass a strictly typed GraphQL schema to my LLM so it can construct accurate database queries,
> **So that** the model hallucinates less syntax errors compared to writing raw AQL strings.

## 2. 🧐 The "So What?" (Business Value)

AletheiaDB currently relies heavily on AQL (its Cypher-like query language) and a basic REST-like API (SPEC-005/010). While AQL is powerful, it forces client applications to compose raw string queries.

**The Gap:**
- **Developer Experience (DX):** Frontend ecosystems (React, Apollo, Relay) treat GraphQL as a first-class citizen. Forcing them to use a custom AQL-over-REST endpoint increases onboarding friction.
- **Over-fetching:** REST endpoints often return the entire node/edge property map. UI components usually only need 2 or 3 fields.
- **LLM Tooling:** LLMs are exceptional at generating GraphQL queries when provided a schema, often performing better than when generating proprietary query languages.

**ROI:**
- **Frontend Adoption:** Dramatically lowers the barrier to entry for full-stack developers building UIs on top of AletheiaDB.
- **Network Efficiency:** Reduces payload sizes by letting clients specify exactly which properties they need.
- **Schema Discoverability:** Built-in introspection provides self-documenting APIs out of the box.

## 3. ✅ Acceptance Criteria

### Functional Requirements

1.  **Schema Generation**:
    -   Must dynamically generate a GraphQL schema based on the current labels and properties in the database (or support a user-defined static schema bridging to the dynamic graph).
    -   Must expose types for `Node` and `Edge`, with their respective properties.

2.  **Graph Traversal Queries**:
    -   Must support querying nodes by ID or property match.
    -   Must support traversing edges (e.g., fetching a `Person` and their nested `KNOWS` relationships to other `Person` nodes).

3.  **Temporal & Vector Integration**:
    -   Query fields must accept optional arguments for bi-temporal access (e.g., `validTime: "2024-01-01T00:00:00Z"`).
    -   Must provide a mechanism to execute vector similarity searches via GraphQL arguments (e.g., `similarTo: [0.1, 0.2, ...], k: 10`).

4.  **Mutations (Phase 1)**:
    -   Must support basic `createNode`, `updateNode`, and `deleteNode` mutations.
    -   Must support basic `createEdge` and `deleteEdge` mutations.

5.  **Interactive Playground**:
    -   When running the HTTP server, navigating to `/graphql` in a browser should open an interactive IDE (like GraphiQL or Apollo Studio Explorer).

### Non-Functional Requirements
-   **Performance**: The translation from GraphQL AST to AletheiaDB's internal query planner/AQL must add < 10ms overhead.
-   **N+1 Problem Mitigation**: Edge traversals must be batched or properly translated to underlying graph joins to avoid classic GraphQL N+1 performance cliffs.

## 4. 🚫 Out of Scope (Phase 1)

-   **Subscriptions**: Real-time updates via GraphQL Subscriptions (WebSockets) are deferred to a later phase.
-   **Federation**: Apollo Federation support is not required.
-   **Fine-Grained Auth**: Field-level authorization is assumed to be handled at the gateway or app layer, not within the DB's GraphQL engine itself.

## 5. 📝 Gap Analysis (Current vs. Spec)

| Feature | Current State | Required State | Action |
| :--- | :--- | :--- | :--- |
| **Protocol** | HTTP JSON/AQL only | GraphQL over HTTP | Add `async-graphql` or `juniper` dependency. |
| **Schema** | Schema-less (Dynamic) | Typed GraphQL Schema | Implement dynamic schema builder based on graph metadata. |
| **Execution** | AQL AST Execution | GraphQL AST Execution | Build a resolver layer that maps GraphQL fields to DB read/write transactions. |
| **Tooling** | None | GraphiQL UI | Embed a GraphQL IDE route in the web server. |
