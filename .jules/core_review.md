No high-severity findings.

### Test gaps
- No missing tests were identified for correctness/regression risks. The core logic of finding the valid history version correctly maintains bounds checks and type validations.

### Residual risks
- Fixed a known `clippy::collapsible_if` warning in `src/experimental/omen.rs` that was previously explicitly suppressed with `#[allow(clippy::collapsible_if)]`. The nested conditions inside `find_vector_at` were refactored using an early return (`continue` loop keyword) to properly flatten the conditional logic without using the unstable `let_chains` feature.
- No correctness, data loss, concurrency, security, or API behavior regressions were found in `src/experimental/omen.rs`'s logical paths.