# 🔭 Vantage Spec: GraphQL API Layer (The "Federation" Dimension)

| Metadata | Details |
| :--- | :--- |
| **ID** | SPEC-012 |
| **Status** | 🔍 Review |
| **Owner** | Vantage (Product) |
| **Implementer** | Nova (Engineering) |
| **Priority** | P2 (Medium) |
| **Related Code** | `src/http/graphql/` (To be created) |

## 1. 👤 User Stories

> **As a** Frontend Developer,
> **I want to** query AletheiaDB using GraphQL,
> **So that** I can request exactly the data I need for my UI components without over-fetching or making multiple REST calls.

> **As an** API Integrator,
> **I want to** introspect the database schema via GraphQL,
> **So that** my tooling (like Apollo Studio or Postman) can automatically generate documentation and provide autocomplete for my queries.

> **As a** Full-Stack Engineer,
> **I want to** traverse relationships deeply in a single query,
> **So that** I don't have to write complex Cypher/AQL queries in the frontend or manage complex state joining.

## 2. 🧐 The "So What?" (Business Value)

AletheiaDB currently exposes a Universal HTTP API (REST-like JSON endpoints) and an AQL/Cypher endpoint. While AQL is powerful, it's not the standard language of modern frontend development. REST endpoints often lead to the "N+1 query problem" when fetching nested relationships.

**The Gap:**
- **Developer Experience (DX):** Frontend teams are accustomed to GraphQL's typed, introspectable nature. AQL strings embedded in JS are error-prone and lack compile-time validation.
- **Over-fetching:** Getting a node and its neighbors currently requires either a custom AQL query or multiple REST calls, often returning properties the client doesn't need.
- **Tooling Ecosystem:** We are missing out on the massive ecosystem of GraphQL clients (Apollo, Relay, urql) that handle caching and state management out of the box.

**ROI:**
- **Frontend Adoption:** Drastically lowers the barrier to entry for React/Vue/Mobile developers.
- **Performance (Client-side):** Reduces payload sizes by allowing clients to specify exactly which properties to return.
- **Schema Discovery:** Provides built-in documentation through introspection, reducing the need to read external docs to understand the graph structure.

## 3. ✅ Acceptance Criteria

### Functional Requirements

1.  **GraphQL Endpoint:**
    -   Must expose a `/graphql` endpoint that accepts standard POST requests.
    -   Must support the GraphQL IDE (e.g., GraphiQL or Apollo Sandbox) for easy exploring at `GET /graphql` (when accessed via browser).

2.  **Schema Definition:**
    -   Must dynamically generate a GraphQL schema based on the nodes and edges present in the database (or support a user-defined schema configuration).
    -   Must support querying Nodes by ID or Label.

3.  **Graph Traversal:**
    -   Must be able to query a node and its connected edges/neighboring nodes in a single GraphQL query.
    -   Example: `query { node(id: 1) { properties, knows: edges(type: "KNOWS") { target { properties } } } }`

4.  **Temporal Queries:**
    -   Must support passing `validTime` and `txTime` arguments to queries to retrieve historical data.
    -   Example: `query { node(id: 1, validTime: "2023-01-01T00:00:00Z") { properties } }`

5.  **Mutations (Phase 1):**
    -   Must support basic `createNode`, `updateNode`, and `deleteNode` mutations.
    -   Must support `createEdge` and `deleteEdge` mutations.

### Non-Functional Requirements
-   **N+1 Mitigation:** The underlying resolver must efficiently batch database lookups (e.g., using DataLoader patterns or translating the GraphQL AST into a single optimized AQL query).
-   **Security:** Must implement query depth limiting to prevent malicious deep-nested queries from causing DoS.

## 4. 🚫 Out of Scope (Phase 1)

-   **Subscriptions:** Real-time updates via WebSockets (GraphQL Subscriptions) are deferred to Phase 2.
-   **Vector Search Integration:** Exposing HNSW vector similarity search directly through GraphQL is complex and deferred to Phase 2. The REST/AQL APIs remain the primary interface for vector search.
-   **Federation:** Supporting Apollo Federation (acting as a subgraph in a larger graph) is out of scope for the initial release.

## 5. 📝 Gap Analysis (Current vs. Spec)

| Feature | Current State | Required State | Action |
| :--- | :--- | :--- | :--- |
| **Endpoint** | REST `/api/v1/*` | GraphQL `/graphql` | Add Juniper/Async-graphql dependency |
| **Schema** | None (Schema-less) | Dynamic/Configured | Build schema generator |
| **Resolvers** | Handlers | GraphQL Resolvers | Map resolvers to `AletheiaDB` API |
| **Security** | None | Depth limits | Implement query validation |
