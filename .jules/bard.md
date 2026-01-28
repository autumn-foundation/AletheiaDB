
## 2024-05-23 - Outdated Vector Index Docs
**Confusion:** The `src/index/vector/mod.rs` documentation contained outdated comments stating "no VectorIndex implementation exists yet" and referencing future phases, despite `HnswIndex` being fully implemented. The examples were also marked `no_run`.
**Clarification:** Updated the documentation to reflect that `HnswIndex` is the concrete implementation. Examples were updated to use `ignore` (as they require setup) and the text was updated to describe the current state of the implementation.
