import re

with open("src/mcp/tests.rs", "r") as f:
    content = f.read()

# Add exhaustive tests for parse_node_id and parse_edge_id
test_code = """
    #[test]
    fn test_invalid_node_edge_id_parsing() {
        let server = create_test_server();
        let invalid_node = json!({ "node_id": "not-a-number", "label": "L", "properties": {}, "valid_time": "2023-01-01T00:00:00Z", "transaction_time": "2023-01-01T00:00:00Z" });
        let invalid_edge = json!({ "edge_id": "not-a-number", "label": "L", "from_node": 1, "to_node": 2, "properties": {}, "valid_time": "2023-01-01T00:00:00Z", "transaction_time": "2023-01-01T00:00:00Z" });
        let invalid_start_node = json!({ "start_node_id": "not-a-number", "direction": "out", "max_depth": 1 });

        assert!(server.handle_get_node(invalid_node.clone()).is_error.unwrap_or(false));
        assert!(server.handle_update_node(invalid_node.clone()).is_error.unwrap_or(false));
        assert!(server.handle_delete_node(invalid_node.clone()).is_error.unwrap_or(false));
        assert!(server.handle_delete_node_cascade(invalid_node.clone()).is_error.unwrap_or(false));

        assert!(server.handle_get_edge(invalid_edge.clone()).is_error.unwrap_or(false));
        assert!(server.handle_update_edge(invalid_edge.clone()).is_error.unwrap_or(false));
        assert!(server.handle_delete_edge(invalid_edge.clone()).is_error.unwrap_or(false));

        assert!(server.handle_get_outgoing_edges(invalid_node.clone()).is_error.unwrap_or(false));
        assert!(server.handle_get_incoming_edges(invalid_node.clone()).is_error.unwrap_or(false));
        assert!(server.handle_traverse(invalid_start_node).is_error.unwrap_or(false));

        assert!(server.handle_get_node_at_time(invalid_node.clone()).is_error.unwrap_or(false));
        assert!(server.handle_get_edge_at_time(invalid_edge.clone()).is_error.unwrap_or(false));

        assert!(server.handle_get_node_at_valid_time(invalid_node.clone()).is_error.unwrap_or(false));
        assert!(server.handle_get_node_at_transaction_time(invalid_node.clone()).is_error.unwrap_or(false));
        assert!(server.handle_get_node_history(invalid_node.clone()).is_error.unwrap_or(false));
        assert!(server.handle_diff_node_versions(invalid_node.clone()).is_error.unwrap_or(false));

        assert!(server.handle_get_edge_at_valid_time(invalid_edge.clone()).is_error.unwrap_or(false));
        assert!(server.handle_get_edge_at_transaction_time(invalid_edge.clone()).is_error.unwrap_or(false));
        assert!(server.handle_get_edge_history(invalid_edge.clone()).is_error.unwrap_or(false));
        assert!(server.handle_diff_edge_versions(invalid_edge.clone()).is_error.unwrap_or(false));
    }
"""

content = content.replace("    #[test]\n    fn test_invalid_argument_parsing_exhaustive() {", test_code + "\n    #[test]\n    fn test_invalid_argument_parsing_exhaustive() {")

with open("src/mcp/tests.rs", "w") as f:
    f.write(content)

print("Added node/edge id tests.")
