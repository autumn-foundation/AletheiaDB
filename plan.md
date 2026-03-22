1. **Understand the problem**:
   - The code coverage check (Codecov) is failing because the newly added refactored lines (mostly the `Err(err) => return err` and `return self.parse_node_id(...).unwrap_err()` lines) are not covered by existing tests.
   - We need to hit at least 85% coverage on the new diff.
2. **Add unit tests**:
   - Since we added `parse_args` and `parse_node_id` usage, these error paths (the `Err` branches in matching and the `unwrap_err` in guard clauses) need tests.
   - We can either write specific unit tests in `src/mcp/tests.rs` for invalid inputs, or just test the helper methods themselves if the methods are covered. Wait, we modified the handlers, so we need to test the handlers directly.
   - Let's look at `src/mcp/tests.rs` to see what tests exist and add more invalid input tests to hit the `Err` and `unwrap_err` branches for `parse_args`, `parse_node_id`, `parse_edge_id`, `parse_timestamp_arg`, `parse_optional_tx_time_arg`.
3. **Execute testing**:
   - Write tests for some methods like `handle_create_node`, `handle_get_node` with invalid inputs (`node_id: null`, invalid string, etc.) to trigger the error branches.
   - Run `cargo clippy` and `cargo test --lib mcp::tests` to verify.
4. **Run pre-commit steps**.
5. **Submit**.
