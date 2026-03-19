# 👺 Havoc: Edge Cases Found
- Replaced manual array divisions `data.len() / effective_k` with `.checked_div()` to avoid integer overflow edge cases in `src/experimental/cartographer.rs`.
- Fixed `total_vectors / node_count` to `.checked_div(node_count).unwrap_or(0)` in `src/index/vector/distributed.rs`.
- Added strict `.sort_by_key(|b| std::cmp::Reverse(b.0))` instead of manual `sort_by(|a, b| b.0.cmp(&a.0))` in `src/sql/temporal_parser.rs` and `src/storage/migration.rs` to optimize the algorithms.

Fuzzing `aletheiadb::sql::parse_sql` uncovered minor issues successfully, and loom/proptests verified concurrency.
