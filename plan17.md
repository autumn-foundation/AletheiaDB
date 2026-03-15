The error is: `error[E0432]: unresolved import aletheiadb::sql`.
This is in `tests/havoc/sql_parser_dos.rs:3:21`.
`use aletheiadb::sql::parse_sql;`

Wait, `tests/havoc/sql_parser_dos.rs` is NOT gated by `#[cfg(feature = "sql")]`!
So when `cargo test` is run without `--features sql`, it tries to compile `tests/havoc.rs` which includes `tests/havoc/sql_parser_dos.rs`.
But the `sql` module in `aletheiadb` is gated behind `#[cfg(feature = "sql")]`.
So the test file needs `#[cfg(feature = "sql")]` at the top!

Let's modify `tests/havoc/sql_parser_dos.rs` to include `#![cfg(feature = "sql")]`.
Or we can modify `tests/havoc/mod.rs` to conditionally include the module:
```rust
#[cfg(feature = "sql")]
mod sql_parser_dos;
```
Let's see what `tests/havoc/mod.rs` currently looks like.
