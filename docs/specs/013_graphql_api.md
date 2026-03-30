# 🔭 Vantage Spec: GraphQL API Layer (The "Access" Dimension)

| Metadata | Details |
| :--- | :--- |
| **ID** | SPEC-013 |
| **Status** | 🔍 Review |
| **Owner** | Vantage (Product) |
| **Implementer** | Nova (Engineering) |
| **Priority** | P2 (Medium) |
| **Related Code** | `src/http/graphql/` (Proposed) |

## 1. 👤 User Stories

> **As a** Frontend Developer building a complex dashboard,
> **I want to** query AletheiaDB using GraphQL, specifying exactly the fields and nested relationships I need in a single request,
> **So that** I avoid over-fetching data, reduce the number of HTTP roundtrips, and iterate faster using standard GraphQL client libraries (like Apollo or Relay).

> **As a** Mobile Developer,
> **I want to** request a tailored subset of a node's properties and its temporal history,
> **So that** I can conserve bandwidth and battery life on mobile networks while still accessing rich temporal graphs.

> **As a** Data Explorer,
> **I want to** use GraphiQL or similar introspective tools against the database,
> **So that** I can discover the available schema, labels, and query capabilities interactively without reading static documentation.

## 2. 🧐 The "So What?" (Business Value)

We introduced the Universal HTTP API in SPEC-005, giving AletheiaDB standard REST/JSON endpoints. However, REST has well-known limitations when querying graph data.

**The Gap:**
- **Over/Under-fetching**: A REST `GET /nodes/:id` returns *all* properties. If a node has a massive vector embedding or a large text block, the client downloads it even if they only needed the "name" property. Alternatively, to get a node and its neighbors, the client must make multiple sequential requests (N+1 problem).
- **Client-Side Complexity**: Reconstructing graph relationships from flat REST responses requires custom logic on the client.
- **Discoverability**: The REST API lacks built-in schema introspection.

**ROI:**
- **Developer Experience (DX)**: GraphQL is the industry standard for querying graph data from the frontend. Supporting it natively makes AletheiaDB instantly familiar to millions of UI developers.
- **Performance**: Drastically reduces network payload size and roundtrips, especially crucial for mobile or edge deployments.
- **Ecosystem**: Unlocks compatibility with a massive ecosystem of existing GraphQL tools (code generators, cache managers, IDE plugins).

## 3. ✅ Acceptance Criteria

### Functional Requirements

1.  **Read Operations (Queries)**:
    -   Must expose a single `/graphql` endpoint accepting standard GraphQL `POST` requests.
    -   Must support querying a node by ID and requesting specific fields (properties).
    -   Must support traversing edges and fetching connected nodes within the same GraphQL query (e.g., `node(id: 1) { name, KNOWS { name } }`).
    -   Success Metric: A complex query fetching a node, 2 levels of neighbors, and filtering specific fields returns the exact requested shape.

2.  **Temporal Support**:
    -   The GraphQL schema must expose arguments for `valid_time` and `tx_time` on node and edge queries.
    -   Example: `node(id: 1, valid_time: "2023-01-01T00:00:00Z") { name }`.

3.  **Vector Search Integration**:
    -   Must expose a `similar_nodes` query that accepts a target vector and returns a ranked list of nodes.

4.  **Schema Introspection**:
    -   The server must support standard GraphQL introspection queries so that tools like GraphiQL work out of the box.

### Non-Functional Requirements
-   **Performance**: The overhead of parsing the GraphQL AST and converting it to AletheiaDB's internal IR should be < 10ms.
-   **Security**: Must implement query depth limiting to prevent Denial of Service (DoS) attacks via deeply nested queries.

## 4. 🚫 Out of Scope (Phase 1)

-   **Mutations**: Creating, updating, or deleting nodes/edges via GraphQL. Phase 1 is strictly read-only.
-   **Subscriptions**: Real-time updates via WebSockets are deferred to a future phase.
-   **Strict Typing based on Labels**: AletheiaDB is schema-less by default. The Phase 1 GraphQL schema will treat properties as a generic JSON scalar or key-value array, rather than generating specific GraphQL types for each node label (e.g., no strict `User` type, just a `Node` type with `properties`). Strong typing inference is Phase 2.