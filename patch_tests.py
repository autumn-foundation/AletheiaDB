import re

with open("src/mcp/tests.rs", "r") as f:
    content = f.read()

# Add exhaustive tests for invalid argument parsing
test_code = """
    #[test]
    fn test_invalid_argument_parsing_exhaustive() {
        let server = create_test_server();

        // 1. empty objects (parse_args fail)
        assert!(server.handle_get_node(json!({})).is_error.unwrap_or(false));
        assert!(server.handle_create_node(json!({})).is_error.unwrap_or(false));
        assert!(server.handle_update_node(json!({})).is_error.unwrap_or(false));
        assert!(server.handle_delete_node(json!({})).is_error.unwrap_or(false));
        assert!(server.handle_delete_node_cascade(json!({})).is_error.unwrap_or(false));
        assert!(server.handle_list_nodes(json!({})).is_error.unwrap_or(false));
        assert!(server.handle_count_nodes(json!({})).is_error.unwrap_or(false));

        assert!(server.handle_get_edge(json!({})).is_error.unwrap_or(false));
        assert!(server.handle_create_edge(json!({})).is_error.unwrap_or(false));
        assert!(server.handle_update_edge(json!({})).is_error.unwrap_or(false));
        assert!(server.handle_delete_edge(json!({})).is_error.unwrap_or(false));
        assert!(server.handle_list_edges(json!({})).is_error.unwrap_or(false));
        assert!(server.handle_count_edges(json!({})).is_error.unwrap_or(false));

        assert!(server.handle_get_outgoing_edges(json!({})).is_error.unwrap_or(false));
        assert!(server.handle_get_incoming_edges(json!({})).is_error.unwrap_or(false));
        assert!(server.handle_traverse(json!({})).is_error.unwrap_or(false));
        assert!(server.handle_find_similar(json!({})).is_error.unwrap_or(false));
        assert!(server.handle_enable_vector_index(json!({})).is_error.unwrap_or(false));

        assert!(server.handle_get_node_at_time(json!({})).is_error.unwrap_or(false));
        assert!(server.handle_get_edge_at_time(json!({})).is_error.unwrap_or(false));

        assert!(server.handle_get_node_at_valid_time(json!({})).is_error.unwrap_or(false));
        assert!(server.handle_get_node_at_transaction_time(json!({})).is_error.unwrap_or(false));
        assert!(server.handle_get_node_history(json!({})).is_error.unwrap_or(false));
        assert!(server.handle_diff_node_versions(json!({})).is_error.unwrap_or(false));

        assert!(server.handle_get_edge_at_valid_time(json!({})).is_error.unwrap_or(false));
        assert!(server.handle_get_edge_at_transaction_time(json!({})).is_error.unwrap_or(false));
        assert!(server.handle_get_edge_history(json!({})).is_error.unwrap_or(false));
        assert!(server.handle_diff_edge_versions(json!({})).is_error.unwrap_or(false));

        assert!(server.handle_hybrid_query(json!({})).is_error.unwrap_or(false));
    }
"""

content = content.replace("    #[test]\n    fn test_invalid_argument_parsing() {", test_code + "\n    #[test]\n    fn test_invalid_argument_parsing() {")

with open("src/mcp/tests.rs", "w") as f:
    f.write(content)

print("Added exhaustive test.")
