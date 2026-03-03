1. **Smell (`src/query/executor/iterators.rs`)**: The `evaluate_predicate` function in `FilterIterator` is a "Pyramid of Doom" and "God Function" smell. It's almost 100 lines long with a massive `match` statement.
2. **Solution**: Extract logic out of `evaluate_predicate` into smaller, focused helper functions like `evaluate_eq`, `evaluate_gt`, `evaluate_contains`, etc., using early returns to flatten the nesting.
3. **Benefit**: Easier to read, test, and maintain. Enforces KISS.
4. **Verification**: Run `cargo clippy --all-targets --all-features -- -D warnings`, `cargo test`, and `cargo fmt --all`. Ensure no logic changes occur.
