# Core🦀 Review Findings

## Findings
1. **Severity:** High
   **File reference:** `src/experimental/omen.rs:102` (before patch)
   **What can break:** `Omen::predict_encounter` produces mathematically invalid encounter predictions (e.g. time_to_encounter and predicted_distance) when comparing trajectories of two nodes that have mismatched vector dimensions (e.g. predicting an encounter between a 2D node and a 3D node).
   **Why it breaks:** The distance physics relies on iterating over the arrays. `Iterator::zip` is used to calculate relative position and velocity (`pos_b.iter().zip(pos_a.iter())`). If `pos_a` and `pos_b` have different lengths, `zip` silently truncates the output to the shortest length. This means the 3rd dimension of a 3D node is completely ignored when compared against a 2D node, resulting in invalid math and no explicit failure.
   **Minimal fix:** Add a dimension check before the math block: `if pos_b.len() != pos_a.len() { return Ok(None); }`.
   **Required tests:** Added `test_omen_dimension_mismatch` which creates two distinct nodes (`node_a` and `node_b`) initialized with a 2D and 3D vector, updates them to establish velocity, and verifies that `predict_encounter` correctly short-circuits and returns `Ok(None)`.

## Test Gaps
* The previous test coverage for `Omen` did not include any validation for mismatched vector dimensions across different semantic entities.
* The error path for `AletheiaDB::write` required explicit type bounds for `Ok(())` (`Ok::<(), crate::core::error::Error>(())`) when used in test closures, indicating potential DX friction for users writing simple write loops.
### Test gaps
- No missing tests were identified for correctness/regression risks. The core logic of finding the valid history version correctly maintains bounds checks and type validations.

### Residual risks
- A known `clippy::collapsible_if` warning exists in `src/experimental/omen.rs` that is explicitly suppressed with `#[allow(clippy::collapsible_if)]`. This prevents CI failures under strict linting (`-D warnings`) but leaves nested `if` statements in the codebase. However, it poses no correctness, regression, or concurrency risk, and correctly resolves the issue without relying on unstable Rust features like let chains.
