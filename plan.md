1. **Analyze existing documentation in `src/index/adjacency.rs`.**
   - The file is relatively well documented but `/// Get total number of edges in the index.` on `pub fn edge_count` and similar lines like `/// Get the maximum node ID in this index.` or `/// Get the number of nodes with outgoing edges.` repeat the function name and lack examples. We will enhance these.
   - We will write examples (`/// ```rust`) for public functions like `export_csr`, `import_csr`, `new`, `build`, `get_adjacency`, `get_adjacency_with_label`, `degree`, `has_edges`, `edge_count`, `max_node_id`, `iter_nodes`, and `node_count`.
2. **Implement the documentation enhancements.**
   - Add a `## Examples` block to all public functions in `src/index/adjacency.rs` demonstrating usage.
   - Refine descriptions to explain *why* and under what conditions to use them.
3. **Verify the enhancements.**
   - Run `cargo test` to ensure all doc tests compile.
   - Run `cargo clippy --all-targets --all-features -- -D warnings` to check for issues.
   - Run `cargo doc --open` or equivalent checks to verify rendering.
4. **Complete pre-commit steps to ensure proper testing, verification, review, and reflection are done.**
5. **Create a PR with title `🎻 Bard: [documentation update]`**.
