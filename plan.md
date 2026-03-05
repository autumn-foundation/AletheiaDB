1. *The Spark:* Create `entanglement.rs` in `src/experimental/` to detect Quantum Entanglement in the graph! It identifies pairs of nodes whose semantic vectors change synchronously over time, even without direct edges (or to find hidden correlations).
2. *The Scaffold:* Implement `EntanglementDetector`.
   - `struct EntanglementDetector`
   - It will take a list of node IDs and analyze their histories over a specific `TimeRange` or simply across versions.
   - For each node, compute a sequence of vector *deltas* (changes between consecutive versions).
   - Compute the correlation (cosine similarity between the delta vectors) between pairs of nodes.
   - High correlation = High Entanglement.
3. *Unslop:* Add tests that create two nodes, mutate them together synchronously with the same delta, and one node with different mutations. The test will assert that the entangled pair has a higher entanglement score.
   - Refactor the code to ensure it's DRY and minimal. Use exact match on node IDs, and proper error handling.
4. *Add module:* Add `#[cfg(feature = "nova")] \n pub mod entanglement;` to `src/experimental/mod.rs`
5. *Check:* Run `cargo clippy`, `cargo test`, `cargo fmt --all`. (Pre-commit steps).
6. *Present:* PR Title "🌟 Nova: Entanglement Detector". Include Spark, Feature, Potential, Risk in description.
