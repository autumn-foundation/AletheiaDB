Let's see what `test_corrupted_version_chain_delta_no_prev_version` tests exactly:
- Creates an anchor version
- Creates a delta version with `prev_version = None` (mutating the `NodeVersion::new_delta` output)
- Puts it into hot storage using `insert_restored_node_version`
- Calls `reconstruct_node_properties(v1_id)`
- Asserts error is `CorruptedVersionChain` with "no previous version".

The edge version logic `reconstruct_edge_properties_iterative` does the exact same thing but has no test covering the error path. So that is a missing test.

Second, `test_version_chain_reconstruction_multi_hop_deltas` tests that a long delta chain for nodes correctly reconstructs properties.
Edge version has a similar `test_edge_reconstruction_with_long_delta_chain`? Wait! Let's check!
