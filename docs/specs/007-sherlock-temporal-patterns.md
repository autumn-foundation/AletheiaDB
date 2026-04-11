# 🔭 Vantage Spec: Sherlock 2.0 (Temporal Pattern Matching Engine)

| Metadata | Details |
| :--- | :--- |
| **ID** | SPEC-007 |
| **Status** | 📝 Draft |
| **Owner** | Vantage (Product) |
| **Implementer** | Nova (Engineering) |
| **Priority** | P2 (Medium) |
| **Related Code** | `src/experimental/sherlock/` |

## 1. 👤 User Stories

> **As a** Compliance Officer or Fraud Analyst,
> **I want to** automatically flag specific temporal sequences of events (e.g., an 'Approval' occurring *before* a 'Risk Check', or an 'Email Change' followed quickly by a 'Large Transfer'),
> **So that** I can detect regulatory violations or coordinated account takeovers that happen over a period of time, ensuring compliance and preventing fraud.

> **As a** Process Engineer,
> **I want to** identify orders that went from 'Shipped' back to 'Processing',
> **So that** I can debug our logistics pipeline and identify bottlenecks or errors.

## 2. 🧐 The "So What?" (Business Value)

While AletheiaDB stores the *history* of every node and edge (allowing "What was the state at time T?"), users currently lack a declarative, expressive way to query *sequences* of change. Users often need to find sequences of events that match a specific criteria, especially for compliance, fraud detection, and process optimization.

The existing prototype (Sherlock v1) is a basic linear scanner without logical operators or robust sequence detection. It lacks expressiveness (no negation, no logical operators) and robustness.

**The Gap:**
- **Expressiveness:** No easy way to query "A happened, then B happened."
- **Efficiency:** Writing manual queries for sequences involves complex, slow joins over historical data.

**ROI:**
- **Productivity:** Sherlock 2.0 provides an expressive Domain Specific Language (DSL) to easily define and match complex temporal patterns (Mysteries), significantly reducing the time required to build compliance and fraud detection rules.
- **Accuracy:** Improves detection accuracy by formalizing the structure of temporal patterns.

## 3. ✅ Acceptance Criteria

### Functional Requirements

1.  **Declarative API (DSL)**:
    -   Must define a builder-based API in Rust for defining `Mystery` sequences consisting of ordered `Clue`s.
    -   Clue types must include: `PropertyState` (property == value at a time), `PropertyChange` (property changed from old to new), `EdgeExistence` (relationship exists), and `SemanticSimilarity` (cosine similarity > threshold).
2.  **Sequence Detection**:
    -   Must correctly identify sequences of events (e.g., A -> B -> C) occurring in the specified order.
3.  **Time Windows**:
    -   Must enforce a maximum Time Window (e.g., the entire sequence must occur within 5 minutes, or between steps).
4.  **Integration**:
    -   Should integrate with `NarrativeGenerator` to explain *why* a pattern matched.

### Non-Functional Requirements
-   **Performance**: Must be optimized to avoid O(N^2) scans over history, leveraging the sorted nature of historical versions.
-   **Metric Definition**: Success = Match Recall: Sherlock 2.0 correctly identifies 100% of defined sequential patterns within specified time windows in a historical dataset. Query Latency: Matching a 3-step sequence over a 1-month history for a specific node completes in <10ms.

## 4. 🚫 Out of Scope (Phase 1)

-   **Real-time Stream Processing**: Full Complex Event Processing (CEP) engine features (e.g., sliding windows, continuous aggregations). MVP focuses on *historical batch analysis*.
-   **Cross-Node Patterns**: (e.g., "Node A did X, then Node B did Y"). Single-shard/single-node analysis first.
-   **Negation Logic**: (e.g., "A happened, and B did NOT happen within X time").
-   **Cross-Shard Patterns**: Detecting patterns that span across different distributed shards.

## 5. 📝 Gap Analysis (Current vs. Spec)

| Feature | Current State | Required State | Action |
| :--- | :--- | :--- | :--- |
| **API** | Simple linear scan function | Declarative builder (DSL) | Implement `Mystery` and `Clue` structures |
| **Logic** | Basic property matching | Sequence, Order, Time bounds | Update matching engine logic |
| **Performance** | O(N) scan per node | Optimized historical lookup | Use binary search on version history |
