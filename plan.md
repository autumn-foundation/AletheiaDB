1. **The Spark:** I noticed that we have temporal data (`get_node_history`) and vector similarity search, but we don't have a way to measure the semantic "distance traveled" by a node over its entire lifespan.
2. **The Feature:** Create a new `Odometer` module (`src/experimental/odometer.rs`) that calculates the cumulative semantic distance a node's vector property has moved across all its historical versions.
3. **The Potential:** This enables "Semantic Volatility" tracking, allowing users to find concepts that have evolved the most (or least) over time, which is highly relevant for analyzing changing LLM embeddings or shifting user preferences.
4. **Execution:**
   - Create `src/experimental/odometer.rs` with the `Odometer` struct and `calculate_distance` method.
   - Add tests using `cargo test --features nova -p aletheiadb --lib experimental::odometer`.
   - Update `src/experimental/mod.rs` to expose the new module.
