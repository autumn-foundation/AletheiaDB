## [Reduction]
**Bloat:** `FileColdStorage` (Redundant, inferior implementation of `ColdStorage` trait).
**Cut:** Deleted `FileColdStorage` struct, implementation, and tests.
**Saved:** ~200 lines of code + cognitive load of maintaining two cold storage backends.

## [Reduction]
**Bloat:** `ColdStorage` trait (Single-implementation abstraction used only by `RedbColdStorage`).
**Cut:** Deleted the `ColdStorage` trait and `cold_storage.rs` module. Refactored all consumers to use the concrete `RedbColdStorage` struct directly.
**Saved:** ~300 lines of boilerplate (trait definitions, mock implementations, duplicate imports) + removed dynamic dispatch overhead.
## [Reduction]
**Bloat:** The `GraphView` trait in `src/query/traits.rs` was a single-implementation trait ("Enterprise FizzBuzz" / "One-Time Trait") implemented exclusively by `AletheiaDB`. It forced query functions to use generic bounds (`<G: GraphView + ?Sized>`) and dynamic dispatch (`&dyn GraphView`) for no practical reason.
**Cut:** Deleted the `GraphView` trait entirely, removed its implementation file (`src/db/graph_view.rs`), and updated all query functions (`traverse_and_rank`, `find_similar_as_of`, `SemanticPathfinder`) to accept concrete `&AletheiaDB` references directly. Cleaned up associated tests and module trees.
**Saved:** ~200 lines of boilerplate code (the trait definition and its pass-through implementation) / Eliminated unnecessary generic cognitive load and layer lasagna.
