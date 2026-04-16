# 🔭 Vantage: Spec for CI Pipeline Stability

## 👤 User Story
**As an** Engineering Manager or Contributor,
**I want** the continuous integration (CI) pipeline to remain stable and pass successfully when no new code logic is introduced,
**so that** the team can merge documentation, specifications, and non-code PRs without being blocked by unrelated linting errors caused by toolchain upgrades.

## 🧐 The "So What?" Ask
**What business problem does this solve?**
Currently, the CI pipeline is failing on documentation-only PRs because the GitHub Actions runner automatically upgraded the stable Rust toolchain (to 1.95.0), introducing new strict Clippy lints (`manual-checked-ops`, `unnecessary-sort-by`). This causes "false positive" blockages for contributors who are not modifying code (such as Product Managers writing specs). A failing CI pipeline halts velocity, creates confusion, and forces non-engineers to attempt code fixes that violate their roles and constraints. Fixing this restores the smooth flow of value delivery across all roles.

**Success Metric Definition:**
- **CI Reliability:** The `Linting` check run in GitHub Actions passes 100% of the time for documentation-only Pull Requests.
- **Developer Experience:** Contributors do not have to fix unrelated code lints to merge Markdown changes.

## ✅ Acceptance Criteria
- Must resolve the `clippy::manual_checked_ops` warning in `src/index/vector/distributed.rs` (line 1086).
- Must resolve the `clippy::unnecessary_sort_by` warning in `src/storage/migration.rs` (line 914).
- Must ensure that `cargo clippy --all-targets --all-features -- -D warnings` runs cleanly on the latest stable Rust toolchain (1.95.0+) in the CI environment.
- Must not introduce any functional regressions or performance degradations when addressing the lints.

## 🚫 Out of Scope
- Pinning the Rust toolchain version in `.github/workflows/ci.yml` (Engineering should address the lints rather than deferring the toolchain upgrade).
- Expanding the scope to fix other technical debt or refactoring unrelated files.
