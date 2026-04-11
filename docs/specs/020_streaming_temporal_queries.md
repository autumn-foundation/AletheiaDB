# 🔭 Vantage Spec: Streaming Temporal Queries

| Metadata | Details |
| :--- | :--- |
| **ID** | SPEC-020 |
| **Status** | 📝 Draft |
| **Owner** | Vantage (Product) |
| **Implementer** | Core (Engineering) |
| **Priority** | P1 (High) |
| **Related Code** | `src/api/subscription/` (Proposed) |

## 1. 👤 User Stories

> **As a** Data Scientist or Security Analyst monitoring real-time semantic drift,
> **I want to** subscribe to specific graph patterns or node embedding changes as they happen,
> **So that** I can trigger immediate downstream alerts or LLM reasoning agents without constantly polling the database.

## 2. 🧐 The "So What?" (Business Value)

Currently, applications relying on real-time temporal knowledge graphs must repeatedly query (poll) the database to detect state changes, which introduces latency and scales poorly.

**The Gap:**
- **Polling Latency:** Polling delays time-to-action.
- **Resource Inefficiency:** Constant polling generates unnecessary database read load and wastes network bandwidth.

**ROI:**
- **Reactivity:** Streaming Temporal Queries enable reactive, push-based architectures.
- **Efficiency:** Drastically reduces database read load.
- **Speed:** Lowers time-to-action for automated reasoning agents, providing the foundation for real-time anomaly detection.

## 3. ✅ Acceptance Criteria

### Functional Requirements

1.  **Subscription API**:
    -   Must define an async Subscription API allowing clients to listen for changes to specific Nodes, Edges, or AQL Patterns.
2.  **Event Filtering**:
    -   Must provide temporal event filtering (e.g., only trigger if a property's value exceeds a certain threshold or if an embedding drifts beyond a similarity threshold).
3.  **Typed Events**:
    -   Must emit strongly-typed `ChangeEvents` detailing the before-and-after state and the transaction timestamp.
4.  **Resource Management**:
    -   Must handle client disconnects gracefully and clean up subscription resources automatically without memory leaks.

### Non-Functional Requirements
-   **Metric Definition:**
    -   **Notification Latency:** From the moment a transaction commits, subscribed clients receive the matching event in <10ms (p99).
    -   **Throughput:** A single node can handle 10,000 active subscriptions processing 1,000 events/second without degrading regular query performance by more than 5%.

## 4. 🚫 Out of Scope (Phase 1)

-   **External Message Queues**: Distributed message queue integrations (e.g., direct Kafka or RabbitMQ connectors). MVP will rely purely on in-memory async channels and WebSocket/SSE endpoints.
-   **Durable Subscriptions**: Durable subscription replay (if a client disconnects and reconnects, missed events are not buffered for replay).
