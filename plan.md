1. **Understand the problem:**
   - I need to hunt for an issue and show it crashing, as the "Havoc" persona.
   - The user provided the memory instruction: "In AletheiaDB, when importing raw integer arrays (e.g., `Vec<u64>`) that will be zero-copy transmuted into strongly typed wrappers (like `Vec<NodeId>` or `Vec<EdgeId>`), explicitly validate that no elements exceed `MAX_VALID_ID` before the `unsafe` block to maintain system invariants and prevent logic bugs or memory corruption downstream."
   - The method `AdjacencyIndex::import_csr` takes `node_ids: Vec<u64>` and `edge_ids: Vec<u64>`, transmuting them directly without ensuring elements are `<= MAX_VALID_ID`.
   - I will modify `src/index/adjacency.rs` to validate that `node_ids` and `edge_ids` are less than or equal to `MAX_VALID_ID` inside `validate_csr_invariants`.
   - Before fixing, I'll ensure I have a test (which I just wrote and failed).
   - I will fix the vulnerability in `validate_csr_invariants`.

2. **Fix `validate_csr_invariants`**:
   - I will update `AdjacencyIndex::validate_csr_invariants` to iterate through `node_ids` and `edge_ids` and ensure they are all `<= crate::core::id::MAX_VALID_ID`.
   - If any ID exceeds `MAX_VALID_ID`, return an `Err`.

3. **Verify Fix**:
   - Re-run `cargo test -p aletheiadb --lib index::havoc_tests` to verify the panic is now caught, returning an error (which `unwrap()`s inside `import_csr`, meaning it correctly panics on invalid input).

4. **Complete Pre-Commit Steps**:
   - Ensure `cargo test` passes.
   - Run `cargo fmt --all`.
   - Run `cargo clippy --all-targets --all-features -- -D warnings`.

5. **Submit PR**:
   - Create a pull request titled "👺 Havoc: Prevent invalid IDs during CSR import".
