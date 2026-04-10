1.  **Fix `story_demo` example feature check**: Change the `README.md` to properly handle the feature by not hardcoding a `main` function that doesn't compile if `nova` isn't enabled. Wait, we already did that! Let's check `DX_AUDIT_REPORT_ECHO_FINAL.md`.
    -   *Ah, the issue is about `NarrativeGenerator` panicking instead of gracefully failing when the `nova` feature is not enabled. Also, the `stub_tests` module uses `unsafe { std::mem::transmute(()) }` which violates `AGENTS.md`.* Let's fix that.
    -   *Also, the `IndexNotFound` error message uses the old API `db.enable_vector_index(..., config)` which is confusing. Let's fix that in `src/query/planner/mod.rs`, `src/query/executor/iterators.rs`, and the tests in `src/core/error.rs`.*
2.  **Fix `NarrativeGenerator`**:
    -   Open `src/experimental/temporal_narrative.rs`.
    -   Replace `unsafe { std::mem::transmute(()) }` with `NarrativeGenerator { _marker: std::marker::PhantomData }` in the `stub_tests` module.
3.  **Fix `IndexNotFound` hint**:
    -   Open `src/query/planner/mod.rs` and `src/query/executor/iterators.rs`. Search for `Call db.enable_vector_index(\"{}\", config) first` and replace it with `Call db.vector_index(\"{}\").hnsw(...).enable() first`.
    -   Update the tests in `src/core/error.rs` that check for this hint.
4.  **Complete pre-commit steps to ensure proper testing, verification, review, and reflection are done.**
5.  **Submit PR** using `worktree-new.sh` and `worktree-pr.sh`.
