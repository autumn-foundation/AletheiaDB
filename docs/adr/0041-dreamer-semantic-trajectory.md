# 41. Dreamer: Semantic Trajectory Extrapolation

Date: 2024-05-22

## Status

Proposed

## Context

In vector databases, embeddings typically represent the semantic meaning of an entity at a specific moment. However, in many domains (e.g., user preferences, market trends, concept evolution), the "meaning" or "position" of an entity drifts over time.

A standard vector search answers: "What is similar to X right now?"
Predictive applications need to answer: "What will X be similar to in the future?"

For example:
-   If a user moved from reading "Basic Python" to "Advanced Rust", where are they heading next?
-   If a market trend is shifting from "Cloud" to "Edge", what is the next logical step?

Simply querying the current state ignores the *trajectory* of the entity's movement in the high-dimensional latent space.

## Decision

We will implement **Dreamer**, a Semantic Trajectory Extrapolation engine, within the `Nova` experimental suite.

### Core Concepts

1.  **Semantic Velocity**: The rate and direction of change of a vector embedding over time.
2.  **Trajectory Projection**: Estimating a future vector position based on past velocity.
3.  **Future Neighbor Search**: Performing ANN (Approximate Nearest Neighbor) search using the projected vector as the query.

### Algorithm

Dreamer operates on the historical vector snapshots of a node:

1.  **History Extraction**: Retrieve historical versions of the target vector property within a specified `history_window`.
2.  **Velocity Calculation**:
    -   Identify the `start` vector at the beginning of the window and the `end` vector at the end.
    -   Calculate the time difference (`delta_t`).
    -   Compute `velocity = (end - start) / delta_t`.
3.  **Extrapolation**:
    -   Define a `future_horizon` (e.g., "1 day into the future").
    -   Compute `predicted_vector = end + (velocity * future_horizon)`.
4.  **Search**: Use the `predicted_vector` to query the current vector index for nearest neighbors.

## Consequences

### Positive
-   **Predictive Intelligence**: Enables "next step" recommendations and trend forecasting directly within the database.
-   **Simplicity**: The linear projection model is computationally cheap (O(D) for projection) and easy to reason about.
-   **Integration**: Leverages existing historical storage and HNSW indexes without requiring new storage structures.

### Negative
-   **Linear Assumption**: Assumes semantic drift is linear. This may fail for cyclical patterns or abrupt shifts (e.g., a user completely changing interests).
-   **Noise Sensitivity**: Small fluctuations in high-dimensional vectors can lead to erratic velocity vectors ("Semantic Jitter"), potentially requiring smoothing (e.g., moving averages) in future iterations.
-   **Data Requirements**: Requires entities to have a sufficient history of updates to calculate a meaningful velocity. Static entities cannot be projected.
