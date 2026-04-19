# 🗣️ Echo: Getting Started example is broken

## Description

🤦 **The Confusion:** Tried to run the examples from the README.md and ran into several confusing issues:
1. The `story_demo` code throws a deprecated warning but then crashes if the `nova` feature is not enabled. The `NarrativeGenerator` fails with a hard panic rather than gracefully handling feature omissions. Worse yet, in the testing code, it bypasses safety with `unsafe { std::mem::transmute(()) }`.
2. The `IndexNotFound` error message uses the old API `db.enable_vector_index(..., config)` which confused me immensely when trying to set up vector indexing as instructed by the hint, since the modern fluent API is `db.vector_index("...").hnsw(...).enable()`.

🕵️ **The Reality:**
1. In `src/experimental/temporal_narrative.rs`, the `stub_tests` module uses `unsafe { std::mem::transmute(()) }` to construct a generator just to test the panic, which violates `AGENTS.md` (must use `PhantomData` instead).
2. `src/query/planner/mod.rs` and `src/query/executor/iterators.rs` were still hardcoding hints with `"Call db.enable_vector_index(\"{}\", config) first"`.

💡 **The Fix:**
1. Replaced `unsafe { std::mem::transmute(()) }` with `NarrativeGenerator { _marker: std::marker::PhantomData }` in the `stub_tests` module of `src/experimental/temporal_narrative.rs`.
2. Changed the `IndexNotFound` hint from `"Call db.enable_vector_index(\"{}\", config) first"` to `"Call db.vector_index(\"{}\").hnsw(...).enable() first"` in `src/query/planner/mod.rs`, `src/query/executor/iterators.rs`, and the tests in `src/core/error.rs`.
