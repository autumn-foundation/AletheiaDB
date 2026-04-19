# Echo's DX Report

## Summary
The "Developer Experience" audit revealed several friction points in the documentation and examples. While the core functionality works, the examples are verbose and miss some convenient shortcuts that would make the library easier to use.

## Findings

### 1. Verbose Property Construction
The `README.md` and examples use `PropertyMapBuilder::new().insert(...).build()` extensively. This is verbose and repetitive.
**Observation:** The `properties!` macro exists and provides a much cleaner syntax:
```rust
properties! {
    "name" => "Alice",
    "age" => 30,
}
```
**Recommendation:** Update documentation and examples to prioritize the macro.

### 2. Verbose Imports
The `README.md` uses deep import paths like `aletheiadb::index::vector::HnswConfig`.
**Observation:** Shorter re-exports are available at the crate root, e.g., `aletheiadb::HnswConfig`.
**Recommendation:** Use shorter imports in documentation to reduce boilerplate.

### 3. Missing `tokio` Dependency Instruction
The "Embedding Generation" example uses `#[tokio::main]`, which implies a dependency on `tokio`. However, the instructions only mention enabling the `embedding-openai` feature.
**Observation:** A new user copying the example will encounter a compilation error unless they know to add `tokio` to their `Cargo.toml`.
**Recommendation:** Add an explicit instruction to add `tokio` as a dependency.

### 4. Experimental Feature Warnings
Running examples with the `nova` feature (e.g., `story_demo`) produces warnings from the library code itself (missing docs).
**Observation:** This creates noise for the user.
**Recommendation:** While this is expected for experimental features, it's worth noting. (No immediate action for this report, just an observation).

## Action Items
- Refactor `README.md` examples to use `properties!` and shorter imports.
- Update `examples/story_demo.rs` to use `properties!`.
- Add `tokio` dependency note to `README.md`.
