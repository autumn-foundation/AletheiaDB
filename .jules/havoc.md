**[Refactored RwLock to parking_lot]**
**Learning:** `std::sync::RwLock` blocks readers when writers are waiting, causing deadlocks if threads hold `read()` locks and request `write()` locks across different `RwLock` instances in cycles.
**Action:** Replaced `std::sync::RwLock` with `parking_lot::RwLock` in `coordinator.rs`, completely dropping the `PoisonError` complexity.
