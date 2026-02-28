No high-severity findings.

### Test gaps
- No missing tests were identified for correctness/regression risks.

### Residual risks
- A known `clippy::collapsible_if` warning exists in `src/experimental/omen.rs` that may cause CI failures when strict linting (`-D warnings`) is enforced, but it poses no correctness or regression risk.
