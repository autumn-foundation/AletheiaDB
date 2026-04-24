# 🔭 Vantage Spec: Cypher Mutations (CREATE, MERGE, SET, DELETE)

| Metadata | Details |
| :--- | :--- |
| **ID** | SPEC-020 |
| **Status** | 🔍 Review |
| **Owner** | Vantage (Product) |
| **Implementer** | Core (Engineering) |
| **Priority** | P1 (High) |
| **Related Code** | `src/cypher/` and `src/api/` |

## 1. 👤 User Stories

> **As an** Application Developer,
> **I want to** use standard Cypher syntax to insert and update data in my graph,
> **So that** I do not have to write custom imperative Rust or JSON payloads to manage my graph state.

> **As a** Data Engineer running ETL pipelines,
> **I want to** use `MERGE` clauses to conditionally insert or update nodes and edges,
> **So that** my ingestion jobs are idempotent and I avoid creating duplicate records during retries.

> **As a** Database Administrator,
> **I want to** execute bulk `DELETE` and `DETACH DELETE` operations via Cypher,
> **So that** I can easily prune outdated or invalid subgraphs using expressive pattern matching.

## 2. 🧐 The "So What?" (Business Value)

AletheiaDB has invested heavily in the ability to *read* data using Cypher, including complex pattern matching and temporal queries. However, a database is only useful if data can be written to it. Currently, users are forced to construct manual JSON payloads or use imperative APIs to mutate state.

**The Gap:**
- **Developer Friction:** The cognitive overhead of switching between Cypher for reads and a custom API for writes slows down development.
- **Idempotency Challenges:** Implementing "create if not exists, otherwise update" (upsert) logic natively using the HTTP API requires complex application-side logic, leading to race conditions or duplication.
- **Ecosystem Compatibility:** Lack of mutation support breaks compatibility with existing graph management tools and ORMs that expect standard Cypher support.

**ROI:**
- **Adoption:** Completing the CRUD lifecycle for Cypher makes AletheiaDB a viable drop-in replacement for users migrating from Neo4j or Memgraph.
- **Safety and Integrity:** Exposing temporal constraints within `CREATE` and `MERGE` statements ensures that data provenance is captured accurately without requiring manual timestamp management by the client.

## 3. ✅ Acceptance Criteria

### Functional Requirements

1.  **Node and Edge Creation (`CREATE`)**:
    -   Must support creating nodes with specific labels and properties (e.g., `CREATE (p:Person {name: 'Alice'})`).
    -   Must support creating relationships between existing nodes found via `MATCH`.

2.  **Idempotent Operations (`MERGE`)**:
    -   Must support `MERGE` for both nodes and edges to handle upserts.
    -   Must support `ON CREATE SET` to apply properties only when a new entity is created.
    -   Must support `ON MATCH SET` to apply properties only when an existing entity is matched.

3.  **Property Modification (`SET` and `REMOVE`)**:
    -   Must support modifying or adding properties on existing nodes and edges (`SET p.age = 31`).
    -   Must support bulk property assignment (`SET p += {city: 'NYC'}`).
    -   Must support removing properties or labels (`REMOVE p.tempFlag`, `REMOVE p:Temporary`).

4.  **Data Deletion (`DELETE`)**:
    -   Must support deleting nodes and relationships.
    -   Must enforce referential integrity by failing if a user attempts to `DELETE` a node that still has connected relationships, unless `DETACH DELETE` is explicitly used.

5.  **Temporal Semantics**:
    -   All mutations MUST transparently create new versions in historical storage.
    -   Valid time MUST be set to the current time by default, with an option to specify it explicitly via parameters.
    -   Transaction time MUST be set automatically by the database engine.

### Non-Functional Requirements

-   **Atomicity:** An entire Cypher statement containing multiple `CREATE` or `SET` clauses must execute within a single transaction. It either succeeds entirely or fails entirely.

## 4. 🚫 Out of Scope (Phase 1)

-   **Complex Graph Refactoring:** Advanced commands like `CALL apoc.refactor.cloneNodes` or heavy schema migrations are deferred.
-   **Bulk Loading:** Specialized `LOAD CSV` commands are out of scope for the standard Cypher mutation parser. (Use the batch insert API for high-throughput ingestion).
-   **Triggers / Webhooks:** Triggering external events automatically after a `MERGE` or `DELETE` statement.