## 2026-01-28 - Decoupling Index Persistence
**Tangle:** `src/db.rs` was a "God Object" acting as a coordinator but also implementing detailed persistence logic for vector, graph, and temporal indexes. It contained `PersistenceTracker` and background thread management, coupling the main DB structure to low-level persistence operations.
**Blueprint:** Extracted persistence logic into `src/storage/index_persistence/` submodules:
- `tracker.rs`: Moved `PersistenceTracker`.
- `operations.rs`: Moved core persistence functions (`persist_vector_indexes`, `load_vector_indexes`, etc.).
- `background.rs`: Moved background thread logic.
This reduced `src/db.rs` complexity and improved cohesion within the `storage` module.
