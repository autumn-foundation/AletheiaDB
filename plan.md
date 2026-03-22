1. **Implement `Mirror`: Semantic Reflection Engine**: Use `write_file` to create `src/experimental/mirror.rs` containing the `Mirror` and `Reflection` structs, the similarity calculation logic (using `cosine_similarity`), and a `#[cfg(test)]` block.
2. **Verify `mirror.rs`**: Use `read_file` to verify the creation and content of `src/experimental/mirror.rs`.
3. **Register `Mirror` module**: Use `sed` to add `#[cfg(feature = "nova")]\npub mod mirror;` to `src/experimental/mod.rs`
4. **Run `cargo check`**: Verify the workspace compiles correctly.
5. **Run `cargo clippy`**: Check for lints using `cargo clippy --all-targets --all-features -- -D warnings`.
6. **Run `cargo test`**: Verify the tests pass via `cargo test --lib aletheiadb`.
7. **Run `cargo fmt`**: Format the code via `cargo fmt --all`.
8. **Pre-commit checks**: Complete pre-commit steps to ensure proper testing, verification, review, and reflection are done.
9. **Submit PR**: Submit the PR with the title "🌟 Nova: [Feature Name]" containing the required description format.
