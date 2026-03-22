import re

with open("src/mcp/tests.rs", "r") as f:
    content = f.read()

test_code = """
    #[test]
    fn test_invalid_node_edge_id_parsing() {
        let server = create_test_server();
        // Provide valid format for serde (u64 for IDs, strings for time)
        // But invalid semantically (0 is invalid NodeId usually, "invalid" is invalid timestamp)
        let invalid_node = json!({
            "node_id": 0,
            "start_node_id": 0,
            "label": "L",
            "properties": {},
            "valid_time": "not-a-timestamp",
            "transaction_time": "not-a-timestamp"
        });
        let invalid_edge = json!({
            "edge_id": 0,
            "label": "L",
            "from_node": 1,
            "to_node": 2,
            "properties": {},
            "valid_time": "not-a-timestamp",
            "transaction_time": "not-a-timestamp"
        });

        // These will pass `parse_args` but fail at `parse_node_id`, `parse_edge_id`, `parse_timestamp_arg`
        assert!(server.handle_get_node(invalid_node.clone()).is_error.unwrap_or(false));
        assert!(server.handle_update_node(invalid_node.clone()).is_error.unwrap_or(false));
        assert!(server.handle_delete_node(invalid_node.clone()).is_error.unwrap_or(false));
        assert!(server.handle_delete_node_cascade(invalid_node.clone()).is_error.unwrap_or(false));

        assert!(server.handle_get_edge(invalid_edge.clone()).is_error.unwrap_or(false));
        assert!(server.handle_update_edge(invalid_edge.clone()).is_error.unwrap_or(false));
        assert!(server.handle_delete_edge(invalid_edge.clone()).is_error.unwrap_or(false));

        assert!(server.handle_get_outgoing_edges(invalid_node.clone()).is_error.unwrap_or(false));
        assert!(server.handle_get_incoming_edges(invalid_node.clone()).is_error.unwrap_or(false));
        assert!(server.handle_traverse(invalid_node.clone()).is_error.unwrap_or(false));

        // Node At Time
        let time_node = json!({ "node_id": 1, "valid_time": "invalid", "transaction_time": "invalid" });
        assert!(server.handle_get_node_at_time(time_node.clone()).is_error.unwrap_or(false));
        assert!(server.handle_get_node_at_valid_time(time_node.clone()).is_error.unwrap_or(false));
        assert!(server.handle_get_node_at_transaction_time(time_node.clone()).is_error.unwrap_or(false));
        assert!(server.handle_get_node_history(invalid_node.clone()).is_error.unwrap_or(false));
        assert!(server.handle_diff_node_versions(invalid_node.clone()).is_error.unwrap_or(false));

        // Edge At Time
        let time_edge = json!({ "edge_id": 1, "valid_time": "invalid", "transaction_time": "invalid" });
        assert!(server.handle_get_edge_at_time(time_edge.clone()).is_error.unwrap_or(false));
        assert!(server.handle_get_edge_at_valid_time(time_edge.clone()).is_error.unwrap_or(false));
        assert!(server.handle_get_edge_at_transaction_time(time_edge.clone()).is_error.unwrap_or(false));
        assert!(server.handle_get_edge_history(invalid_edge.clone()).is_error.unwrap_or(false));
        assert!(server.handle_diff_edge_versions(invalid_edge.clone()).is_error.unwrap_or(false));
    }
"""

pattern = re.compile(r'    #\[test\]\n    fn test_invalid_node_edge_id_parsing\(\) \{.*?(?=    #\[test\])', re.DOTALL)
content = pattern.sub(test_code + "\n", content)

with open("src/mcp/tests.rs", "w") as f:
    f.write(content)
