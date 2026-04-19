# 🔭 Vantage: Spec for Sherlock 2.0 (Temporal Pattern Matching)

## 👤 User Story
**As a** Compliance Officer or Fraud Analyst,
**I want** to automatically flag specific temporal sequences of events (e.g., an 'Approval' occurring *before* a 'Risk Check', or an 'Email Change' followed quickly by a 'Large Transfer'),
**so that** I can detect regulatory violations or coordinated account takeovers that happen over a period of time.

## 🧐 The "So What?" Ask
**What business problem does this solve?**
While AletheiaDB stores the full history of every node and edge, users currently lack a declarative, expressive way to query *sequences* of change. The existing prototype (Sherlock v1) is a linear scanner without logical operators or robust sequence detection. Sherlock 2.0 provides an expressive Domain Specific Language (DSL) to easily define and match complex temporal patterns (Mysteries), significantly reducing the time required to build compliance and fraud detection rules while improving detection accuracy.

**Success Metric Definition:**
- **Match Recall:** Sherlock 2.0 correctly identifies 100% of defined sequential patterns (A -> B -> C) within the specified time windows in a historical dataset.
- **Query Latency:** Matching a 3-step sequence over a 1-month history for a specific node completes in <10ms.

## ✅ Acceptance Criteria
- Must define a declarative builder-based API (DSL) for defining `Mystery` sequences consisting of ordered `Clue`s (e.g., `PropertyState`, `PropertyChange`, `EdgeExistence`, `SemanticSimilarity`).
- Must correctly detect sequences of events occurring in the specified order.
- Must enforce a maximum Time Window (e.g., the entire sequence must occur within 5 minutes).
- Must support vector similarity as a temporal condition (e.g., detecting if a user's behavior vector drifted > 0.5 over time).
- Must be optimized to avoid O(N^2) scans over history, leveraging the sorted nature of historical versions.

## 🚫 Out of Scope
- Real-time Stream Processing or full Complex Event Processing (CEP) engine features (e.g., sliding windows, aggregations). MVP focuses on historical batch analysis.
- Cross-Node Patterns (e.g., "Node A did X, then Node B did Y").
- Negation logic (e.g., "A happened, and B did NOT happen").
