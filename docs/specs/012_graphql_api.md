# 🔭 Vantage Spec: GraphQL API Layer

| Metadata | Details |
| :--- | :--- |
| **ID** | SPEC-012 |
| **Status** | 🔍 Review |
| **Owner** | Vantage (Product) |
| **Implementer** | Core (Engineering) |
| **Priority** | P2 (Medium) |
| **Related Code** | `src/http/graphql/` (To be created) |

## 1. 👤 User Stories

> **As a** Frontend Developer,
> **I want to** query AletheiaDB using GraphQL,
> **So that** I can request exactly the data I need for my UI components without over-fetching or under-fetching, utilizing the powerful ecosystem of Apollo/Relay clients.

> **As an** API Integrator,
> **I want to** automatically explore the AletheiaDB schema through GraphQL introspection,
> **So that** my development tools (like GraphiQL or Postman) can autocomplete queries and provide documentation natively, saving me from having to read external docs for every property.

> **As a** Data Analyst,
> **I want to** embed bi-temporal parameters (`as_of`) directly into my GraphQL queries,
> **So that** I can retrieve the state of a graph at a specific historical moment using a standard, strongly-typed query language instead of constructing custom REST payloads or raw AQL.

## 2. 🧐 The "So What?" (Business Value)

AletheiaDB currently has a Universal HTTP API (REST-like, JSON-based via `/query`) and a custom query language (AQL). However, modern web and mobile ecosystems heavily rely on GraphQL for data fetching due to its efficiency and strong typing.

**The Gap:**
- **Over-fetching/Under-fetching:** REST-like endpoints often return either too much data (wasting bandwidth) or too little (requiring multiple round-trips). For a graph database, retrieving deeply nested relationships in a single optimized request is crucial.
- **Developer Experience (DX):** Frontend developers expect introspection and tooling (GraphiQL, Apollo Client). Without GraphQL, they must write brittle boilerplate to parse raw AQL responses or JSON payloads.
- **Ecosystem Integration:** Many modern frameworks (Next.js, React Native) have built-in optimizations for GraphQL that our users cannot currently leverage.

**ROI:**
- **Adoption:** Drastically lowers the barrier to entry for frontend and full-stack developers.
- **Efficiency:** Reduces network overhead by allowing clients to specify their exact data requirements.
- **Product Stickiness:** By integrating seamlessly into existing frontend tooling, AletheiaDB becomes the default choice for modern applications needing temporal graph capabilities.

## 3. ✅ Acceptance Criteria

### Functional Requirements

1.  **GraphQL Endpoint:**
    - Must expose a single `/graphql` endpoint that accepts standard GraphQL POST requests.
    - Must provide a `/graphiql` or `/playground` endpoint for in-browser query exploration (enabled in development mode).

2.  **Schema Auto-Generation (Introspection):**
    - The server MUST dynamically generate a GraphQL schema based on the existing node labels, edge types, and properties in the database.
    - Since AletheiaDB is schemaless, the GraphQL layer must infer the schema by sampling or maintaining an internal type registry.

3.  **Graph Traversal Queries:**
    - Must allow querying nodes by ID or property.
    - Must allow traversing edges as nested GraphQL fields.
    - Success = Can execute a query like `{ Person(name: "Alice") { KNOWS { name } } }` and receive the expected graph structure.

4.  **Temporal Support:**
    - The GraphQL schema must include arguments for `valid_time` and `tx_time` on root queries to support point-in-time reads.

5.  **Mutations (CRUD):**
    - Must support `createNode`, `updateNode`, `deleteNode`, and equivalent edge mutations with proper input types.

### Non-Functional Requirements

-   **Performance:** The translation from GraphQL AST to AletheiaDB's internal `QueryBuilder` or `AQL` must add < 2ms of overhead.
-   **Security:** Must include query depth limiting and complexity analysis to prevent DoS attacks via deeply nested traversals (e.g., maximum depth = 5).
-   **Metric Definition:** Success = A 3-hop traversal GraphQL query completes in < 20ms for 99% of requests.

## 4. 🚫 Out of Scope (Phase 1)

-   **Subscriptions (Real-time):** GraphQL Subscriptions over WebSockets for live updates are deferred to Phase 2.
-   **Federation:** Apollo Federation or schema stitching with external services is out of scope.
-   **Vector Search Integration:** Integrating `SIMILAR TO` vector queries into the GraphQL schema is complex and deferred to Phase 2. Phase 1 focuses purely on graph traversal and temporal CRUD.

## 5. 📝 Gap Analysis (Current vs. Spec)

| Feature | Current State | Required State | Action |
| :--- | :--- | :--- | :--- |
| **API Protocol** | Custom JSON over HTTP | Standard GraphQL protocol | Add `async-graphql` or `juniper` dependency and HTTP handler |
| **Schema** | None (Schemaless) | Strongly typed GraphQL Schema | Implement dynamic schema generation/inference based on db labels |
| **Tooling** | None | GraphiQL Playground | Mount a playground handler on `/graphiql` |
| **Security** | Rate limiting (API level) | Query depth limiting | Configure GraphQL executor with depth/complexity limits |
