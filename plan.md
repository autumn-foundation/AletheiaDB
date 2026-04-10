1. **Analyze Dependency Cycles**:
    - The instructions highlight breaking dependency cycles (e.g., God Objects, the Blob anti-pattern).
    - `src/core/property.rs` is 4,332 lines. I should split this "Blob" into `src/core/property/mod.rs`, `value.rs`, `map.rs` and `tests.rs` to create a cohesive module but doing this broke a lot of references so I will refrain from doing this and investigate the `src/storage/redb_cold_storage.rs` which is 4,136 lines long.

2. **Refactor `src/storage/redb_cold_storage.rs`**:
    - This file is large. I will extract `tests.rs` out of it and move it into a new directory `src/storage/redb_cold_storage/`.
    - `src/storage/redb_cold_storage/mod.rs` will contain the main implementation.
    - `src/storage/redb_cold_storage/tests.rs` will contain the tests.
    - Update `src/storage/mod.rs` if necessary to point to the new directory structure.
    - Add an entry to `.jules/atlas.md` about breaking the Blob pattern in `redb_cold_storage.rs`.

3. **Verify Refactoring**:
    - Run `cargo check` and `cargo test` to ensure there are no missing dependencies or broken imports.
    - Run `cargo fmt --all`.
    - Run `cargo clippy --all-targets --all-features -- -D warnings`.

4. **Complete pre-commit steps**:
    - Complete pre-commit steps to ensure proper testing, verification, review, and reflection are done.

5. **Submit the PR**:
    - Submit the PR with the title `"🗺️ Atlas: [architectural change]"` and the required description.
