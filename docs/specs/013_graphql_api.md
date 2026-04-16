# 🔭 Vantage: Spec for GraphQL API Layer

## 👤 User Story
**As a** Frontend Developer building a complex data dashboard,
**I want** to execute nested graph and temporal queries over an HTTP API using standard GraphQL syntax,
**so that** I can rapidly build user interfaces without needing custom parsing for AQL, while fetching exactly the fields I need to minimize network payloads.

## 🧐 The "So What?" Ask
**What business problem does this solve?**
Currently, interacting with AletheiaDB requires either the native Rust client, MCP server, or sending raw AQL strings over a custom protocol. This creates a steep learning curve and friction for web and mobile developers, preventing rapid adoption in modern web stacks. By providing a standard GraphQL API, we unlock the massive ecosystem of existing GraphQL clients (Apollo, Relay, urql), tooling, and developers. It significantly reduces the "Time to First Successful Query" for frontend teams, directly driving adoption and user growth.

**Success Metric Definition:**
- **Developer Experience (DX):** Frontend engineers can execute multi-hop graph queries from a web client using standard Apollo Client within 5 minutes.
- **Query Latency (Overhead):** The API layer adds <5ms of overhead parsing the GraphQL AST into AQL/internal queries.
- **Payload Efficiency:** Network payload size is reduced by up to 40% compared to typical REST equivalents due to exact field selection.

## ✅ Acceptance Criteria
- Must expose an HTTP endpoint (e.g., `/graphql`) serving standard GraphQL POST requests.
- Must provide an auto-generated GraphQL Schema encompassing the current Graph schema (Node Labels and Edge Types).
- Must support nested multi-hop graph queries mapped efficiently to the underlying traverse engine, avoiding N+1 query problems (using DataLoader patterns internally).
- Must extend GraphQL with temporal query arguments (e.g., `validTime`, `txTime`) mapped to AletheiaDB's temporal APIs.
- Must support querying node properties, metadata, and vectors seamlessly.

## 🚫 Out of Scope
- GraphQL Subscriptions (Real-time updates via WebSockets) - MVP focuses on Query and Mutation operations.
- Federation (e.g., Apollo Federation) - MVP acts as a standalone graph.
- Native gRPC API layer (Focus solely on GraphQL over HTTP for this spec).
