# 🔭 Vantage: Spec for Streaming Temporal Queries

## 👤 User Story
**As a** Data Scientist monitoring real-time semantic drift,
**I want** to subscribe to specific graph patterns or node embedding changes as they happen,
**so that** I can trigger immediate downstream alerts or LLM reasoning agents without constantly polling the database.

## 🧐 The "So What?" Ask
**What business problem does this solve?**
Currently, applications relying on real-time temporal knowledge graphs must repeatedly query (poll) the database to detect state changes, which introduces latency and scales poorly. Streaming Temporal Queries enable reactive, push-based architectures. This drastically reduces database read load, lowers time-to-action for automated reasoning agents, and provides the foundation for real-time anomaly detection.

**Success Metric Definition:**
- **Notification Latency:** From the moment a transaction commits, subscribed clients receive the matching event in <10ms (p99).
- **Throughput:** A single node can handle 10,000 active subscriptions processing 1,000 events/second without degrading regular query performance by more than 5%.

## ✅ Acceptance Criteria
- Must define an async Subscription API allowing clients to listen for changes to specific Nodes, Edges, or AQL Patterns.
- Must provide temporal event filtering (e.g., only trigger if a property's value exceeds a certain threshold or if an embedding drifts beyond a similarity threshold).
- Must emit strongly-typed `ChangeEvents` detailing the before-and-after state and the transaction timestamp.
- Must handle client disconnects gracefully and clean up subscription resources automatically without memory leaks.

## 🚫 Out of Scope
- Distributed message queue integrations (e.g., direct Kafka or RabbitMQ connectors) - MVP will rely purely on in-memory async channels and WebSocket/SSE endpoints.
- Durable subscription replay (if a client disconnects and reconnects, missed events are not buffered).
