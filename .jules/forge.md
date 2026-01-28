## 2024-05-23 - God Object Decomposition
**Learning:** `src/db.rs` was a classic God Object, mixing core database coordination with low-level persistence logic. Splitting it required careful handling of private helper functions and internal types that were implicitly shared.
**Action:** When extracting modules, first identify the "seams" - groups of functions that share state (like `PersistenceTracker`). Move the state struct first, then the functions that operate on it. Be wary of `Arc` vs `Option` types during refactoring, as `Option` inhibits auto-deref coercion.
