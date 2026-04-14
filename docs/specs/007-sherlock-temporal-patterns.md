# Spec: Sherlock 2.0 - Temporal Pattern Matching Engine

## Status
- **State**: Draft
- **Owner**: Vantage
- **Date**: 2024-05-22

## Context
AletheiaDB stores the *history* of every node and edge. While we can query "What was the state at time T?", we currently lack a high-level way to query *patterns* of change over time. Users often need to find sequences of events that match a specific criteria, especially for compliance, fraud detection, and process optimization.

The existing `Sherlock` prototype (v1) is a basic linear scanner that supports simple property checks. It lacks expressiveness (no negation, no logical operators) and robustness.

## User Stories

### 1. Compliance Officer
"As a Compliance Officer, I want to automatically flag any transaction where 'Approval' occurred *before* 'Risk Check' was completed, so that I can ensure regulatory compliance."

### 2. Fraud Analyst
"As a Fraud Analyst, I want to find users who changed their 'Email Address' and then initiated a 'Large Transfer' within 5 minutes, so I can block potential account takeovers."

### 3. Process Engineer
"As a Process Engineer, I want to identify orders that went from 'Shipped' back to 'Processing', so I can debug our logistics pipeline."

## The "Mystery" DSL (Domain Specific Language)

We propose a declarative DSL for defining "Mysteries" (Temporal Patterns).

### Conceptual Model
A `Mystery` consists of:
1.  **Sequence**: An ordered list of `Clue`s.
2.  **Time Window**: The maximum duration for the entire sequence (or between steps).
3.  **Constraints**: Additional logic (e.g. Negation).

### Clue Types
-   **PropertyState**: `Node.property == Value` (at a specific time).
-   **PropertyChange**: `Node.property` changed from `Old` to `New`.
-   **EdgeExistence**: `(Node)-[:REL]->(Other)` exists.
-   **SemanticSimilarity**: `cosine_similarity(Node.vector, TargetVector) > Threshold`.

### Example (Pseudocode)

```rust
let mystery = Mystery::new()
    .within(Duration::from_minutes(5))
    .sequence(vec![
        Clue::PropertyChange("email", Any, Any), // Step 1: Email changed
        Clue::PropertyChange("status", "Normal", "HighValueTransfer"), // Step 2: High Value Transfer
    ]);
```

## Acceptance Criteria

1.  **Sequence Detection**: Must correctly identify sequences of events (A -> B -> C) in the correct order.
2.  **Time Windows**: Must respect a maximum time window for the entire sequence.
3.  **Semantic Clues**: Must support vector similarity as a condition (e.g. "User's behavior vector drifted > 0.5").
4.  **Negation (Future)**: "A happened, and B did NOT happen within X time." (Phase 2).
5.  **Cross-Node Patterns (Future)**: "Node A did X, then Node B (connected to A) did Y." (Phase 2).

## Out of Scope (Phase 1)
-   **Real-time Stream Processing**: We focus on *historical analysis* (batch) first.
-   **Complex Event Processing (CEP)**: Full CEP engine features (sliding windows, aggregation) are out of scope. We focus on *sequence matching*.
-   **Cross-Shard Patterns**: Single-shard/single-node analysis first.

## Technical Considerations
-   **Efficiency**: Avoid O(N^2) scans over history. Use the sorted nature of the log/versions.
-   **API**: The API should be builder-based for ease of use in Rust.
-   **Integration**: Should integrate with `NarrativeGenerator` to explain *why* a pattern matched.
