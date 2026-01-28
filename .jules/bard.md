## 2024-05-22 - Vector Index Confusion
**Confusion:** The documentation in `src/index/vector/mod.rs` stated that the HNSW implementation did not exist and used "Phase 2" future tense, while `src/index/vector/hnsw.rs` contained a full implementation.
**Clarification:** Updated `src/index/vector/mod.rs` to reflect the current state of the codebase, removing outdated "no implementation" warnings and pointing to the concrete `HnswIndex` implementation.
