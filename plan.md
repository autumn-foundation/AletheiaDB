1. **The Spark:** "Can we identify concepts that act as "black holes" in the semantic graph—nodes that draw many paths inward but rarely lead out semantically?"
2. **The Feature:** Create `src/experimental/black_hole.rs` to implement `BlackHoleDetector`. It evaluates node "gravity" by analyzing the ratio of incoming vs outgoing semantic paths and their similarity gradients.
3. **The Potential:** Useful for identifying sink concepts in knowledge bases, such as overly generic terms ("Thing", "Entity") or terminal thoughts in reasoning chains.
4. **Risk:** Low. Isolated in `src/experimental/black_hole.rs`.
5. **Implementation Steps:**
    - Create `src/experimental/black_hole.rs` with `BlackHoleDetector` struct.
    - Implement `detect(node_id)` which calculates an `EventHorizonScore` based on incoming/outgoing edge count and the vector similarity gradient (do incoming edges increase similarity to the node while outgoing edges decrease it?).
    - Add it to `src/experimental/mod.rs` gated by `#[cfg(feature = "nova")]`.
    - Write unit tests in `src/experimental/black_hole.rs`.
6. Complete pre commit steps to ensure proper testing, verification, review, and reflection are done.
