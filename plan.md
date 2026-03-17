1. **The Spark:** "We have vector magnitude, distance and velocity (Thermos, Tremor). But what about Acceleration? Can we detect semantic forces pushing a concept over time? Like detecting 'Momentum' of an idea."
2. **The Idea:** Build a new experimental module `src/experimental/momentum.rs` ("Momentum: Semantic Acceleration & Mass").
   - It will measure the *acceleration* of a node's vector path. If a node moves from `[0,0]` to `[1,0]` in 1s, then to `[3,0]` in the next 1s, its velocity increased, indicating a semantic force acting upon it.
3. **The Prototype:**
   - Define `SemanticMomentum` struct that calculates velocity vectors across multiple time windows.
   - Output `Acceleration` (rate of change of velocity) and `Momentum` (assuming mass = 1 for now, or proportional to node degree/property).
4. **Implementation:**
   - Create `src/experimental/momentum.rs`.
   - Use `TimeRange`, `NodeId`, get multiple historical states to compute 1st derivative (velocity) and 2nd derivative (acceleration).
   - Add doc-tests and unit tests demonstrating a node "speeding up" semantically vs moving at constant velocity.
   - Update `src/experimental/mod.rs` to expose `momentum`.
5. **The Pitch:** Create a PR titled `🌟 Nova: Semantic Momentum` explaining the spark, feature, potential, and risk.
6. **Pre-commit:** Complete pre-commit steps to ensure proper testing, verification, review, and reflection are done.
