1. **Understand the problem**:
   - The test coverage on the modified diff is `79.06%`, and it must be `85.00%`.
   - Codecov warns about specific lines being uncovered, e.g., line 1506, 1539, 1547-1549, 1576, 1600, etc.
2. **Examine the untested lines**:
   - For line 1547-1549, looking at the previous diffs, these correspond to the `parse_optional_tx_time_arg` guard branches in the temporal endpoints (`handle_get_edge_at_time`, `handle_get_node_at_time`, etc).
   - For line 846, 897, 927: `unwrap_err` for `parse_node_id`, `parse_edge_id`, etc.
   - Wait, didn't we just add a test specifically for these? Ah! `invalid_node` and `invalid_edge` in `test_invalid_node_edge_id_parsing` was a valid JSON structure (`json!({ "node_id": "not-a-number", ... })`). But wait, in `test_invalid_argument_parsing_exhaustive` we used `bad_args` where `node_id` is `"not-a-number"`.
   - `serde_json::from_value` parses strictly according to types! If `node_id` expects a `u64`, and we provide `"not-a-number"`, `serde_json::from_value` will return an `Err`. This means it hits the FIRST guard clause `Ok(req) = self.parse_args(args) else { ... }`, and NEVER REACHES the second guard clause `self.parse_node_id(req.node_id)`.
   - `self.parse_node_id` is a function taking `u64` (because `req.node_id` is parsed as `u64` by Serde). If Serde already parsed it as `u64`, `NodeId::new` will almost always succeed unless there's a specific validation (like 0 or reserved ID).
   Wait, does `NodeId::new` ever fail for a `u64`? Let's check `src/core/id.rs`. NodeId might fail if it's reserved.
   - For timestamps, `valid_time: String`. If we pass `"invalid"`, Serde successfully parses it as a String, so `parse_args` succeeds! Then `parse_timestamp_arg` fails. This should be covered. But wait, `bad_args` used `"valid_time": 123`. Serde expects a String, so it fails at `parse_args`!
3. **Fix the tests**:
   - To test `parse_node_id` failure, we need to provide a `u64` that makes `NodeId::new` fail (e.g. maybe 0 if it's invalid?).
   - To test `parse_timestamp_arg` failure, we need to provide a valid JSON String for `valid_time` but an invalid format (e.g. `"not-a-timestamp"`).
   - To test `parse_optional_tx_time_arg` failure, we need `transaction_time: "invalid-time"`.
4. **Implement**:
   - Create a specific bad request that passes Serde (`parse_args`) but fails `parse_node_id`, `parse_edge_id`, `parse_timestamp_arg`, `parse_optional_tx_time_arg`.
   - Run coverage locally or just push.
