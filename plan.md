1. **Explore the codebase for refactoring opportunities.**
   - We already found a repetitive block in `src/mcp/server.rs` involving `serde_json::from_value(args)` where it had a lot of nested blocks.
   - We created `parse_args` helper function to reuse code and reduced repetitions in `handle_*` functions.
2. **Continue refactoring `src/mcp/server.rs` to flatten structure.**
   - In functions like `handle_get_node`, we can avoid nesting `match` statements by using early returns.
   - e.g. for `let node_id = match NodeId::new(req.node_id) { ... }` use `let Ok(node_id) = NodeId::new(req.node_id) else { ... }`.
   - Wait, `parse_args` now uses a match, we can potentially use guard clauses `let Ok(req) = ... else { return ... }` if we fix the double execution error we ran into. However, since the `Err` is already the `CallToolResult` we can't easily extract it from `else` branch without doing `.unwrap_err()`. We can use `let req = match ...` as it is simple enough.
3. **Check other repetitive blocks like `NodeId::new(...)` and `EdgeId::new(...)`.**
   - Extract error handling into a helper method or use `Result` combinators.
4. **Run pre-commit instructions.**
   - Complete pre-commit steps to ensure proper testing, verification, review, and reflection are done.
5. **Create the PR/Submit the change.**
   - Use the `submit` tool to finalize the refactoring.
