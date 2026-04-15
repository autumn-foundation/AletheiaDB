1. **IDENTIFY**
   - The CI is failing because `aletheiadb::sql` cannot be found in `tests/havoc_vector_parser.rs`. The `tests/havoc_vector_parser.rs` file does not import `aletheiadb::sql` behind `#[cfg(feature = "sql")]` correctly when run without the `sql` feature, OR the test itself needs to be gated behind `#[cfg(feature = "sql")]`.
   - In `tests/havoc_vector_parser.rs`, I simply wrote `use aletheiadb::sql::parse_sql;` and the test itself. Because tests are compiled with and without features, it fails when the `sql` feature is NOT enabled.

2. **FIX**
   - Add `#![cfg(feature = "sql")]` to the top of `tests/havoc_vector_parser.rs` so that the entire test file is ignored if the `sql` feature is not enabled.

3. **VERIFY**
   - Run `cargo test --test havoc_vector_parser` to confirm it skips or compiles successfully.

4. **SUBMIT**
   - Commit the changes and push.
