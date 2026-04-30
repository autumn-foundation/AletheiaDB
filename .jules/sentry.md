**[Fix `execute_traversal` target shards bug]**
**Learning:** `plan.steps` from `route_traversal` returns empty list, and we need `involved_shards`
**Action:** Replace `steps` with `involved_shards` and update the test.
