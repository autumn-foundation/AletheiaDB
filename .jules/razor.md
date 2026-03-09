## [Reduction]
**Bloat:** `FileColdStorage` (Redundant, inferior implementation of `ColdStorage` trait).
**Cut:** Deleted `FileColdStorage` struct, implementation, and tests.
**Saved:** ~200 lines of code + cognitive load of maintaining two cold storage backends.

## [Reduction]
**Bloat:** `ColdStorage` trait (Single-implementation abstraction used only by `RedbColdStorage`).
**Cut:** Deleted the `ColdStorage` trait and `cold_storage.rs` module. Refactored all consumers to use the concrete `RedbColdStorage` struct directly.
**Saved:** ~300 lines of boilerplate (trait definitions, mock implementations, duplicate imports) + removed dynamic dispatch overhead.

## [Reduction]
**Bloat:** `SimilarToBuilder`, `RankBySimilarityBuilder`, and `FindSimilarBuilder` (Factory Factories for objects taking 1-2 arguments).
**Cut:** Deleted the 3 builder structs and replaced them with `similar_to_advanced`, `rank_by_similarity_advanced`, and `find_similar_advanced` methods on `QueryBuilder` directly.
**Saved:** ~250 lines of boilerplate code + cognitive load of learning auxiliary builders for simple parameter passing.
