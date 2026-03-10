1. **The Spark:** "We have time-traveling graphs and vector similarity. Can we detect if history is repeating itself?"
   - I want to create a `Dejavu` engine in `src/experimental/dejavu.rs` that detects cycles or repeating semantic patterns over time.
   - E.g., comparing a node's vector trajectory from `T0 to T1` with its trajectory from `T2 to T3`. Are we moving in a similar pattern?

2. **The Prototype:**
   - Create `src/experimental/dejavu.rs`
   - Expose `DejavuEngine` with a `detect_repeating_history(node, window_size, min_similarity)` method.
   - It will fetch a node's history, slice it into windows, and compare the vector movements between windows using cosine similarity.
   - Register the module in `src/experimental/mod.rs` with `#[cfg(feature = "nova")] pub mod dejavu;`.
   - Add a test that proves it detects repeating history.

3. **The Unslop:**
   - `cargo test --features nova`
   - `cargo clippy --all-targets --all-features -- -D warnings`
   - `cargo fmt --all`

4. **The Pre-commit Step:**
   - Complete pre-commit steps to ensure proper testing, verification, review, and reflection are done.

5. **The Submit:**
   - Commit and submit PR: "🌟 Nova: Dejavu Engine (Repeating History Detection)"
