## [Reduction]
**Bloat:** `FileColdStorage` (Redundant, inferior implementation of `ColdStorage` trait).
**Cut:** Deleted `FileColdStorage` struct, implementation, and tests.
**Saved:** ~200 lines of code + cognitive load of maintaining two cold storage backends.

## [Reduction]
**Bloat:** `ColdStorage` trait (Single-implementation abstraction used only by `RedbColdStorage`).
**Cut:** Deleted the `ColdStorage` trait and `cold_storage.rs` module. Refactored all consumers to use the concrete `RedbColdStorage` struct directly.
**Saved:** ~300 lines of boilerplate (trait definitions, mock implementations, duplicate imports) + removed dynamic dispatch overhead.

## [Reduction]
**Bloat:** `StorageSnapshot` and `FieldHolder` traits.
**Cut:** Deleted single-implementation traits `StorageSnapshot` (implemented only by `CurrentStorageSnapshot`) and `FieldHolder` (implemented only by `Event`, unused except in tests). Moved methods directly to structs.
**Saved:** ~50 lines of boilerplate + cognitive load of unnecessary abstraction layers.
## [Reduction]
**De-Abstracted GraphView**
**Bloat:** A `GraphView` trait that was only implemented by exactly one struct (`AletheiaDB`), adding an unnecessary layer of indirection and decoupling between query components and the main database structure.
**Cut:** Deleted the `GraphView` trait and `graph_view.rs` adapter file. Modifed all dependencies (like `query::hybrid` and `SemanticPathfinder`) to directly accept references to the concrete `AletheiaDB` struct.
**Saved:** Reduced code complexity, removed a "One-Time" Trait layer, and eliminated unnecessary generic `<G: GraphView>` bounds across query modules.
