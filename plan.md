1. **Add doctests to `Metaphor::align` in `src/experimental/metaphor.rs`**
   - Provide an executable example demonstrating how to align two subgraphs.
   - Use the `#[cfg(feature = "nova")]` pattern for doc tests as specified in memory.
2. **Add doctests to `Muse::inspire` in `src/experimental/muse.rs`**
   - Provide an executable example demonstrating how to find an inspiration vector.
   - Use the `#[cfg(feature = "nova")]` pattern for doc tests as specified in memory.
3. **Run Pre-PR Checks**
   - Run `cargo fmt --all`.
   - Run `cargo clippy --all-targets --all-features -- -D warnings`.
   - Run `cargo test --features nova`.
   - Complete pre-commit steps to ensure proper testing, verification, review, and reflection are done.
