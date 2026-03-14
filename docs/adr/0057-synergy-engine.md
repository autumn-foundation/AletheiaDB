# ADR-0057: Synergy Engine

**Status:** Accepted
**Date:** 2026-06-25
**Deciders:** AletheiaDB Core Team
**Categories:** Experimental, Cognitive Architecture, Data Synthesis

## Context

Within AletheiaDB's cognitive architecture, we require a mechanism to evaluate the "synergy" or emergent semantic value of a group of interconnected nodes. The goal is to determine how the relationships (graph structure) between a set of nodes alter their collective semantic meaning compared to their individual semantics.

Existing methods focus on either pure graph topology (e.g., centrality) or pure vector similarity (e.g., clustering), but do not adequately capture the interplay where the *structure* influences the *meaning* (semantics). A torn-read vulnerability in the analysis phase could also lead to inconsistent states if the vector properties and the graph structure are read from different MVCC snapshots.

## Decision

We introduced the **Synergy Engine** (`src/experimental/synergy.rs`) to calculate synergy scores using a hybrid structural-semantic approach. The process is defined as follows:

1.  **Baseline Vector Calculation:** We compute the simple arithmetic mean of the vectors of the specified nodes. This represents the "sum of the parts" without considering their structural relationships.
2.  **Emergent Vector Calculation:** For each node in the set, we look at its internal connections (edges to other nodes *within* the set). We calculate a structurally-influenced vector for the node by blending its original vector with the average vector of its internal neighbors (using an alpha factor of 0.5). The overall emergent vector is the average of these structurally-influenced vectors. This represents the meaning of the group *when considering their interactions*.
3.  **Synergy Score:** We calculate the cosine similarity between the normalized Baseline Vector and the normalized Emergent Vector. The synergy score is defined as `max(0.0, 1.0 - similarity)`. A higher score indicates that the structure significantly shifts the collective meaning away from the simple average.
4.  **Transaction Unity:** To prevent torn reads and ensure consistency between the semantic properties (vectors) and the graph structure (edges), the entire analysis process (steps 1 and 2) must be executed within a single read transaction `db.read(|tx| { ... })`.

## Consequences

### Positive
- **Hybrid Reasoning:** Provides a novel metric that combines graph topology and vector semantics, enabling AI agents to identify high-value, highly-interactive subgraphs.
- **Consistency Guarantee:** By enforcing a single read transaction for the entire analysis, we eliminate the risk of torn reads (where properties are read at time T1, but edges at time T2), ensuring deterministic and accurate synergy scores based on a consistent snapshot.
- **Modularity:** The engine is isolated in the `experimental` module, allowing for iteration without impacting core database performance.

### Negative
- **Computational Overhead:** The calculation requires multiple vector averages and neighbor lookups, which can be expensive for very large sets of highly connected nodes.
- **Parameter Sensitivity:** The alpha factor (currently fixed at 0.5) significantly impacts the emergent vector and the resulting synergy score.

### Neutral
- **Symmetric Edges:** The current implementation treats incoming and outgoing edges equally when finding neighbors within the set. Future iterations might weight edge directionality.