# ADR-0036: Dreamer Semantic Extrapolation

**Status:** Accepted
**Date:** 2026-05-20
**Deciders:** GallifreyDB Core Team
**Categories:** ai, vectors, experimental

## Context

In a vector database, embeddings typically represent the semantic meaning of an entity *at a specific moment*. However, in many domains (user preferences, financial markets, concept evolution), the "meaning" or "position" of an entity drifts over time.

A standard vector search answers: "What is similar to X right now?"
Users often want to answer: "Where is X heading?" or "What will X be similar to in the future if current trends continue?"

For example, if a user starts reading "Intro to Python" (Beginner) and then "Advanced Rust" (Expert), a simple average might suggest "Intermediate", but the *trajectory* suggests they are becoming an "Expert".

## Decision

We will implement **Dreamer**, a Semantic Trajectory Extrapolation engine, within the `experimental` feature set.

Dreamer treats the high-dimensional vector space as a physical space where entities have "Velocity".

### Algorithm

1.  **Fetch History:** Retrieve historical snapshots of the entity's vector embedding over a specified `history_window`.
2.  **Calculate Velocity:**
    -   Identify the `first` and `last` vector within the window.
    -   Compute the time delta (`duration`).
    -   `Velocity = (Last_Vector - First_Vector) / duration_seconds`.
3.  **Project:**
    -   `Future_Vector = Last_Vector + (Velocity * future_horizon_seconds)`.
4.  **Search:**
    -   Perform a standard HNSW vector search using `Future_Vector` as the query.

## Consequences

### Positive

-   **Predictive Power:** Enables "forecasting" in semantic space, allowing applications to anticipate user needs or concept shifts.
-   **Dynamic Recommendations:** Can recommend content that matches where the user *will be*, not just where they are.

### Negative

-   **Linearity Assumption:** The current implementation assumes linear progression. Complex trajectories (curves, oscillations) are not modeled, which may lead to poor predictions for non-linear behaviors.
-   **Noise Sensitivity:** Small fluctuations in embeddings (common with some models) can be amplified into large velocities, resulting in wild predictions.
-   **Data Requirements:** Requires dense-enough history of vector updates to form a meaningful trajectory.

## Compliance

This ADR supports the "Nova" persona's directive to build "Additive R&D" features.
