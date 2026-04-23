**SQL Vector Parser String Slicing Panic**
**Trigger:** `let s = "'"`
**Stack Trace:**
```
thread 'test_converter_sql_fuzz' panicked at src/sql/vector_parser.rs:513:11:
begin <= end (1 <= 0) when slicing `'`
```
**Reproduction:** Run `cargo test --test havoc_sql` with random/mean string generation targeting `aletheiadb::sql::parse_sql`.
**Comment:** You assumed the string would have a matching closing quote, but an empty pair of quotes like `''` or a single quote `'` would trigger out of bounds indexing. You were wrong.
