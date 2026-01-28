## 2024-05-22 - [Refactoring God Object src/db.rs]
**Learning:** `src/db.rs` was accumulating too many responsibilities, specifically index persistence logic mixed with database initialization. The `GallifreyDB` struct initialization block was over 300 lines long.
**Action:** Extracted persistence orchestration into `restore_indexes_from_persistence` and helper functions `load_single_vector_index` / `persist_single_vector_index`. Future refactoring should consider moving these to a dedicated `persistence` module if they grow further.
