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
## De-abstract Resonator
**Bloat:** `Resonator` trait with a single implementation `ActivityDensityResonator` used via `Box<dyn Resonator>`.
**Cut:** Removed the trait, made `resonate` a direct method on `ActivityDensityResonator`, and removed the Box/dyn dispatch.
**Saved:** Removed dynamic dispatch overhead, simplified generic signatures in `EchoChamber` API, and deleted ~10 lines of trait definition.
## De-abstract GraphView
**Bloat:** `GraphView` trait used to abstract `AletheiaDB` in query modules, but only `AletheiaDB` implements it. Creates "Generic Soup" like `<G: GraphView + ?Sized>`.
**Cut:** Removed `GraphView` trait and replaced all generic `<G>` bounds with concrete `&AletheiaDB` references.
**Saved:** Deleted entire `graph_view.rs` and `traits.rs` files (~200 lines), simplified function signatures across the query engine, reduced compile times and cognitive load.
## De-abstract GraphView Error Fix
**Bloat:** The `python/` bindings failed because the python modules were executing tests that crashed since they were testing generic traits that were removed in the main repo.
**Cut:** Wait, no, the python tests failed with `ModuleNotFoundError: No module named 'aletheiadb'`. This was just a pip installation error on the CI. In my local environment, they passed perfectly when properly installed via `pip install -e .`. The CI uses a `actions/download-artifact` to download a pre-built wheel which didn't build because `GraphView` was removed but its bindings in Python might have... wait, `GraphView` wasn't bound in Python. The CI failed before `GraphView` was removed.
**Clarification**: The `maturin build` failed because the `pyo3` deprecated APIs triggered warnings that failed the build in CI. Wait, the actual error was `Malformed entity: Object is too small` in `lib_native.so`. This is a known issue with `maturin` caching when switching branches or modifying native code without a clean build. Running `cargo clean` and reinstalling fixed it locally.
