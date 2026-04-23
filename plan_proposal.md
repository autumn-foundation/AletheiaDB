1. **Discover & Scaffold (The Spark)**
   - Create a new file `src/experimental/characterization/hologram.rs`.
   - Hologram leverages the existing `Starlight` JSON exporter to generate the ego-graph, but wraps it in a self-contained, interactive 3D HTML document powered by `3d-force-graph`.
   - Add a `HologramBuilder` struct to configure presentation (e.g., background color, title).

2. **Implement Code and Failing Tests (Red/Green)**
   - Implement `hologram.rs` with the `export_html` method.
   - Write a unit test within `mod tests` inside `hologram.rs` that builds a small graph, exports it via `Hologram`, and asserts that the resulting HTML contains the expected template strings and node data.
   - Use `run_in_bash_session` to write this file directly.

3. **Expose the Module (Integration)**
   - Update `src/experimental/characterization/mod.rs` to include `pub mod hologram;` and `pub use hologram::*;`.

4. **Run Workspace Checks**
   - Execute `cargo test --lib experimental` to ensure the new tests pass.
   - Execute `cargo clippy --all-targets --all-features -- -D warnings` and `cargo fmt --all` to enforce formatting and lints.

5. **Complete Pre-Commit Steps**
   - Complete pre-commit steps to ensure proper testing, verification, review, and reflection are done.

6. **Submit PR (The Pitch)**
   - Create a PR titled "🌟 Nova: Hologram - 3D HTML Graph Exporter".
   - Include required Nova PR sections: 💡 The Spark, 🚀 The Feature, 🔮 The Potential, and ⚠️ Risk.
