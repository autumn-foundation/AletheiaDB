# ADR-0036: Semantic Temperature (Thermos)

**Status:** Accepted
**Date:** 2026-05-25
**Deciders:** AletheiaDB Core Team
**Categories:** experimental, vector-search, temporal

## Context

Standard vector databases treat embeddings as static points or a series of points. While we can find "nearest neighbors" at any point in time, we lack metrics to understand the **dynamics** of these points.

Questions like:
*   "Is this user's interest profile stable, or are they exploring new topics rapidly?"
*   "Is this company's market positioning shifting?"
*   "Which concepts in our knowledge graph are most volatile?"

require measuring the *rate of change* of vector embeddings, not just their position. A static similarity search cannot capture "Velocity" or "Volatility".

## Decision

We will implement **Semantic Temperature** (codenamed "Thermos") in `src/experimental/thermos.rs`.

Thermos introduces physical concepts to the semantic space:
1.  **Volatility**: The total Euclidean distance traveled by a node's vector embedding over a specific time window.
    $$ Volatility = \sum_{t=1}^{n} distance(v_t, v_{t+1}) $$
2.  **Temperature**: The average velocity of semantic change.
    $$ Temperature = \frac{Volatility}{Duration} $$
3.  **Thermal Reading**: A struct capturing these metrics plus the update count.

The `Thermos` engine allows users to `measure_node(id, time_range)` and receive a `ThermalReading`.

## Consequences

### Positive

*   **Derivative Metrics**: We can now index and query on the *first derivative* of meaning (Velocity), not just position.
*   **Anomaly Detection**: High temperature can indicate instability, pivoting, or account compromise (rapid behavioral shift).
*   **Engagement Metrics**: In recommendation systems, "High Temperature" users might be in "Discovery Mode", while "Low Temperature" users are in "Exploitation Mode".

### Negative

*   **Computationally Expensive**: calculating volatility requires retrieving the full version history of a node and computing pairwise distances between all consecutive versions ($O(V)$ where $V$ is version count). It is not an O(1) lookup.
*   **Noise Sensitivity**: If the embedding model is unstable or if there is "jitter" in the vector generation process, it will register as high volatility even if the semantic meaning hasn't changed.
*   **Storage Pressure**: To be useful, this requires storing many historical versions of vectors, which can be large.

## Implementation Details

*   **Location**: `src/experimental/thermos.rs`
*   **Metric**: Euclidean Distance is used for "travel" (Path Length), as Cosine Distance is not a true metric (triangle inequality issues) and doesn't represent "movement" as intuitively in this context.
*   **Input**: `TimeRange` to define the measurement window.
*   **Output**: `ThermalReading { volatility: f32, temperature: f32, updates: usize }`.
