🤦 **The Confusion:** Tried to run the `story_demo`. Compiler said `NarrativeGenerator` not found. Then, in the codebase I found tests ignoring AGENTS.md rules (`unsafe` blocks in `NarrativeGenerator`). Further, error messages tell me to use `db.enable_vector_index` which is no longer the correct API, it should be `db.vector_index("...").hnsw(...).enable()`.

🕵️ **The Reality:**
1. Tests were not properly configuring `NarrativeGenerator` and using `unsafe` code inappropriately.
2. The `IndexNotFound` error message in `src/query/planner/mod.rs` and `src/query/executor/iterators.rs` used the outdated `db.enable_vector_index` format.
3. Tests and codebase broadly were still using the deprecated `db.enable_vector_index` API.

💡 **The Fix:**
1. Removed `unsafe` from `NarrativeGenerator` tests, using `PhantomData`.
2. Updated all Error hints and remaining references of `db.enable_vector_index` to properly use `db.vector_index(prop).hnsw(config).enable()`.
3. Validated docs and code compile.
