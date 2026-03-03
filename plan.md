1. **Explore mutants to confirm fix**. I will run `cargo mutants --file src/query/planner/rules/predicate_pushdown.rs -v --timeout 60` to ensure no mutants survive in `predicate_pushdown.rs` tests.
2. **Update journal**. I will write a new entry to `.jules/elenchus.md` reporting that weak assertions in `PredicatePushdown` tests were refactored using destructuring over `matches!` to avoid false confidence.
3. **Complete pre-commit steps to ensure proper testing, verification, review, and reflection are done.** I will run the required checks for standard workflow.
4. **Submit change**. I will commit the changes as `⚔️ Elenchus: Strengthen Predicate Pushdown Tests`.
1. **Smell (`src/query/executor/iterators.rs`)**: The `evaluate_predicate` function in `FilterIterator` is a "Pyramid of Doom" and "God Function" smell. It's almost 100 lines long with a massive `match` statement.
2. **Solution**: Extract logic out of `evaluate_predicate` into smaller, focused helper functions like `evaluate_eq`, `evaluate_gt`, `evaluate_contains`, etc., using early returns to flatten the nesting.
3. **Benefit**: Easier to read, test, and maintain. Enforces KISS.
4. **Verification**: Run `cargo clippy --all-targets --all-features -- -D warnings`, `cargo test`, and `cargo fmt --all`. Ensure no logic changes occur.
