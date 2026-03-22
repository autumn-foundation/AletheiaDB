import re

with open("src/mcp/tests.rs", "r") as f:
    content = f.read()

# Replace json!({}) with bad json for all assertions in test_invalid_argument_parsing_exhaustive
bad_json = "json!({ \"limit\": \"not-a-number\", \"node_id\": \"not-a-number\", \"edge_id\": \"not-a-number\", \"label\": 123, \"start_node_id\": \"not-a-number\", \"query\": 123, \"property_key\": 123, \"property_value\": 123, \"properties\": 123, \"from_node\": \"not-a-number\", \"to_node\": \"not-a-number\" })"

# Just replacing the entire body of the function
new_fn = """    fn test_invalid_argument_parsing_exhaustive() {
        let server = create_test_server();
        let bad_args = json!({ "limit": "not-a-number", "node_id": "not-a-number", "edge_id": "not-a-number", "label": 123, "start_node_id": "not-a-number", "query": 123, "property_key": 123, "property_value": 123, "properties": 123, "from_node": "not-a-number", "to_node": "not-a-number" });

        assert!(server.handle_get_node(bad_args.clone()).is_error.unwrap_or(false));
        assert!(server.handle_create_node(bad_args.clone()).is_error.unwrap_or(false));
        assert!(server.handle_update_node(bad_args.clone()).is_error.unwrap_or(false));
        assert!(server.handle_delete_node(bad_args.clone()).is_error.unwrap_or(false));
        assert!(server.handle_delete_node_cascade(bad_args.clone()).is_error.unwrap_or(false));
        assert!(server.handle_list_nodes(bad_args.clone()).is_error.unwrap_or(false));
        assert!(server.handle_count_nodes(bad_args.clone()).is_error.unwrap_or(false));

        assert!(server.handle_get_edge(bad_args.clone()).is_error.unwrap_or(false));
        assert!(server.handle_create_edge(bad_args.clone()).is_error.unwrap_or(false));
        assert!(server.handle_update_edge(bad_args.clone()).is_error.unwrap_or(false));
        assert!(server.handle_delete_edge(bad_args.clone()).is_error.unwrap_or(false));
        assert!(server.handle_list_edges(bad_args.clone()).is_error.unwrap_or(false));
        assert!(server.handle_count_edges(bad_args.clone()).is_error.unwrap_or(false));

        assert!(server.handle_get_outgoing_edges(bad_args.clone()).is_error.unwrap_or(false));
        assert!(server.handle_get_incoming_edges(bad_args.clone()).is_error.unwrap_or(false));
        assert!(server.handle_traverse(bad_args.clone()).is_error.unwrap_or(false));
        assert!(server.handle_find_similar(bad_args.clone()).is_error.unwrap_or(false));
        assert!(server.handle_enable_vector_index(bad_args.clone()).is_error.unwrap_or(false));

        assert!(server.handle_get_node_at_time(bad_args.clone()).is_error.unwrap_or(false));
        assert!(server.handle_get_edge_at_time(bad_args.clone()).is_error.unwrap_or(false));

        assert!(server.handle_get_node_at_valid_time(bad_args.clone()).is_error.unwrap_or(false));
        assert!(server.handle_get_node_at_transaction_time(bad_args.clone()).is_error.unwrap_or(false));
        assert!(server.handle_get_node_history(bad_args.clone()).is_error.unwrap_or(false));
        assert!(server.handle_diff_node_versions(bad_args.clone()).is_error.unwrap_or(false));

        assert!(server.handle_get_edge_at_valid_time(bad_args.clone()).is_error.unwrap_or(false));
        assert!(server.handle_get_edge_at_transaction_time(bad_args.clone()).is_error.unwrap_or(false));
        assert!(server.handle_get_edge_history(bad_args.clone()).is_error.unwrap_or(false));
        assert!(server.handle_diff_edge_versions(bad_args.clone()).is_error.unwrap_or(false));

        assert!(server.handle_hybrid_query(bad_args.clone()).is_error.unwrap_or(false));
    }"""

pattern = re.compile(r'    fn test_invalid_argument_parsing_exhaustive\(\) \{.*?(?=    #\[test\])', re.DOTALL)
content = pattern.sub(new_fn + "\n\n", content)

with open("src/mcp/tests.rs", "w") as f:
    f.write(content)
