# 🔭 Vantage Spec: Universal HTTP API CRUD and Temporal Endpoints (The "Access" Dimension)

| Metadata | Details |
| :--- | :--- |
| **ID** | SPEC-010 |
| **Status** | 🔍 Review |
| **Owner** | Vantage (Product) |
| **Implementer** | Nova (Engineering) |
| **Priority** | P1 (High) |
| **Related Code** | `src/http/` |

## 1. 👤 User Stories

> **As a** Data Scientist,
> **I want to** update or delete nodes and edges via the HTTP API,
> **So that** I can manage my knowledge graph entirely from Python without writing backend Rust wrappers.

> **As a** Frontend Developer,
> **I want to** retrieve the specific history of a node or edge using a timestamp via the HTTP API,
> **So that** I can build a "time-traveling" UI that shows how a node's properties looked at a specific moment in time.

> **As a** System Administrator,
> **I want to** define and relate entities (creating edges between nodes) directly through simple REST-like JSON payloads,
> **So that** I can write simple bash/curl scripts to bootstrap the database without dealing with complex query languages.

## 2. 🧐 The "So What?" (Business Value)

In SPEC-005, we introduced the Universal HTTP API but left it partially incomplete. Users can create and read nodes, but they hit a wall when they need to modify existing data or manage edges. Furthermore, AletheiaDB's defining feature—its bi-temporal capability—is inaccessible via the HTTP endpoints.

**The Gap:**
- **Incomplete Lifecycle**: Users can't fix typos (Update) or remove outdated entities (Delete) via HTTP.
- **Disconnected Data**: Users can create nodes, but they can't link them together because edge creation/retrieval via HTTP is missing.
- **Hidden Superpowers**: Time-travel queries (getting a node *as of* a specific time) are AletheiaDB's unique selling point, yet they are locked behind the Rust API.

**ROI:**
- **Feature Completeness**: Makes the HTTP API a first-class citizen capable of full CRUD.
- **Product Differentiation**: Exposes our core bi-temporal features to the massive Python and JS ecosystems.
- **Reduced Friction**: Developers can prototype faster without needing to switch contexts or build custom middleware.

## 3. ✅ Acceptance Criteria

### Functional Requirements

1.  **Node Operations (Update/Delete)**:
    -   Must be able to update a node's properties by providing its ID and a JSON object of new properties.
    -   Must be able to delete a node by providing its ID.
    -   The update operation must return the updated node's state.
    -   Success metric: Operations complete with a standard 200 OK or 204 No Content.

2.  **Edge Operations (CRUD)**:
    -   Must be able to retrieve an edge by its ID.
    -   Must be able to create an edge between a `source_id` and a `target_id` with a specific label and optional properties.
    -   Must be able to update an edge's properties by its ID.
    -   Must be able to delete an edge by its ID.

3.  **Temporal Access**:
    -   Read operations (Get Node, Get Edge, Find Node, Find Neighbors) must accept optional `valid_time` and `tx_time` parameters (as timestamps).
    -   If provided, the database must return the entity's state *as of* that specific time, rather than its current state.
    -   If the entity did not exist at the requested time, the API should return a 404 Not Found.

4.  **Vector Search (Optional/Bonus)**:
    -   If vector indices are enabled, the API should allow finding similar nodes by providing a target vector and the property name to search against.

### Non-Functional Requirements
-   **Consistency**: All new operations must follow the existing polymorphic JSON request format (`operation` tag) established in SPEC-005.
-   **Security**: Pagination limits must apply to any list-returning endpoints to prevent DoS attacks.
-   **Performance**: Overhead for parsing and routing the new JSON payloads should remain negligible (< 5ms).

## 4. 🚫 Out of Scope (Phase 1)

-   **Batch Operations**: Updating or deleting multiple nodes/edges in a single HTTP request (Phase 2).
-   **GraphQL/gRPC**: This is strictly an extension of the existing JSON-over-HTTP API.
-   **Authentication/Authorization**: Handled at the infrastructure layer (e.g., API Gateway).
-   **Streaming Responses**: No Server-Sent Events (SSE) or WebSockets for historical playback yet.