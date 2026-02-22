**[HNSW Vector Index Refactoring]
**Tangle:** `src/index/vector/hnsw.rs` was a 161KB "Blob" containing configuration, builder logic, core index implementation, persistence logic, and extensive tests. This violated the Single Responsibility Principle and made navigation and maintenance difficult.
**Blueprint:** Refactored into `src/index/vector/hnsw/` directory.
1. `config.rs`: Extracted `HnswConfig` and `HnswIndexBuilder`.
2. `persistence.rs`: Extracted persistence logic (`save`, `load`, `MAPPING_MAGIC`, `IndexMetadata`).
3. `mod.rs`: Kept core `HnswIndex` struct and implementation, delegating to submodules.
4. `tests.rs`: Moved all tests (~1800 lines) to a separate file.
