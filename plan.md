1. **Address Code Review - Deduplicate Candidates Deterministically**: Use `replace_with_git_merge_diff` to modify `src/experimental/mirror.rs` to deduplicate `candidates` deterministically.
2. **Address Code Review - Optimize Lazy Fetching**: Use `replace_with_git_merge_diff` to modify `src/experimental/mirror.rs` to optimize lazy fetching by moving the fetch of `vec_a` to the outer loop.
3. **Address Code Review - Error Handling for Connections**: Use `replace_with_git_merge_diff` to modify `src/experimental/mirror.rs` to propagate errors when checking for node connections.
4. **Run `cargo check`**: Verify the workspace compiles correctly via `cargo check`.
5. **Run `cargo test`**: Verify the tests pass via `cargo test --lib aletheiadb`.
6. **Run `cargo fmt`**: Format the code via `cargo fmt --all`.
7. **Pre-commit checks**: Complete pre-commit steps to ensure proper testing, verification, review, and reflection are done.
8. **Submit PR**: Submit the changes with the `submit` tool using the exact same branch name `nova-feature-mirror`.
