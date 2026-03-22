1. **Understand the problem:** Codecov check is failing because our added tests did not cover enough lines of the modified diff. We have ~56% coverage on the new lines and need 85%.
   - Most of the lines not covered are the `return err` or `return self.parse_...(req.x).unwrap_err()` lines that are error guard branches.
2. **Examine the untested lines:**
   - e.g. Line 840, 858, 887: these are `return err` blocks when `self.parse_args` fails, or `unwrap_err` on `self.parse_node_id` in other `handle_*` functions.
3. **Write tests:**
   - Instead of writing individual tests for every single handler `handle_update_node`, `handle_delete_node`, `handle_list_nodes`, we can write a simple iteration test in `src/mcp/tests.rs`.
   - The test will just call `server.handle_XXX(json!({ "node_id": "not-a-number", "edge_id": "not-a-number", "start_node_id": "not-a-number" }))` or `server.handle_XXX(json!({}))` (empty JSON to fail `parse_args`) for every single handler, which will hit all the `parse_args` error branches.
   - We will also call it with valid JSON args but invalid ID formats `{"node_id": "invalid"}` to trigger the `parse_node_id` and `parse_edge_id` errors.
   - This should easily boost coverage.
4. **Compile and test locally**.
5. **Submit**.
