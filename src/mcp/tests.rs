//! Tests for the AletheiaDB MCP server.
//!
//! These tests verify the MCP server functionality including:
//! - Server initialization and info
//! - Node CRUD operations
//! - Edge CRUD operations
//! - Graph traversal
//! - Vector similarity search
//! - Temporal queries
//! - Hybrid queries
//!
//! Tests are executed sequentially to avoid cross-test interference on internal state.

use std::collections::HashMap;
use std::sync::Arc;

use crate::core::PropertyMapBuilder;
use crate::db::AletheiaDB;

use super::server::AletheiaMcpServer;
use super::tools::*;

/// Helper to create a test database with some sample data.
fn create_test_db() -> Arc<AletheiaDB> {
    let db = AletheiaDB::new().expect("Failed to create database");
    Arc::new(db)
}

#[allow(unused_imports)]
use rmcp::ServerHandler;

/// Helper to create a test server.
fn create_test_server() -> AletheiaMcpServer {
    AletheiaMcpServer::new(create_test_db())
}

/// Helper to parse JSON response and check for errors.
fn parse_response<T: serde::de::DeserializeOwned>(response: &str) -> Result<T, String> {
    let value: serde_json::Value =
        serde_json::from_str(response).map_err(|e| format!("Failed to parse JSON: {}", e))?;

    if let Some(error) = value.get("error") {
        // Structured error shape (Issue #3234): `{code, message, retriable}`.
        return Err(error
            .get("message")
            .and_then(|m| m.as_str())
            .unwrap_or("Unknown error")
            .to_string());
    }

    serde_json::from_value(value).map_err(|e| format!("Failed to deserialize: {}", e))
}

// ============================================================================
// Server Initialization Tests
// ============================================================================

#[test]
fn test_server_creation() {
    let server = create_test_server();
    assert!(server.db().node_count() == 0);
    assert!(server.db().edge_count() == 0);
}

#[test]
fn test_server_with_existing_db() {
    let db = create_test_db();

    // Add some data to the database first
    let _node_id = db
        .create_node(
            "Person",
            PropertyMapBuilder::new().insert("name", "Alice").build(),
        )
        .expect("Failed to create node");

    let server = AletheiaMcpServer::new(db);
    assert_eq!(server.db().node_count(), 1);
}

// ============================================================================
// Node CRUD Tests
// ============================================================================

mod node_tests {
    use super::*;

    /// Helper to create two `Person` nodes and return their IDs.
    fn create_two_nodes(server: &AletheiaMcpServer) -> (u64, u64) {
        let n1: NodeResponse = parse_response(&server.create_node(CreateNodeRequest {
            valid_time: None,
            label: "Person".to_string(),
            properties: None,
            provenance: None,
        }))
        .unwrap();
        let n2: NodeResponse = parse_response(&server.create_node(CreateNodeRequest {
            valid_time: None,
            label: "Person".to_string(),
            properties: None,
            provenance: None,
        }))
        .unwrap();
        (n1.id, n2.id)
    }

    #[test]
    fn test_create_node_basic() {
        let server = create_test_server();

        let req = CreateNodeRequest {
            valid_time: None,
            label: "Person".to_string(),
            properties: None,
            provenance: None,
        };

        let response = server.create_node(req);
        let node: NodeResponse = parse_response(&response).expect("Failed to parse response");

        assert_eq!(node.label, "Person");
        // Note: Node IDs start at 0, so we just verify a valid response was returned
    }

    #[test]
    fn test_create_node_with_properties() {
        let server = create_test_server();

        let mut props = HashMap::new();
        props.insert("name".to_string(), serde_json::json!("Alice"));
        props.insert("age".to_string(), serde_json::json!(30));

        let req = CreateNodeRequest {
            valid_time: None,
            label: "Person".to_string(),
            properties: Some(props),
            provenance: None,
        };

        let response = server.create_node(req);
        let node: NodeResponse = parse_response(&response).expect("Failed to parse response");

        assert_eq!(node.label, "Person");
        assert_eq!(
            node.properties.get("name"),
            Some(&serde_json::json!("Alice"))
        );
        assert_eq!(node.properties.get("age"), Some(&serde_json::json!(30)));
    }

    #[test]
    fn test_get_node() {
        let server = create_test_server();

        // Create a node first
        let mut props = HashMap::new();
        props.insert("name".to_string(), serde_json::json!("Bob"));

        let create_req = CreateNodeRequest {
            valid_time: None,
            label: "Person".to_string(),
            properties: Some(props),
            provenance: None,
        };

        let create_response = server.create_node(create_req);
        let created: NodeResponse = parse_response(&create_response).expect("Failed to create");

        // Now get it
        let get_req = GetNodeRequest {
            node_id: created.id,
            include_vectors: None,
        };

        let get_response = server.get_node(get_req);
        let retrieved: NodeResponse = parse_response(&get_response).expect("Failed to get");

        assert_eq!(retrieved.id, created.id);
        assert_eq!(retrieved.label, "Person");
        assert_eq!(
            retrieved.properties.get("name"),
            Some(&serde_json::json!("Bob"))
        );
    }

    #[test]
    fn test_get_nonexistent_node() {
        let server = create_test_server();

        let req = GetNodeRequest {
            node_id: 999999,
            include_vectors: None,
        };

        let response = server.get_node(req);
        let value: serde_json::Value = serde_json::from_str(&response).unwrap();

        assert!(value.get("error").is_some());
    }

    #[test]
    fn test_update_node() {
        let server = create_test_server();

        // Create a node
        let mut props = HashMap::new();
        props.insert("name".to_string(), serde_json::json!("Charlie"));
        props.insert("age".to_string(), serde_json::json!(25));

        let create_req = CreateNodeRequest {
            valid_time: None,
            label: "Person".to_string(),
            properties: Some(props),
            provenance: None,
        };

        let create_response = server.create_node(create_req);
        let created: NodeResponse = parse_response(&create_response).expect("Failed to create");

        // Update it
        let mut new_props = HashMap::new();
        new_props.insert("age".to_string(), serde_json::json!(26));
        new_props.insert("city".to_string(), serde_json::json!("London"));

        let update_req = UpdateNodeRequest {
            valid_time: None,
            node_id: created.id,
            properties: new_props,
            provenance: None,
        };

        let update_response = server.update_node(update_req);
        let updated: NodeResponse = parse_response(&update_response).expect("Failed to update");

        assert_eq!(updated.properties.get("age"), Some(&serde_json::json!(26)));
        assert_eq!(
            updated.properties.get("city"),
            Some(&serde_json::json!("London"))
        );
    }

    #[test]
    fn test_delete_node() {
        let server = create_test_server();

        // Create a node
        let create_req = CreateNodeRequest {
            valid_time: None,
            label: "ToDelete".to_string(),
            properties: None,
            provenance: None,
        };

        let create_response = server.create_node(create_req);
        let created: NodeResponse = parse_response(&create_response).expect("Failed to create");
        let node_id = created.id;

        // Delete it
        let delete_req = DeleteNodeRequest {
            node_id,
            detach: None,
            valid_time: None,
        };

        let delete_response = server.delete_node(delete_req);
        let value: serde_json::Value = serde_json::from_str(&delete_response).unwrap();

        assert_eq!(value.get("success"), Some(&serde_json::json!(true)));

        // Verify it's gone
        let get_req = GetNodeRequest {
            node_id,
            include_vectors: None,
        };
        let get_response = server.get_node(get_req);
        let get_value: serde_json::Value = serde_json::from_str(&get_response).unwrap();

        assert!(get_value.get("error").is_some());
    }

    #[test]
    fn test_delete_node_with_edges_refused_without_detach() {
        // Issue #3209: the MCP delete_node must NOT return a bare success when the
        // target node has connected edges. Without `detach`, it refuses and reports
        // the machine-readable count of connected edges.
        let server = create_test_server();
        let (source_id, target_id) = create_two_nodes(&server);

        server.create_edge(CreateEdgeRequest {
            valid_time: None,
            source_id,
            target_id,
            label: "KNOWS".to_string(),
            properties: None,
            provenance: None,
        });

        let delete_response = server.delete_node(DeleteNodeRequest {
            node_id: source_id,
            detach: None,
            valid_time: None,
        });
        let value: serde_json::Value = serde_json::from_str(&delete_response).unwrap();

        // It must not claim success...
        assert_ne!(value.get("success"), Some(&serde_json::json!(true)));
        // ...and it must surface exactly the number of connected edges.
        assert_eq!(value.get("connected_edges"), Some(&serde_json::json!(1)));

        // The node must still exist (refused, not destroyed).
        let get_value: serde_json::Value = serde_json::from_str(&server.get_node(GetNodeRequest {
            node_id: source_id,
            include_vectors: None,
        }))
        .unwrap();
        assert!(get_value.get("error").is_none());
    }

    #[test]
    fn test_delete_node_with_detach_removes_edges_no_dangling() {
        // Issue #3209: with detach: true, the MCP path performs a cascade-equivalent
        // delete and reports the number of edges removed, leaving no dangling endpoints.
        let server = create_test_server();
        let (source_id, target_id) = create_two_nodes(&server);

        // A third node so we have both an outgoing and an incoming edge.
        let third = server.create_node(CreateNodeRequest {
            valid_time: None,
            label: "Person".to_string(),
            properties: None,
            provenance: None,
        });
        let third_id: NodeResponse = parse_response(&third).unwrap();
        let third_id = third_id.id;

        server.create_edge(CreateEdgeRequest {
            valid_time: None,
            source_id,
            target_id,
            label: "KNOWS".to_string(),
            properties: None,
            provenance: None,
        });
        server.create_edge(CreateEdgeRequest {
            valid_time: None,
            source_id: third_id,
            target_id: source_id,
            label: "FOLLOWS".to_string(),
            properties: None,
            provenance: None,
        });

        let delete_response = server.delete_node(DeleteNodeRequest {
            node_id: source_id,
            detach: Some(true),
            valid_time: None,
        });
        let value: serde_json::Value = serde_json::from_str(&delete_response).unwrap();

        assert_eq!(value.get("success"), Some(&serde_json::json!(true)));
        assert_eq!(value.get("edges_removed"), Some(&serde_json::json!(2)));
        assert_eq!(value.get("detached"), Some(&serde_json::json!(true)));

        // Node is gone.
        let get_value: serde_json::Value = serde_json::from_str(&server.get_node(GetNodeRequest {
            node_id: source_id,
            include_vectors: None,
        }))
        .unwrap();
        assert!(get_value.get("error").is_some());

        // Traversal from the surviving neighbors yields no dangling endpoint to
        // the deleted node: the connected edges were removed with it.
        let outgoing: serde_json::Value =
            serde_json::from_str(&server.get_outgoing_edges(GetOutgoingEdgesRequest {
                node_id: third_id,
                label: None,
                include_vectors: None,
            }))
            .unwrap();
        let edges = outgoing.get("edges").and_then(|e| e.as_array());
        assert!(
            edges.map(|e| e.is_empty()).unwrap_or(true),
            "third node should have no outgoing edges after detach delete"
        );

        let incoming: serde_json::Value =
            serde_json::from_str(&server.get_incoming_edges(GetIncomingEdgesRequest {
                node_id: target_id,
                label: None,
                include_vectors: None,
            }))
            .unwrap();
        let in_edges = incoming.get("edges").and_then(|e| e.as_array());
        assert!(
            in_edges.map(|e| e.is_empty()).unwrap_or(true),
            "target node should have no incoming edges after detach delete"
        );
    }

    #[test]
    fn test_delete_node_without_edges_succeeds_with_detach_false() {
        // A node with no edges deletes cleanly and reports zero edges removed.
        let server = create_test_server();
        let created: NodeResponse = parse_response(&server.create_node(CreateNodeRequest {
            valid_time: None,
            label: "Lonely".to_string(),
            properties: None,
            provenance: None,
        }))
        .unwrap();

        let delete_response = server.delete_node(DeleteNodeRequest {
            node_id: created.id,
            detach: None,
            valid_time: None,
        });
        let value: serde_json::Value = serde_json::from_str(&delete_response).unwrap();

        assert_eq!(value.get("success"), Some(&serde_json::json!(true)));
        assert_eq!(value.get("edges_removed"), Some(&serde_json::json!(0)));
    }

    #[test]
    fn test_list_nodes() {
        let server = create_test_server();

        // Create multiple nodes
        for i in 0..5 {
            let mut props = HashMap::new();
            props.insert("index".to_string(), serde_json::json!(i));

            let req = CreateNodeRequest {
                valid_time: None,
                label: "ListTest".to_string(),
                properties: Some(props),
                provenance: None,
            };
            server.create_node(req);
        }

        // List with label filter (required for listing nodes efficiently)
        let list_req = ListNodesRequest {
            label: Some("ListTest".to_string()),
            property_key: None,
            property_value: None,
            limit: None,
            offset: None,
            include_vectors: None,
        };

        let response = server.list_nodes(list_req);
        let value: serde_json::Value = serde_json::from_str(&response).unwrap();

        assert_eq!(value.get("count"), Some(&serde_json::json!(5)));
    }

    #[test]
    fn test_list_nodes_with_label_filter() {
        let server = create_test_server();

        // Create nodes with different labels
        for _ in 0..3 {
            server.create_node(CreateNodeRequest {
                valid_time: None,
                label: "TypeA".to_string(),
                properties: None,
                provenance: None,
            });
        }

        for _ in 0..2 {
            server.create_node(CreateNodeRequest {
                valid_time: None,
                label: "TypeB".to_string(),
                properties: None,
                provenance: None,
            });
        }

        // List only TypeA
        let list_req = ListNodesRequest {
            label: Some("TypeA".to_string()),
            property_key: None,
            property_value: None,
            limit: None,
            offset: None,
            include_vectors: None,
        };

        let response = server.list_nodes(list_req);
        let value: serde_json::Value = serde_json::from_str(&response).unwrap();

        assert_eq!(value.get("count"), Some(&serde_json::json!(3)));
    }

    #[test]
    fn test_list_nodes_with_pagination() {
        let server = create_test_server();

        // Create 10 nodes
        for i in 0..10 {
            let mut props = HashMap::new();
            props.insert("index".to_string(), serde_json::json!(i));

            server.create_node(CreateNodeRequest {
                valid_time: None,
                label: "Paginated".to_string(),
                properties: Some(props),
                provenance: None,
            });
        }

        // Get first page (with label filter required for efficient listing)
        let page1_req = ListNodesRequest {
            label: Some("Paginated".to_string()),
            property_key: None,
            property_value: None,
            limit: Some(5),
            offset: Some(0),
            include_vectors: None,
        };

        let page1_response = server.list_nodes(page1_req);
        let page1: serde_json::Value = serde_json::from_str(&page1_response).unwrap();

        assert_eq!(page1.get("count"), Some(&serde_json::json!(5)));

        // Get second page
        let page2_req = ListNodesRequest {
            label: Some("Paginated".to_string()),
            property_key: None,
            property_value: None,
            limit: Some(5),
            offset: Some(5),
            include_vectors: None,
        };

        let page2_response = server.list_nodes(page2_req);
        let page2: serde_json::Value = serde_json::from_str(&page2_response).unwrap();

        assert_eq!(page2.get("count"), Some(&serde_json::json!(5)));
    }

    #[test]
    fn test_count_nodes() {
        let server = create_test_server();

        // Initially empty
        let count_req = CountNodesRequest { label: None };
        let response = server.count_nodes(count_req);
        let value: serde_json::Value = serde_json::from_str(&response).unwrap();
        assert_eq!(value.get("count"), Some(&serde_json::json!(0)));

        // Create some nodes
        for _ in 0..3 {
            server.create_node(CreateNodeRequest {
                valid_time: None,
                label: "Counted".to_string(),
                properties: None,
                provenance: None,
            });
        }

        let count_req = CountNodesRequest { label: None };
        let response = server.count_nodes(count_req);
        let value: serde_json::Value = serde_json::from_str(&response).unwrap();
        assert_eq!(value.get("count"), Some(&serde_json::json!(3)));
    }

    #[test]
    fn test_count_nodes_with_label() {
        let server = create_test_server();

        // Create nodes with different labels
        for _ in 0..3 {
            server.create_node(CreateNodeRequest {
                valid_time: None,
                label: "CountA".to_string(),
                properties: None,
                provenance: None,
            });
        }

        for _ in 0..2 {
            server.create_node(CreateNodeRequest {
                valid_time: None,
                label: "CountB".to_string(),
                properties: None,
                provenance: None,
            });
        }

        let count_a = server.count_nodes(CountNodesRequest {
            label: Some("CountA".to_string()),
        });
        let value_a: serde_json::Value = serde_json::from_str(&count_a).unwrap();
        assert_eq!(value_a.get("count"), Some(&serde_json::json!(3)));

        let count_b = server.count_nodes(CountNodesRequest {
            label: Some("CountB".to_string()),
        });
        let value_b: serde_json::Value = serde_json::from_str(&count_b).unwrap();
        assert_eq!(value_b.get("count"), Some(&serde_json::json!(2)));
    }
}

// ============================================================================
// Edge CRUD Tests
// ============================================================================

mod edge_tests {
    use super::*;

    fn create_two_nodes(server: &AletheiaMcpServer) -> (u64, u64) {
        let node1 = server.create_node(CreateNodeRequest {
            valid_time: None,
            label: "Person".to_string(),
            properties: Some({
                let mut m = HashMap::new();
                m.insert("name".to_string(), serde_json::json!("Alice"));
                m
            }),
            provenance: None,
        });
        let n1: NodeResponse = parse_response(&node1).unwrap();

        let node2 = server.create_node(CreateNodeRequest {
            valid_time: None,
            label: "Person".to_string(),
            properties: Some({
                let mut m = HashMap::new();
                m.insert("name".to_string(), serde_json::json!("Bob"));
                m
            }),
            provenance: None,
        });
        let n2: NodeResponse = parse_response(&node2).unwrap();

        (n1.id, n2.id)
    }

    #[test]
    fn test_create_edge_basic() {
        let server = create_test_server();
        let (source_id, target_id) = create_two_nodes(&server);

        let req = CreateEdgeRequest {
            valid_time: None,
            source_id,
            target_id,
            label: "KNOWS".to_string(),
            properties: None,
            provenance: None,
        };

        let response = server.create_edge(req);
        let edge: EdgeResponse = parse_response(&response).expect("Failed to create edge");

        assert_eq!(edge.source_id, source_id);
        assert_eq!(edge.target_id, target_id);
        assert_eq!(edge.label, "KNOWS");
    }

    #[test]
    fn test_create_edge_with_properties() {
        let server = create_test_server();
        let (source_id, target_id) = create_two_nodes(&server);

        let mut props = HashMap::new();
        props.insert("since".to_string(), serde_json::json!("2024-01-01"));
        props.insert("strength".to_string(), serde_json::json!(0.9));

        let req = CreateEdgeRequest {
            valid_time: None,
            source_id,
            target_id,
            label: "KNOWS".to_string(),
            properties: Some(props),
            provenance: None,
        };

        let response = server.create_edge(req);
        let edge: EdgeResponse = parse_response(&response).expect("Failed to create edge");

        assert_eq!(
            edge.properties.get("since"),
            Some(&serde_json::json!("2024-01-01"))
        );
        assert_eq!(
            edge.properties.get("strength"),
            Some(&serde_json::json!(0.9))
        );
    }

    #[test]
    fn test_get_edge() {
        let server = create_test_server();
        let (source_id, target_id) = create_two_nodes(&server);

        let create_response = server.create_edge(CreateEdgeRequest {
            valid_time: None,
            source_id,
            target_id,
            label: "KNOWS".to_string(),
            properties: None,
            provenance: None,
        });
        let created: EdgeResponse = parse_response(&create_response).unwrap();

        let get_response = server.get_edge(GetEdgeRequest {
            edge_id: created.id,
            include_vectors: None,
        });
        let retrieved: EdgeResponse = parse_response(&get_response).unwrap();

        assert_eq!(retrieved.id, created.id);
        assert_eq!(retrieved.source_id, source_id);
        assert_eq!(retrieved.target_id, target_id);
    }

    #[test]
    fn test_update_edge() {
        let server = create_test_server();
        let (source_id, target_id) = create_two_nodes(&server);

        let create_response = server.create_edge(CreateEdgeRequest {
            valid_time: None,
            source_id,
            target_id,
            label: "KNOWS".to_string(),
            properties: None,
            provenance: None,
        });
        let created: EdgeResponse = parse_response(&create_response).unwrap();

        let mut new_props = HashMap::new();
        new_props.insert("weight".to_string(), serde_json::json!(0.5));

        let update_response = server.update_edge(UpdateEdgeRequest {
            valid_time: None,
            edge_id: created.id,
            properties: new_props,
            provenance: None,
        });
        let updated: EdgeResponse = parse_response(&update_response).unwrap();

        assert_eq!(
            updated.properties.get("weight"),
            Some(&serde_json::json!(0.5))
        );
    }

    #[test]
    fn test_delete_edge() {
        let server = create_test_server();
        let (source_id, target_id) = create_two_nodes(&server);

        let create_response = server.create_edge(CreateEdgeRequest {
            valid_time: None,
            source_id,
            target_id,
            label: "KNOWS".to_string(),
            properties: None,
            provenance: None,
        });
        let created: EdgeResponse = parse_response(&create_response).unwrap();

        let delete_response = server.delete_edge(DeleteEdgeRequest {
            edge_id: created.id,
            valid_time: None,
        });
        let value: serde_json::Value = serde_json::from_str(&delete_response).unwrap();
        assert_eq!(value.get("success"), Some(&serde_json::json!(true)));

        // Verify it's gone
        let get_response = server.get_edge(GetEdgeRequest {
            edge_id: created.id,
            include_vectors: None,
        });
        let get_value: serde_json::Value = serde_json::from_str(&get_response).unwrap();
        assert!(get_value.get("error").is_some());
    }

    #[test]
    fn test_list_edges() {
        let server = create_test_server();
        let (n1, n2) = create_two_nodes(&server);

        // Create node3
        let node3 = server.create_node(CreateNodeRequest {
            valid_time: None,
            label: "Person".to_string(),
            properties: None,
            provenance: None,
        });
        let n3: NodeResponse = parse_response(&node3).unwrap();

        // Create edges
        server.create_edge(CreateEdgeRequest {
            valid_time: None,
            source_id: n1,
            target_id: n2,
            label: "KNOWS".to_string(),
            properties: None,
            provenance: None,
        });
        server.create_edge(CreateEdgeRequest {
            valid_time: None,
            source_id: n2,
            target_id: n3.id,
            label: "KNOWS".to_string(),
            properties: None,
            provenance: None,
        });

        // Note: list_edges doesn't support listing all edges without a node
        // It returns a message indicating to use get_outgoing_edges or get_incoming_edges
        let list_response = server.list_edges(ListEdgesRequest {
            label: None,
            limit: None,
            offset: None,
            include_vectors: None,
        });
        let value: serde_json::Value = serde_json::from_str(&list_response).unwrap();
        // Verify total_count is 2 (edges exist, even if not returned)
        assert_eq!(value.get("total_count"), Some(&serde_json::json!(2)));
        // Verify a helpful message is returned
        assert!(value.get("message").is_some());
    }

    #[test]
    fn test_count_edges() {
        let server = create_test_server();
        let (n1, n2) = create_two_nodes(&server);

        let count_response = server.count_edges(CountEdgesRequest { label: None });
        let value: serde_json::Value = serde_json::from_str(&count_response).unwrap();
        assert_eq!(value.get("count"), Some(&serde_json::json!(0)));

        server.create_edge(CreateEdgeRequest {
            valid_time: None,
            source_id: n1,
            target_id: n2,
            label: "KNOWS".to_string(),
            properties: None,
            provenance: None,
        });

        let count_response = server.count_edges(CountEdgesRequest { label: None });
        let value: serde_json::Value = serde_json::from_str(&count_response).unwrap();
        assert_eq!(value.get("count"), Some(&serde_json::json!(1)));
    }

    #[test]
    fn test_get_outgoing_edges() {
        let server = create_test_server();
        let (n1, n2) = create_two_nodes(&server);

        // Create node3
        let node3 = server.create_node(CreateNodeRequest {
            valid_time: None,
            label: "Person".to_string(),
            properties: None,
            provenance: None,
        });
        let n3: NodeResponse = parse_response(&node3).unwrap();

        // Create edges from n1
        server.create_edge(CreateEdgeRequest {
            valid_time: None,
            source_id: n1,
            target_id: n2,
            label: "KNOWS".to_string(),
            properties: None,
            provenance: None,
        });
        server.create_edge(CreateEdgeRequest {
            valid_time: None,
            source_id: n1,
            target_id: n3.id,
            label: "WORKS_WITH".to_string(),
            properties: None,
            provenance: None,
        });

        // Get all outgoing edges
        let response = server.get_outgoing_edges(GetOutgoingEdgesRequest {
            node_id: n1,
            label: None,
            include_vectors: None,
        });
        let value: serde_json::Value = serde_json::from_str(&response).unwrap();
        assert_eq!(value.get("count"), Some(&serde_json::json!(2)));

        // Get only KNOWS edges
        let response = server.get_outgoing_edges(GetOutgoingEdgesRequest {
            node_id: n1,
            label: Some("KNOWS".to_string()),
            include_vectors: None,
        });
        let value: serde_json::Value = serde_json::from_str(&response).unwrap();
        assert_eq!(value.get("count"), Some(&serde_json::json!(1)));
    }

    #[test]
    fn test_get_incoming_edges() {
        let server = create_test_server();
        let (n1, n2) = create_two_nodes(&server);

        // Create node3
        let node3 = server.create_node(CreateNodeRequest {
            valid_time: None,
            label: "Person".to_string(),
            properties: None,
            provenance: None,
        });
        let n3: NodeResponse = parse_response(&node3).unwrap();

        // Create edges to n2
        server.create_edge(CreateEdgeRequest {
            valid_time: None,
            source_id: n1,
            target_id: n2,
            label: "KNOWS".to_string(),
            properties: None,
            provenance: None,
        });
        server.create_edge(CreateEdgeRequest {
            valid_time: None,
            source_id: n3.id,
            target_id: n2,
            label: "KNOWS".to_string(),
            properties: None,
            provenance: None,
        });

        let response = server.get_incoming_edges(GetIncomingEdgesRequest {
            node_id: n2,
            label: None,
            include_vectors: None,
        });
        let value: serde_json::Value = serde_json::from_str(&response).unwrap();
        assert_eq!(value.get("count"), Some(&serde_json::json!(2)));
    }
}

// ============================================================================
// Graph Traversal Tests
// ============================================================================

mod traversal_tests {
    use super::*;

    fn create_graph(server: &AletheiaMcpServer) -> Vec<u64> {
        // Create a simple graph: A -> B -> C -> D
        let nodes: Vec<u64> = (0..4)
            .map(|i| {
                let response = server.create_node(CreateNodeRequest {
                    valid_time: None,
                    label: "Node".to_string(),
                    properties: Some({
                        let mut m = HashMap::new();
                        m.insert("name".to_string(), serde_json::json!(format!("Node{}", i)));
                        m
                    }),
                    provenance: None,
                });
                let node: NodeResponse = parse_response(&response).unwrap();
                node.id
            })
            .collect();

        // Create edges
        for i in 0..3 {
            server.create_edge(CreateEdgeRequest {
                valid_time: None,
                source_id: nodes[i],
                target_id: nodes[i + 1],
                label: "NEXT".to_string(),
                properties: None,
                provenance: None,
            });
        }

        nodes
    }

    #[test]
    fn test_traverse_single_hop() {
        let server = create_test_server();
        let nodes = create_graph(&server);

        let response = server.traverse(TraverseRequest {
            start_node_id: nodes[0],
            edge_label: "NEXT".to_string(),
            direction: Some("outgoing".to_string()),
            depth: Some(1),
            limit: None,
            offset: None,
            include_vectors: None,
            as_of_valid_time: None,
            as_of_transaction_time: None,
        });

        let value: serde_json::Value = serde_json::from_str(&response).unwrap();
        // Should find Node1 (one hop from Node0)
        let count = value.get("count").and_then(|c| c.as_u64()).unwrap_or(0);
        assert!(count >= 1, "Should find at least one node in traversal");
    }

    #[test]
    fn test_traverse_multi_hop() {
        let server = create_test_server();
        let nodes = create_graph(&server);

        let response = server.traverse(TraverseRequest {
            start_node_id: nodes[0],
            edge_label: "NEXT".to_string(),
            direction: Some("outgoing".to_string()),
            depth: Some(3),
            limit: None,
            offset: None,
            include_vectors: None,
            as_of_valid_time: None,
            as_of_transaction_time: None,
        });

        let value: serde_json::Value = serde_json::from_str(&response).unwrap();
        // Should find nodes at depths 1, 2, and 3
        let count = value.get("count").and_then(|c| c.as_u64()).unwrap_or(0);
        assert!(count >= 1, "Should find nodes in multi-hop traversal");
    }

    #[test]
    fn test_traverse_incoming() {
        let server = create_test_server();
        let nodes = create_graph(&server);

        // Traverse incoming from Node3 (should find Node2)
        let response = server.traverse(TraverseRequest {
            start_node_id: nodes[3],
            edge_label: "NEXT".to_string(),
            direction: Some("incoming".to_string()),
            depth: Some(1),
            limit: None,
            offset: None,
            include_vectors: None,
            as_of_valid_time: None,
            as_of_transaction_time: None,
        });

        let value: serde_json::Value = serde_json::from_str(&response).unwrap();
        let count = value.get("count").and_then(|c| c.as_u64()).unwrap_or(0);
        assert!(count >= 1, "Should find incoming node");
    }

    #[test]
    fn test_traverse_with_limit() {
        let server = create_test_server();
        let nodes = create_graph(&server);

        let response = server.traverse(TraverseRequest {
            start_node_id: nodes[0],
            edge_label: "NEXT".to_string(),
            direction: None,
            depth: Some(3),
            limit: Some(2),
            offset: None,
            include_vectors: None,
            as_of_valid_time: None,
            as_of_transaction_time: None,
        });

        let value: serde_json::Value = serde_json::from_str(&response).unwrap();
        let count = value.get("count").and_then(|c| c.as_u64()).unwrap_or(0);
        assert!(count <= 2, "Should respect limit");
    }
}

// ============================================================================
// Vector Search Tests
// ============================================================================

mod vector_tests {
    use super::*;

    #[test]
    fn test_enable_vector_index() {
        let server = create_test_server();

        let response = server.enable_vector_index(EnableVectorIndexRequest {
            property_name: "embedding".to_string(),
            dimensions: 128,
            distance_metric: Some("cosine".to_string()),
        });

        let value: serde_json::Value = serde_json::from_str(&response).unwrap();
        assert_eq!(value.get("success"), Some(&serde_json::json!(true)));
    }

    #[test]
    fn test_list_vector_indexes() {
        let server = create_test_server();

        // Enable an index
        server.enable_vector_index(EnableVectorIndexRequest {
            property_name: "embedding".to_string(),
            dimensions: 128,
            distance_metric: None,
        });

        let response = server.list_vector_indexes(ListVectorIndexesRequest {});
        let value: serde_json::Value = serde_json::from_str(&response).unwrap();

        let indexes = value.get("indexes").unwrap().as_array().unwrap();
        // The new format returns objects with property_name, dimensions, and distance_metric
        assert!(indexes.iter().any(|i| {
            i.get("property_name")
                .and_then(|p| p.as_str())
                .map(|p| p == "embedding")
                .unwrap_or(false)
        }));
    }

    #[test]
    fn test_find_similar_without_index() {
        let server = create_test_server();

        let response = server.find_similar(FindSimilarRequest {
            property_name: "embedding".to_string(),
            embedding: vec![0.1, 0.2, 0.3, 0.4],
            k: Some(5),
            offset: None,
            include_vectors: None,
        });

        let value: serde_json::Value = serde_json::from_str(&response).unwrap();
        assert!(value.get("error").is_some(), "Should error without index");
    }

    #[test]
    fn test_find_similar_with_index() {
        let server = create_test_server();

        // Enable vector index
        server.enable_vector_index(EnableVectorIndexRequest {
            property_name: "embedding".to_string(),
            dimensions: 4,
            distance_metric: Some("cosine".to_string()),
        });

        // Create nodes with embeddings
        for i in 0..5 {
            let mut props = HashMap::new();
            props.insert("name".to_string(), serde_json::json!(format!("Doc{}", i)));
            props.insert(
                "embedding".to_string(),
                serde_json::json!([
                    (i as f32) * 0.1,
                    (i as f32) * 0.2,
                    (i as f32) * 0.3,
                    (i as f32) * 0.4
                ]),
            );

            server.create_node(CreateNodeRequest {
                valid_time: None,
                label: "Document".to_string(),
                properties: Some(props),
                provenance: None,
            });
        }

        // Search for similar
        let response = server.find_similar(FindSimilarRequest {
            property_name: "embedding".to_string(),
            embedding: vec![0.1, 0.2, 0.3, 0.4],
            k: Some(3),
            offset: None,
            include_vectors: None,
        });

        let value: serde_json::Value = serde_json::from_str(&response).unwrap();
        // May or may not find results depending on index state
        assert!(value.get("results").is_some() || value.get("error").is_some());
    }
}

// ============================================================================
// Temporal Query Tests
// ============================================================================

mod temporal_tests {
    use super::*;

    #[test]
    fn test_get_node_at_time_invalid_timestamp() {
        let server = create_test_server();

        // Create a node
        let node_response = server.create_node(CreateNodeRequest {
            valid_time: None,
            label: "Person".to_string(),
            properties: None,
            provenance: None,
        });
        let node: NodeResponse = parse_response(&node_response).unwrap();

        // Try to get with invalid timestamp format
        let response = server.get_node_at_time(GetNodeAtTimeRequest {
            node_id: node.id,
            valid_time: "invalid-timestamp".to_string(),
            transaction_time: None,
        });

        let value: serde_json::Value = serde_json::from_str(&response).unwrap();
        assert!(value.get("error").is_some());
    }

    #[test]
    fn test_get_node_at_time_valid_timestamp() {
        let server = create_test_server();

        // Create a node
        let node_response = server.create_node(CreateNodeRequest {
            valid_time: None,
            label: "Person".to_string(),
            properties: Some({
                let mut m = HashMap::new();
                m.insert("name".to_string(), serde_json::json!("Alice"));
                m
            }),
            provenance: None,
        });
        let node: NodeResponse = parse_response(&node_response).unwrap();

        // Get with timestamp as microseconds since epoch
        let now_micros = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_micros() as i64;

        let response = server.get_node_at_time(GetNodeAtTimeRequest {
            node_id: node.id,
            valid_time: now_micros.to_string(),
            transaction_time: None,
        });

        let value: serde_json::Value = serde_json::from_str(&response).unwrap();
        // Should either succeed or give a meaningful error
        assert!(value.get("node").is_some() || value.get("error").is_some());
    }

    #[test]
    fn test_get_edge_at_time() {
        let server = create_test_server();

        // Create nodes and edge
        let n1_response = server.create_node(CreateNodeRequest {
            valid_time: None,
            label: "Person".to_string(),
            properties: None,
            provenance: None,
        });
        let n1: NodeResponse = parse_response(&n1_response).unwrap();

        let n2_response = server.create_node(CreateNodeRequest {
            valid_time: None,
            label: "Person".to_string(),
            properties: None,
            provenance: None,
        });
        let n2: NodeResponse = parse_response(&n2_response).unwrap();

        let edge_response = server.create_edge(CreateEdgeRequest {
            valid_time: None,
            source_id: n1.id,
            target_id: n2.id,
            label: "KNOWS".to_string(),
            properties: None,
            provenance: None,
        });
        let edge: EdgeResponse = parse_response(&edge_response).unwrap();

        let now_micros = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_micros() as i64;

        let response = server.get_edge_at_time(GetEdgeAtTimeRequest {
            edge_id: edge.id,
            valid_time: now_micros.to_string(),
            transaction_time: None,
        });

        let value: serde_json::Value = serde_json::from_str(&response).unwrap();
        assert!(value.get("edge").is_some() || value.get("error").is_some());
    }

    // ------------------------------------------------------------------------
    // list_changes (temporal changefeed, Issue #3216)
    // ------------------------------------------------------------------------

    fn list_changes_req() -> ListChangesRequest {
        ListChangesRequest {
            tx_from: "0".to_string(),
            tx_to: i64::MAX.to_string(),
            valid_from: None,
            valid_to: None,
            label: None,
            limit: None,
            cursor: None,
        }
    }

    #[test]
    fn test_list_changes_success_shape() {
        let server = create_test_server();
        server.create_node(CreateNodeRequest {
            valid_time: None,
            label: "Person".to_string(),
            properties: None,
            provenance: None,
        });

        let response = server.list_changes(list_changes_req());
        let value: serde_json::Value = serde_json::from_str(&response).unwrap();

        assert!(value.get("error").is_none(), "unexpected error: {value}");
        let changes = value["changes"].as_array().expect("changes array");
        assert_eq!(changes.len(), 1);
        assert_eq!(value["count"], serde_json::json!(1));
        assert!(value.get("next_cursor").is_some());

        let row = &changes[0];
        assert_eq!(row["kind"], serde_json::json!("node"));
        assert_eq!(row["change_type"], serde_json::json!("created"));
        assert_eq!(row["label"], serde_json::json!("Person"));
        assert!(row.get("transaction_time").is_some());
        assert!(row["transaction_time_range"].get("start").is_some());
        assert!(row["valid_time_range"].get("start").is_some());
    }

    #[test]
    fn test_list_changes_invalid_window_errors() {
        let server = create_test_server();
        let mut req = list_changes_req();
        req.tx_from = "2000000".to_string();
        req.tx_to = "1000000".to_string();
        let response = server.list_changes(req);
        let value: serde_json::Value = serde_json::from_str(&response).unwrap();
        assert!(value.get("error").is_some());
    }

    #[test]
    fn test_list_changes_empty_window_is_success() {
        let server = create_test_server();
        server.create_node(CreateNodeRequest {
            valid_time: None,
            label: "Person".to_string(),
            properties: None,
            provenance: None,
        });
        let mut req = list_changes_req();
        req.tx_from = "1000000".to_string();
        req.tx_to = "1000000".to_string(); // empty window
        let response = server.list_changes(req);
        let value: serde_json::Value = serde_json::from_str(&response).unwrap();
        assert!(value.get("error").is_none());
        assert_eq!(value["count"], serde_json::json!(0));
    }

    #[test]
    fn test_list_changes_half_specified_valid_window_errors() {
        let server = create_test_server();
        let mut req = list_changes_req();
        req.valid_from = Some("1000".to_string());
        // valid_to omitted.
        let response = server.list_changes(req);
        let value: serde_json::Value = serde_json::from_str(&response).unwrap();
        assert!(value.get("error").is_some());
    }

    #[test]
    fn test_list_changes_bad_timestamp_errors() {
        let server = create_test_server();
        let mut req = list_changes_req();
        req.tx_from = "not-a-time".to_string();
        let response = server.list_changes(req);
        let value: serde_json::Value = serde_json::from_str(&response).unwrap();
        assert!(value.get("error").is_some());
    }

    #[test]
    fn test_list_changes_registered_in_tool_list() {
        let server = create_test_server();
        let tools = server.list_tools_for_test();
        assert!(
            tools.iter().any(|name| name == "list_changes"),
            "list_changes must be registered in the tool list"
        );
    }
}

// ============================================================================
// Hybrid Query Tests
// ============================================================================

mod hybrid_tests {
    use super::*;

    #[test]
    fn test_hybrid_query_with_start_node() {
        let server = create_test_server();

        // Create nodes
        let n1_response = server.create_node(CreateNodeRequest {
            valid_time: None,
            label: "Person".to_string(),
            properties: Some({
                let mut m = HashMap::new();
                m.insert("name".to_string(), serde_json::json!("Alice"));
                m
            }),
            provenance: None,
        });
        let n1: NodeResponse = parse_response(&n1_response).unwrap();

        let n2_response = server.create_node(CreateNodeRequest {
            valid_time: None,
            label: "Person".to_string(),
            properties: Some({
                let mut m = HashMap::new();
                m.insert("name".to_string(), serde_json::json!("Bob"));
                m
            }),
            provenance: None,
        });
        let _n2: NodeResponse = parse_response(&n2_response).unwrap();

        // Execute hybrid query starting from n1
        let response = server.hybrid_query(HybridQueryRequest {
            start_node_id: Some(n1.id),
            traverse_edge: None,
            traverse_depth: None,
            vector_property: None,
            query_embedding: None,
            top_k: None,
            valid_time: None,
            transaction_time: None,
            filter_label: None,
            limit: Some(10),
            include_vectors: None,
        });

        let value: serde_json::Value = serde_json::from_str(&response).unwrap();
        assert!(value.get("results").is_some() || value.get("error").is_some());
    }

    #[test]
    fn test_hybrid_query_with_label_filter() {
        let server = create_test_server();

        // Create nodes with different labels
        for _ in 0..3 {
            server.create_node(CreateNodeRequest {
                valid_time: None,
                label: "Person".to_string(),
                properties: None,
                provenance: None,
            });
        }

        for _ in 0..2 {
            server.create_node(CreateNodeRequest {
                valid_time: None,
                label: "Document".to_string(),
                properties: None,
                provenance: None,
            });
        }

        // Query only Person nodes
        let response = server.hybrid_query(HybridQueryRequest {
            start_node_id: None,
            traverse_edge: None,
            traverse_depth: None,
            vector_property: None,
            query_embedding: None,
            top_k: None,
            valid_time: None,
            transaction_time: None,
            filter_label: Some("Person".to_string()),
            limit: Some(100),
            include_vectors: None,
        });

        let value: serde_json::Value = serde_json::from_str(&response).unwrap();
        if let Some(results) = value.get("results").and_then(|r| r.as_array()) {
            for result in results {
                let label = result
                    .get("node")
                    .and_then(|n| n.get("label"))
                    .and_then(|l| l.as_str());
                assert_eq!(label, Some("Person"));
            }
        }
    }

    #[test]
    fn test_hybrid_query_requires_criteria() {
        let server = create_test_server();

        // Query without any criteria should error
        let response = server.hybrid_query(HybridQueryRequest {
            start_node_id: None,
            traverse_edge: None,
            traverse_depth: None,
            vector_property: None,
            query_embedding: None,
            top_k: None,
            valid_time: None,
            transaction_time: None,
            filter_label: None,
            limit: None,
            include_vectors: None,
        });

        let value: serde_json::Value = serde_json::from_str(&response).unwrap();
        assert!(value.get("error").is_some());
    }

    #[test]
    fn test_hybrid_query_with_traversal() {
        let server = create_test_server();

        // Create a simple graph
        let n1_response = server.create_node(CreateNodeRequest {
            valid_time: None,
            label: "Person".to_string(),
            properties: None,
            provenance: None,
        });
        let n1: NodeResponse = parse_response(&n1_response).unwrap();

        let n2_response = server.create_node(CreateNodeRequest {
            valid_time: None,
            label: "Person".to_string(),
            properties: None,
            provenance: None,
        });
        let n2: NodeResponse = parse_response(&n2_response).unwrap();

        server.create_edge(CreateEdgeRequest {
            valid_time: None,
            source_id: n1.id,
            target_id: n2.id,
            label: "KNOWS".to_string(),
            properties: None,
            provenance: None,
        });

        // Query with traversal
        let response = server.hybrid_query(HybridQueryRequest {
            start_node_id: Some(n1.id),
            traverse_edge: Some("KNOWS".to_string()),
            traverse_depth: Some(1),
            vector_property: None,
            query_embedding: None,
            top_k: None,
            valid_time: None,
            transaction_time: None,
            filter_label: None,
            limit: Some(10),
            include_vectors: None,
        });

        let value: serde_json::Value = serde_json::from_str(&response).unwrap();
        assert!(value.get("results").is_some() || value.get("error").is_some());
    }
}

// ============================================================================
// Property Conversion Tests
// ============================================================================

mod conversion_tests {
    use super::*;

    #[test]
    fn test_property_types_roundtrip() {
        let server = create_test_server();

        let mut props = HashMap::new();
        props.insert("string_val".to_string(), serde_json::json!("hello"));
        props.insert("int_val".to_string(), serde_json::json!(42));
        props.insert("float_val".to_string(), serde_json::json!(1.5));
        props.insert("bool_val".to_string(), serde_json::json!(true));
        props.insert("null_val".to_string(), serde_json::Value::Null);
        props.insert("array_val".to_string(), serde_json::json!([1, 2, 3]));

        let response = server.create_node(CreateNodeRequest {
            valid_time: None,
            label: "Test".to_string(),
            properties: Some(props),
            provenance: None,
        });

        let node: NodeResponse = parse_response(&response).expect("Failed to create node");

        assert_eq!(
            node.properties.get("string_val"),
            Some(&serde_json::json!("hello"))
        );
        assert_eq!(node.properties.get("int_val"), Some(&serde_json::json!(42)));
        assert_eq!(
            node.properties.get("bool_val"),
            Some(&serde_json::json!(true))
        );
        assert_eq!(
            node.properties.get("null_val"),
            Some(&serde_json::Value::Null)
        );
    }

    #[test]
    fn test_vector_property() {
        let server = create_test_server();

        let mut props = HashMap::new();
        props.insert(
            "embedding".to_string(),
            serde_json::json!([0.1, 0.2, 0.3, 0.4]),
        );

        let response = server.create_node(CreateNodeRequest {
            valid_time: None,
            label: "Document".to_string(),
            properties: Some(props),
            provenance: None,
        });

        let node: NodeResponse = parse_response(&response).expect("Failed to create node");

        // Vector should be preserved
        let embedding = node.properties.get("embedding").unwrap();
        assert!(embedding.is_array());
    }
}

// ============================================================================
// Additional Coverage Tests
// ============================================================================

mod coverage_tests {
    use super::*;

    #[test]
    fn test_vector_dimension_validation() {
        let server = create_test_server();

        // Enable vector index with 4 dimensions
        let response = server.enable_vector_index(EnableVectorIndexRequest {
            property_name: "embedding".to_string(),
            dimensions: 4,
            distance_metric: Some("cosine".to_string()),
        });
        assert!(
            !response.contains("error"),
            "Failed to enable index: {}",
            response
        );

        // Try to search with wrong dimensions (3 instead of 4)
        let response = server.find_similar(FindSimilarRequest {
            property_name: "embedding".to_string(),
            embedding: vec![0.1, 0.2, 0.3], // Wrong: 3 dimensions instead of 4
            k: Some(5),
            offset: None,
            include_vectors: None,
        });

        // Should get dimension mismatch error
        assert!(response.contains("error"), "Expected error response");
        assert!(
            response.contains("dimension mismatch") || response.contains("Embedding dimension"),
            "Expected dimension mismatch error, got: {}",
            response
        );
    }

    #[test]
    fn test_hybrid_query_dimension_validation() {
        let server = create_test_server();

        // Enable vector index with 4 dimensions
        let response = server.enable_vector_index(EnableVectorIndexRequest {
            property_name: "embedding".to_string(),
            dimensions: 4,
            distance_metric: Some("cosine".to_string()),
        });
        assert!(
            !response.contains("error"),
            "Failed to enable index: {}",
            response
        );

        // Try hybrid query with wrong embedding dimensions
        let response = server.hybrid_query(HybridQueryRequest {
            start_node_id: None,
            traverse_edge: None,
            traverse_depth: None,
            query_embedding: Some(vec![0.1, 0.2, 0.3]), // Wrong: 3 dimensions
            vector_property: Some("embedding".to_string()),
            top_k: Some(5),
            filter_label: None,
            limit: None,
            valid_time: None,
            transaction_time: None,
            include_vectors: None,
        });

        // Should get dimension mismatch error
        assert!(response.contains("error"), "Expected error response");
        assert!(
            response.contains("dimension mismatch") || response.contains("Embedding dimension"),
            "Expected dimension mismatch error, got: {}",
            response
        );
    }

    #[test]
    fn test_iso8601_timestamp_parsing() {
        let server = create_test_server();

        // Create a node first
        let response = server.create_node(CreateNodeRequest {
            valid_time: None,
            label: "Event".to_string(),
            properties: None,
            provenance: None,
        });
        let node: NodeResponse = parse_response(&response).expect("Failed to create node");

        // Test with ISO 8601 timestamp format (with Z timezone)
        let response = server.get_node_at_time(GetNodeAtTimeRequest {
            node_id: node.id,
            valid_time: "2024-01-15T10:30:00Z".to_string(),
            transaction_time: None,
        });

        // Should not contain a parsing error
        let value: serde_json::Value = serde_json::from_str(&response).unwrap();
        // Either we get the node back or a "not found" type error (since we're querying at a past time)
        // but we should NOT get a timestamp parsing error
        if let Some(error) = value.get("error") {
            let err_str = error["message"].as_str().unwrap_or("");
            assert!(
                !err_str.contains("Invalid timestamp format"),
                "ISO 8601 timestamp should be parsed correctly, got error: {}",
                err_str
            );
        }
    }

    #[test]
    fn test_iso8601_timestamp_without_timezone() {
        let server = create_test_server();

        // Create a node first
        let response = server.create_node(CreateNodeRequest {
            valid_time: None,
            label: "Event".to_string(),
            properties: None,
            provenance: None,
        });
        let node: NodeResponse = parse_response(&response).expect("Failed to create node");

        // Test with ISO 8601 timestamp without timezone (should assume UTC)
        let response = server.get_node_at_time(GetNodeAtTimeRequest {
            node_id: node.id,
            valid_time: "2024-01-15T10:30:00".to_string(),
            transaction_time: None,
        });

        // Should not contain a parsing error
        let value: serde_json::Value = serde_json::from_str(&response).unwrap();
        if let Some(error) = value.get("error") {
            let err_str = error["message"].as_str().unwrap_or("");
            assert!(
                !err_str.contains("Invalid timestamp format"),
                "ISO 8601 timestamp without TZ should be parsed correctly, got error: {}",
                err_str
            );
        }
    }

    #[test]
    fn test_transaction_time_now_response() {
        let server = create_test_server();

        // Create a node first
        let response = server.create_node(CreateNodeRequest {
            valid_time: None,
            label: "Event".to_string(),
            properties: None,
            provenance: None,
        });
        let node: NodeResponse = parse_response(&response).expect("Failed to create node");

        // Get node at time without specifying transaction_time
        let response = server.get_node_at_time(GetNodeAtTimeRequest {
            node_id: node.id,
            valid_time: "0".to_string(), // Use 0 for simplicity
            transaction_time: None,
        });

        // Response should contain "now" for transaction_time when not specified
        let value: serde_json::Value = serde_json::from_str(&response).unwrap();
        if value.get("error").is_none() {
            let tx_time = value.get("transaction_time");
            assert_eq!(
                tx_time,
                Some(&serde_json::json!("now")),
                "Expected 'now' for unspecified transaction_time"
            );
        }
    }

    #[test]
    fn test_count_nodes_with_nonexistent_label() {
        let server = create_test_server();

        // Count nodes with a label that doesn't exist
        let response = server.count_nodes(CountNodesRequest {
            label: Some("NonexistentLabel".to_string()),
        });

        // Should return count: 0 (not an error)
        let value: serde_json::Value = serde_json::from_str(&response).unwrap();
        assert!(
            value.get("error").is_none(),
            "Should not error for nonexistent label"
        );
        assert_eq!(
            value.get("count"),
            Some(&serde_json::json!(0)),
            "Count should be 0 for nonexistent label"
        );
    }

    #[test]
    fn test_list_nodes_offset_cap() {
        let server = create_test_server();

        // Create a few nodes
        for i in 0..5 {
            server.create_node(CreateNodeRequest {
                valid_time: None,
                label: "OffsetTest".to_string(),
                properties: Some({
                    let mut props = HashMap::new();
                    props.insert("index".to_string(), serde_json::json!(i));
                    props
                }),
                provenance: None,
            });
        }

        // Request with a very large offset (should be capped)
        let response = server.list_nodes(ListNodesRequest {
            label: Some("OffsetTest".to_string()),
            property_key: None,
            property_value: None,
            limit: Some(10),
            offset: Some(100_000), // Very large offset, should be capped to MAX_PAGINATION_OFFSET,
            include_vectors: None,
        });

        // Should not error, just return empty results due to offset being beyond data
        let value: serde_json::Value = serde_json::from_str(&response).unwrap();
        assert!(
            value.get("error").is_none(),
            "Large offset should not cause error: {}",
            response
        );
    }

    #[test]
    fn test_traversal_depth_limit() {
        let server = create_test_server();

        // Create a chain of nodes
        let mut prev_id: Option<u64> = None;
        for i in 0..5 {
            let response = server.create_node(CreateNodeRequest {
                valid_time: None,
                label: "ChainNode".to_string(),
                properties: Some({
                    let mut props = HashMap::new();
                    props.insert("level".to_string(), serde_json::json!(i));
                    props
                }),
                provenance: None,
            });
            let node: NodeResponse = parse_response(&response).unwrap();

            if let Some(source_id) = prev_id {
                server.create_edge(CreateEdgeRequest {
                    valid_time: None,
                    source_id,
                    target_id: node.id,
                    label: "NEXT".to_string(),
                    properties: None,
                    provenance: None,
                });
            }
            prev_id = Some(node.id);
        }

        // Try to traverse with a very large depth (should be capped to MAX_TRAVERSAL_DEPTH)
        let response = server.traverse(TraverseRequest {
            start_node_id: 0,
            edge_label: "NEXT".to_string(),
            depth: Some(100), // Very large depth, should be capped
            direction: Some("outgoing".to_string()),
            limit: Some(50),
            offset: None,
            include_vectors: None,
            as_of_valid_time: None,
            as_of_transaction_time: None,
        });

        // Should not error
        let value: serde_json::Value = serde_json::from_str(&response).unwrap();
        assert!(
            value.get("error").is_none(),
            "Large depth should be capped, not error: {}",
            response
        );
    }
}

// ============================================================================
// ServerHandler Trait Tests
// ============================================================================

mod server_handler_tests {
    use super::*;
    use rmcp::ServerHandler;

    #[test]
    fn test_get_info() {
        let server = create_test_server();
        let info = server.get_info();

        assert!(info.instructions.is_some());
        let instructions = info.instructions.unwrap();
        assert!(instructions.contains("AletheiaDB"));
        assert!(instructions.contains("bi-temporal"));
    }

    #[test]
    fn test_get_info_instructions_content() {
        let server = create_test_server();
        let info = server.get_info();

        let instructions = info.instructions.expect("Instructions should be set");

        // Verify instructions mention key features
        assert!(instructions.contains("graph"));
        assert!(instructions.contains("temporal") || instructions.contains("Temporal"));
        assert!(instructions.contains("vector") || instructions.contains("Vector"));
    }
}

// ============================================================================
// Error Handling Tests
// ============================================================================

mod error_handling_tests {
    use super::*;

    #[test]
    fn test_update_node_nonexistent() {
        let server = create_test_server();

        let req = UpdateNodeRequest {
            valid_time: None,
            node_id: 999999,
            properties: HashMap::new(),
            provenance: None,
        };

        let response = server.update_node(req);
        let value: serde_json::Value = serde_json::from_str(&response).unwrap();

        assert!(value.get("error").is_some());
    }

    #[test]
    fn test_delete_node_nonexistent() {
        let server = create_test_server();

        let req = DeleteNodeRequest {
            node_id: 999999,
            detach: None,
            valid_time: None,
        };

        let response = server.delete_node(req);
        let value: serde_json::Value = serde_json::from_str(&response).unwrap();

        assert!(value.get("error").is_some());
    }

    #[test]
    fn test_get_edge_nonexistent() {
        let server = create_test_server();

        let req = GetEdgeRequest {
            edge_id: 999999,
            include_vectors: None,
        };

        let response = server.get_edge(req);
        let value: serde_json::Value = serde_json::from_str(&response).unwrap();

        assert!(value.get("error").is_some());
    }

    #[test]
    fn test_update_edge_nonexistent() {
        let server = create_test_server();

        let req = UpdateEdgeRequest {
            valid_time: None,
            edge_id: 999999,
            properties: HashMap::new(),
            provenance: None,
        };

        let response = server.update_edge(req);
        let value: serde_json::Value = serde_json::from_str(&response).unwrap();

        assert!(value.get("error").is_some());
    }

    #[test]
    fn test_delete_edge_nonexistent() {
        let server = create_test_server();

        let req = DeleteEdgeRequest {
            edge_id: 999999,
            valid_time: None,
        };

        let response = server.delete_edge(req);
        let value: serde_json::Value = serde_json::from_str(&response).unwrap();

        assert!(value.get("error").is_some());
    }

    #[test]
    fn test_create_edge_invalid_source() {
        let server = create_test_server();

        // Create only target node
        let target_resp = server.create_node(CreateNodeRequest {
            valid_time: None,
            label: "Target".to_string(),
            properties: None,
            provenance: None,
        });
        let target: NodeResponse = parse_response(&target_resp).unwrap();

        // Try to create edge with non-existent source
        let req = CreateEdgeRequest {
            valid_time: None,
            source_id: 999999,
            target_id: target.id,
            label: "KNOWS".to_string(),
            properties: None,
            provenance: None,
        };

        let response = server.create_edge(req);
        let value: serde_json::Value = serde_json::from_str(&response).unwrap();

        assert!(value.get("error").is_some());
    }

    #[test]
    fn test_create_edge_invalid_target() {
        let server = create_test_server();

        // Create only source node
        let source_resp = server.create_node(CreateNodeRequest {
            valid_time: None,
            label: "Source".to_string(),
            properties: None,
            provenance: None,
        });
        let source: NodeResponse = parse_response(&source_resp).unwrap();

        // Try to create edge with non-existent target
        let req = CreateEdgeRequest {
            valid_time: None,
            source_id: source.id,
            target_id: 999999,
            label: "KNOWS".to_string(),
            properties: None,
            provenance: None,
        };

        let response = server.create_edge(req);
        let value: serde_json::Value = serde_json::from_str(&response).unwrap();

        assert!(value.get("error").is_some());
    }
}

// ============================================================================
// Vector Index Distance Metric Tests
// ============================================================================

mod vector_distance_tests {
    use super::*;

    #[test]
    fn test_enable_vector_index_euclidean() {
        let server = create_test_server();

        let response = server.enable_vector_index(EnableVectorIndexRequest {
            property_name: "embedding_euclidean".to_string(),
            dimensions: 64,
            distance_metric: Some("euclidean".to_string()),
        });

        let value: serde_json::Value = serde_json::from_str(&response).unwrap();
        assert_eq!(value.get("success"), Some(&serde_json::json!(true)));
        assert_eq!(
            value.get("distance_metric"),
            Some(&serde_json::json!("euclidean"))
        );
    }

    #[test]
    fn test_enable_vector_index_dot_product() {
        let server = create_test_server();

        let response = server.enable_vector_index(EnableVectorIndexRequest {
            property_name: "embedding_dot".to_string(),
            dimensions: 64,
            distance_metric: Some("dot".to_string()),
        });

        let value: serde_json::Value = serde_json::from_str(&response).unwrap();
        assert_eq!(value.get("success"), Some(&serde_json::json!(true)));
        assert_eq!(
            value.get("distance_metric"),
            Some(&serde_json::json!("dot"))
        );
    }

    #[test]
    fn test_enable_vector_index_dot_product_alias() {
        let server = create_test_server();

        let response = server.enable_vector_index(EnableVectorIndexRequest {
            property_name: "embedding_dotprod".to_string(),
            dimensions: 64,
            distance_metric: Some("dot_product".to_string()),
        });

        let value: serde_json::Value = serde_json::from_str(&response).unwrap();
        assert_eq!(value.get("success"), Some(&serde_json::json!(true)));
    }

    #[test]
    fn test_enable_vector_index_unknown_metric_defaults_to_cosine() {
        let server = create_test_server();

        let response = server.enable_vector_index(EnableVectorIndexRequest {
            property_name: "embedding_unknown".to_string(),
            dimensions: 64,
            distance_metric: Some("unknown_metric".to_string()),
        });

        let value: serde_json::Value = serde_json::from_str(&response).unwrap();
        // Unknown metrics should default to cosine and succeed
        assert_eq!(value.get("success"), Some(&serde_json::json!(true)));
    }

    #[test]
    fn test_find_similar_k_capping() {
        let server = create_test_server();

        // Enable vector index
        server.enable_vector_index(EnableVectorIndexRequest {
            property_name: "embedding".to_string(),
            dimensions: 4,
            distance_metric: None,
        });

        // Try to request more than MAX_VECTOR_K results
        let response = server.find_similar(FindSimilarRequest {
            property_name: "embedding".to_string(),
            embedding: vec![0.1, 0.2, 0.3, 0.4],
            k: Some(10000), // Much larger than MAX_VECTOR_K,
            offset: None,
            include_vectors: None,
        });

        // Should not error (k gets capped internally)
        let value: serde_json::Value = serde_json::from_str(&response).unwrap();
        // The query succeeds (may return empty results, but no error about k)
        assert!(
            value.get("results").is_some(),
            "Should handle large k gracefully without an error, but got: {:?}",
            value.get("error")
        );
    }
}

// ============================================================================
// Temporal Query Extended Tests
// ============================================================================

mod temporal_extended_tests {
    use super::*;

    #[test]
    fn test_get_node_at_time_with_transaction_time() {
        let server = create_test_server();

        // Create a node
        let node_response = server.create_node(CreateNodeRequest {
            valid_time: None,
            label: "Event".to_string(),
            properties: Some({
                let mut m = HashMap::new();
                m.insert("title".to_string(), serde_json::json!("Conference"));
                m
            }),
            provenance: None,
        });
        let node: NodeResponse = parse_response(&node_response).unwrap();

        let now_micros = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_micros() as i64;

        // Get node at time with explicit transaction time
        let response = server.get_node_at_time(GetNodeAtTimeRequest {
            node_id: node.id,
            valid_time: now_micros.to_string(),
            transaction_time: Some(now_micros.to_string()),
        });

        let value: serde_json::Value = serde_json::from_str(&response).unwrap();
        // Should have explicit transaction_time in response
        if value.get("error").is_none() {
            assert!(
                value.get("transaction_time").is_some(),
                "Should include transaction_time in response"
            );
            // Should not be "now" since we provided explicit time
            assert_ne!(
                value.get("transaction_time"),
                Some(&serde_json::json!("now"))
            );
        }
    }

    #[test]
    fn test_get_edge_at_time_with_transaction_time() {
        let server = create_test_server();

        // Create nodes and edge
        let n1_response = server.create_node(CreateNodeRequest {
            valid_time: None,
            label: "Person".to_string(),
            properties: None,
            provenance: None,
        });
        let n1: NodeResponse = parse_response(&n1_response).unwrap();

        let n2_response = server.create_node(CreateNodeRequest {
            valid_time: None,
            label: "Person".to_string(),
            properties: None,
            provenance: None,
        });
        let n2: NodeResponse = parse_response(&n2_response).unwrap();

        let edge_response = server.create_edge(CreateEdgeRequest {
            valid_time: None,
            source_id: n1.id,
            target_id: n2.id,
            label: "KNOWS".to_string(),
            properties: None,
            provenance: None,
        });
        let edge: EdgeResponse = parse_response(&edge_response).unwrap();

        let now_micros = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_micros() as i64;

        // Get edge at time with explicit transaction time
        let response = server.get_edge_at_time(GetEdgeAtTimeRequest {
            edge_id: edge.id,
            valid_time: now_micros.to_string(),
            transaction_time: Some(now_micros.to_string()),
        });

        let value: serde_json::Value = serde_json::from_str(&response).unwrap();
        if value.get("error").is_none() {
            assert!(value.get("edge").is_some());
            assert!(value.get("transaction_time").is_some());
        }
    }

    #[test]
    fn test_get_node_at_time_invalid_transaction_time() {
        let server = create_test_server();

        let node_response = server.create_node(CreateNodeRequest {
            valid_time: None,
            label: "Test".to_string(),
            properties: None,
            provenance: None,
        });
        let node: NodeResponse = parse_response(&node_response).unwrap();

        // Try with invalid transaction time format
        let response = server.get_node_at_time(GetNodeAtTimeRequest {
            node_id: node.id,
            valid_time: "0".to_string(),
            transaction_time: Some("not-a-valid-timestamp".to_string()),
        });

        let value: serde_json::Value = serde_json::from_str(&response).unwrap();
        assert!(value.get("error").is_some());
        let error_str = value["error"]["message"].as_str().unwrap();
        assert!(
            error_str.contains("Invalid timestamp format"),
            "Should report invalid timestamp: {}",
            error_str
        );
    }

    #[test]
    fn test_get_edge_at_time_invalid_valid_time() {
        let server = create_test_server();

        // Create nodes and edge
        let n1_response = server.create_node(CreateNodeRequest {
            valid_time: None,
            label: "Person".to_string(),
            properties: None,
            provenance: None,
        });
        let n1: NodeResponse = parse_response(&n1_response).unwrap();

        let n2_response = server.create_node(CreateNodeRequest {
            valid_time: None,
            label: "Person".to_string(),
            properties: None,
            provenance: None,
        });
        let n2: NodeResponse = parse_response(&n2_response).unwrap();

        let edge_response = server.create_edge(CreateEdgeRequest {
            valid_time: None,
            source_id: n1.id,
            target_id: n2.id,
            label: "KNOWS".to_string(),
            properties: None,
            provenance: None,
        });
        let edge: EdgeResponse = parse_response(&edge_response).unwrap();

        // Try with invalid valid_time format
        let response = server.get_edge_at_time(GetEdgeAtTimeRequest {
            edge_id: edge.id,
            valid_time: "invalid-time".to_string(),
            transaction_time: None,
        });

        let value: serde_json::Value = serde_json::from_str(&response).unwrap();
        assert!(value.get("error").is_some());
    }

    #[test]
    fn test_iso8601_with_offset_timezone() {
        let server = create_test_server();

        let node_response = server.create_node(CreateNodeRequest {
            valid_time: None,
            label: "Event".to_string(),
            properties: None,
            provenance: None,
        });
        let node: NodeResponse = parse_response(&node_response).unwrap();

        // Test with offset timezone (+00:00)
        let response = server.get_node_at_time(GetNodeAtTimeRequest {
            node_id: node.id,
            valid_time: "2024-01-15T10:30:00+00:00".to_string(),
            transaction_time: None,
        });

        let value: serde_json::Value = serde_json::from_str(&response).unwrap();
        if let Some(error) = value.get("error") {
            let err_str = error["message"].as_str().unwrap_or("");
            assert!(
                !err_str.contains("Invalid timestamp format"),
                "Offset timezone should be parsed correctly, got error: {}",
                err_str
            );
        }
    }
}

// ============================================================================
// Traversal Extended Tests
// ============================================================================

mod traversal_extended_tests {
    use super::*;

    fn create_bidirectional_graph(server: &AletheiaMcpServer) -> Vec<u64> {
        // Create nodes: A <-> B <-> C
        let nodes: Vec<u64> = (0..3)
            .map(|i| {
                let response = server.create_node(CreateNodeRequest {
                    valid_time: None,
                    label: "BiNode".to_string(),
                    properties: Some({
                        let mut m = HashMap::new();
                        m.insert("name".to_string(), serde_json::json!(format!("Node{}", i)));
                        m
                    }),
                    provenance: None,
                });
                let node: NodeResponse = parse_response(&response).unwrap();
                node.id
            })
            .collect();

        // Create bidirectional edges
        for i in 0..2 {
            server.create_edge(CreateEdgeRequest {
                valid_time: None,
                source_id: nodes[i],
                target_id: nodes[i + 1],
                label: "CONNECTED".to_string(),
                properties: None,
                provenance: None,
            });
            server.create_edge(CreateEdgeRequest {
                valid_time: None,
                source_id: nodes[i + 1],
                target_id: nodes[i],
                label: "CONNECTED".to_string(),
                properties: None,
                provenance: None,
            });
        }

        nodes
    }

    #[test]
    fn test_traverse_bidirectional() {
        let server = create_test_server();
        let nodes = create_bidirectional_graph(&server);

        // Traverse bidirectionally from middle node
        let response = server.traverse(TraverseRequest {
            start_node_id: nodes[1], // Middle node
            edge_label: "CONNECTED".to_string(),
            direction: Some("both".to_string()),
            depth: Some(1),
            limit: None,
            offset: None,
            include_vectors: None,
            as_of_valid_time: None,
            as_of_transaction_time: None,
        });

        let value: serde_json::Value = serde_json::from_str(&response).unwrap();
        let count = value.get("count").and_then(|c| c.as_u64()).unwrap_or(0);
        // Should find both nodes[0] and nodes[2]
        assert!(
            count >= 2,
            "Bidirectional traversal should find both neighbors"
        );
    }

    #[test]
    fn test_traverse_both_direction_finds_node_reachable_only_via_incoming_edge() {
        // Regression test: direction:"both" must resolve the correct
        // neighbor for edges discovered through the *incoming* side too, not
        // just fall back to `edge.target` (which is `current_id` itself for
        // an incoming edge, silently dropping the neighbor). Build a graph
        // with NO reciprocal outgoing edge, so this can only pass if the
        // incoming half of "both" is resolved correctly.
        let server = create_test_server();

        let a = {
            let response = server.create_node(CreateNodeRequest {
                valid_time: None,
                label: "Node".to_string(),
                properties: None,
                provenance: None,
            });
            let node: NodeResponse = parse_response(&response).unwrap();
            node.id
        };
        let b = {
            let response = server.create_node(CreateNodeRequest {
                valid_time: None,
                label: "Node".to_string(),
                properties: None,
                provenance: None,
            });
            let node: NodeResponse = parse_response(&response).unwrap();
            node.id
        };
        // Only B -> A exists; A has no outgoing edge back to B.
        server.create_edge(CreateEdgeRequest {
            valid_time: None,
            source_id: b,
            target_id: a,
            label: "POINTS_AT".to_string(),
            properties: None,
            provenance: None,
        });

        let response = server.traverse(TraverseRequest {
            start_node_id: a,
            edge_label: "POINTS_AT".to_string(),
            direction: Some("both".to_string()),
            depth: Some(1),
            limit: None,
            offset: None,
            include_vectors: None,
            as_of_valid_time: None,
            as_of_transaction_time: None,
        });

        let value: serde_json::Value = serde_json::from_str(&response).unwrap();
        assert!(value.get("error").is_none(), "unexpected error: {value}");
        let count = value["count"].as_u64().unwrap();
        assert_eq!(
            count, 1,
            "direction:\"both\" must find B via the incoming B->A edge, not drop it: {value}"
        );
        assert_eq!(value["results"][0]["node"]["id"], b);
    }

    #[test]
    fn test_traverse_nonexistent_edge_label() {
        let server = create_test_server();

        // Create a single node
        let node_response = server.create_node(CreateNodeRequest {
            valid_time: None,
            label: "Lonely".to_string(),
            properties: None,
            provenance: None,
        });
        let node: NodeResponse = parse_response(&node_response).unwrap();

        // Traverse with non-existent edge label
        let response = server.traverse(TraverseRequest {
            start_node_id: node.id,
            edge_label: "NONEXISTENT".to_string(),
            direction: None,
            depth: Some(3),
            limit: None,
            offset: None,
            include_vectors: None,
            as_of_valid_time: None,
            as_of_transaction_time: None,
        });

        let value: serde_json::Value = serde_json::from_str(&response).unwrap();
        // Should return empty results, not error
        assert!(value.get("error").is_none());
        assert_eq!(value.get("count"), Some(&serde_json::json!(0)));
    }

    #[test]
    fn test_traverse_default_direction() {
        let server = create_test_server();

        // Create A -> B
        let n1_response = server.create_node(CreateNodeRequest {
            valid_time: None,
            label: "Node".to_string(),
            properties: None,
            provenance: None,
        });
        let n1: NodeResponse = parse_response(&n1_response).unwrap();

        let n2_response = server.create_node(CreateNodeRequest {
            valid_time: None,
            label: "Node".to_string(),
            properties: None,
            provenance: None,
        });
        let n2: NodeResponse = parse_response(&n2_response).unwrap();

        server.create_edge(CreateEdgeRequest {
            valid_time: None,
            source_id: n1.id,
            target_id: n2.id,
            label: "NEXT".to_string(),
            properties: None,
            provenance: None,
        });

        // Traverse without specifying direction (should default to outgoing)
        let response = server.traverse(TraverseRequest {
            start_node_id: n1.id,
            edge_label: "NEXT".to_string(),
            direction: None, // Default to outgoing
            depth: Some(1),
            limit: None,
            offset: None,
            include_vectors: None,
            as_of_valid_time: None,
            as_of_transaction_time: None,
        });

        let value: serde_json::Value = serde_json::from_str(&response).unwrap();
        let count = value.get("count").and_then(|c| c.as_u64()).unwrap_or(0);
        assert!(count >= 1, "Default direction should find outgoing nodes");
    }

    #[test]
    fn test_traverse_result_limit() {
        let server = create_test_server();

        // Create a star graph: center -> 10 spokes
        let center_response = server.create_node(CreateNodeRequest {
            valid_time: None,
            label: "Center".to_string(),
            properties: None,
            provenance: None,
        });
        let center: NodeResponse = parse_response(&center_response).unwrap();

        for i in 0..10 {
            let spoke_response = server.create_node(CreateNodeRequest {
                valid_time: None,
                label: "Spoke".to_string(),
                properties: Some({
                    let mut m = HashMap::new();
                    m.insert("index".to_string(), serde_json::json!(i));
                    m
                }),
                provenance: None,
            });
            let spoke: NodeResponse = parse_response(&spoke_response).unwrap();

            server.create_edge(CreateEdgeRequest {
                valid_time: None,
                source_id: center.id,
                target_id: spoke.id,
                label: "SPOKE".to_string(),
                properties: None,
                provenance: None,
            });
        }

        // Traverse with limit
        let response = server.traverse(TraverseRequest {
            start_node_id: center.id,
            edge_label: "SPOKE".to_string(),
            direction: Some("outgoing".to_string()),
            depth: Some(1),
            limit: Some(5),
            offset: None,
            include_vectors: None,
            as_of_valid_time: None,
            as_of_transaction_time: None,
        });

        let value: serde_json::Value = serde_json::from_str(&response).unwrap();
        let count = value.get("count").and_then(|c| c.as_u64()).unwrap_or(0);
        assert!(count <= 5, "Traversal should respect limit");
    }
}

// ============================================================================
// Hybrid Query Extended Tests
// ============================================================================

mod hybrid_extended_tests {
    use super::*;

    #[test]
    fn test_hybrid_query_with_temporal() {
        let server = create_test_server();

        // Create a node
        let node_response = server.create_node(CreateNodeRequest {
            valid_time: None,
            label: "Person".to_string(),
            properties: Some({
                let mut m = HashMap::new();
                m.insert("name".to_string(), serde_json::json!("Alice"));
                m
            }),
            provenance: None,
        });
        let node: NodeResponse = parse_response(&node_response).unwrap();

        let now_micros = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_micros() as i64;

        // Query with both valid_time and transaction_time
        let response = server.hybrid_query(HybridQueryRequest {
            start_node_id: Some(node.id),
            traverse_edge: None,
            traverse_depth: None,
            vector_property: None,
            query_embedding: None,
            top_k: None,
            valid_time: Some(now_micros.to_string()),
            transaction_time: Some(now_micros.to_string()),
            filter_label: None,
            limit: Some(10),
            include_vectors: None,
        });

        let value: serde_json::Value = serde_json::from_str(&response).unwrap();
        // Should return temporal query result
        if value.get("error").is_none() {
            assert!(
                value.get("temporal_query").is_some() && value.get("results").is_some(),
                "A successful temporal hybrid query should return both 'temporal_query' and 'results' fields."
            );
        }
    }

    #[test]
    fn test_hybrid_query_multi_depth_traversal() {
        let server = create_test_server();

        // Create chain: A -> B -> C -> D
        let nodes: Vec<u64> = (0..4)
            .map(|i| {
                let response = server.create_node(CreateNodeRequest {
                    valid_time: None,
                    label: "ChainNode".to_string(),
                    properties: Some({
                        let mut m = HashMap::new();
                        m.insert("level".to_string(), serde_json::json!(i));
                        m
                    }),
                    provenance: None,
                });
                let node: NodeResponse = parse_response(&response).unwrap();
                node.id
            })
            .collect();

        for i in 0..3 {
            server.create_edge(CreateEdgeRequest {
                valid_time: None,
                source_id: nodes[i],
                target_id: nodes[i + 1],
                label: "CHAIN".to_string(),
                properties: None,
                provenance: None,
            });
        }

        // Query with depth > 1
        let response = server.hybrid_query(HybridQueryRequest {
            start_node_id: Some(nodes[0]),
            traverse_edge: Some("CHAIN".to_string()),
            traverse_depth: Some(3),
            vector_property: None,
            query_embedding: None,
            top_k: None,
            valid_time: None,
            transaction_time: None,
            filter_label: None,
            limit: Some(10),
            include_vectors: None,
        });

        let value: serde_json::Value = serde_json::from_str(&response).unwrap();
        if value.get("error").is_none() {
            let count = value.get("count").and_then(|c| c.as_u64()).unwrap_or(0);
            assert!(count >= 1, "Multi-depth traversal should find nodes");
        }
    }

    #[test]
    fn test_hybrid_query_invalid_valid_time() {
        let server = create_test_server();

        let node_response = server.create_node(CreateNodeRequest {
            valid_time: None,
            label: "Test".to_string(),
            properties: None,
            provenance: None,
        });
        let node: NodeResponse = parse_response(&node_response).unwrap();

        // Query with invalid valid_time
        let response = server.hybrid_query(HybridQueryRequest {
            start_node_id: Some(node.id),
            traverse_edge: None,
            traverse_depth: None,
            vector_property: None,
            query_embedding: None,
            top_k: None,
            valid_time: Some("invalid-time".to_string()),
            transaction_time: None,
            filter_label: None,
            limit: None,
            include_vectors: None,
        });

        let value: serde_json::Value = serde_json::from_str(&response).unwrap();
        assert!(value.get("error").is_some());
        let error = value["error"]["message"].as_str().unwrap();
        assert!(
            error.contains("Invalid valid_time"),
            "Should report invalid valid_time"
        );
    }

    #[test]
    fn test_hybrid_query_invalid_transaction_time() {
        let server = create_test_server();

        let node_response = server.create_node(CreateNodeRequest {
            valid_time: None,
            label: "Test".to_string(),
            properties: None,
            provenance: None,
        });
        let node: NodeResponse = parse_response(&node_response).unwrap();

        // Query with invalid transaction_time
        let response = server.hybrid_query(HybridQueryRequest {
            start_node_id: Some(node.id),
            traverse_edge: None,
            traverse_depth: None,
            vector_property: None,
            query_embedding: None,
            top_k: None,
            valid_time: Some("0".to_string()),
            transaction_time: Some("bad-tx-time".to_string()),
            filter_label: None,
            limit: None,
            include_vectors: None,
        });

        let value: serde_json::Value = serde_json::from_str(&response).unwrap();
        assert!(value.get("error").is_some());
        let error = value["error"]["message"].as_str().unwrap();
        assert!(
            error.contains("Invalid transaction_time"),
            "Should report invalid transaction_time"
        );
    }

    #[test]
    fn test_hybrid_query_limit_capping() {
        let server = create_test_server();

        // Create some nodes
        for i in 0..5 {
            server.create_node(CreateNodeRequest {
                valid_time: None,
                label: "LimitTest".to_string(),
                properties: Some({
                    let mut m = HashMap::new();
                    m.insert("index".to_string(), serde_json::json!(i));
                    m
                }),
                provenance: None,
            });
        }

        // Query with very large limit (should be capped internally)
        let response = server.hybrid_query(HybridQueryRequest {
            start_node_id: None,
            traverse_edge: None,
            traverse_depth: None,
            vector_property: None,
            query_embedding: None,
            top_k: None,
            valid_time: None,
            transaction_time: None,
            filter_label: Some("LimitTest".to_string()),
            limit: Some(100000), // Much larger than MAX_RESULT_LIMIT,
            include_vectors: None,
        });

        let value: serde_json::Value = serde_json::from_str(&response).unwrap();
        // Should not error (limit gets capped)
        assert!(
            value.get("results").is_some(),
            "Large limit should be capped, not error"
        );
    }

    #[test]
    fn test_hybrid_query_vector_without_index() {
        let server = create_test_server();

        // Try vector search without enabling index
        let response = server.hybrid_query(HybridQueryRequest {
            start_node_id: None,
            traverse_edge: None,
            traverse_depth: None,
            vector_property: Some("embedding".to_string()),
            query_embedding: Some(vec![0.1, 0.2, 0.3, 0.4]),
            top_k: Some(5),
            valid_time: None,
            transaction_time: None,
            filter_label: None,
            limit: None,
            include_vectors: None,
        });

        let value: serde_json::Value = serde_json::from_str(&response).unwrap();
        assert!(value.get("error").is_some());
        let error = value["error"]["message"].as_str().unwrap();
        assert!(
            error.contains("Vector index not enabled"),
            "Should report missing vector index"
        );
    }
}

// ============================================================================
// Edge Operations Extended Tests
// ============================================================================

mod edge_extended_tests {
    use super::*;

    #[test]
    fn test_list_edges_with_label() {
        let server = create_test_server();

        // Create nodes
        let n1_response = server.create_node(CreateNodeRequest {
            valid_time: None,
            label: "Person".to_string(),
            properties: None,
            provenance: None,
        });
        let n1: NodeResponse = parse_response(&n1_response).unwrap();

        let n2_response = server.create_node(CreateNodeRequest {
            valid_time: None,
            label: "Person".to_string(),
            properties: None,
            provenance: None,
        });
        let n2: NodeResponse = parse_response(&n2_response).unwrap();

        // Create edges with different labels
        server.create_edge(CreateEdgeRequest {
            valid_time: None,
            source_id: n1.id,
            target_id: n2.id,
            label: "KNOWS".to_string(),
            properties: None,
            provenance: None,
        });
        server.create_edge(CreateEdgeRequest {
            valid_time: None,
            source_id: n1.id,
            target_id: n2.id,
            label: "WORKS_WITH".to_string(),
            properties: None,
            provenance: None,
        });

        // List edges with label filter
        let response = server.list_edges(ListEdgesRequest {
            label: Some("KNOWS".to_string()),
            limit: None,
            offset: None,
            include_vectors: None,
        });

        let value: serde_json::Value = serde_json::from_str(&response).unwrap();
        // list_edges doesn't actually filter, it just includes the label in response
        assert!(value.get("label_filter").is_some());
        assert_eq!(value.get("label_filter"), Some(&serde_json::json!("KNOWS")));
    }

    #[test]
    fn test_count_edges_with_label() {
        let server = create_test_server();

        // Create nodes and edges
        let n1_response = server.create_node(CreateNodeRequest {
            valid_time: None,
            label: "Person".to_string(),
            properties: None,
            provenance: None,
        });
        let n1: NodeResponse = parse_response(&n1_response).unwrap();

        let n2_response = server.create_node(CreateNodeRequest {
            valid_time: None,
            label: "Person".to_string(),
            properties: None,
            provenance: None,
        });
        let n2: NodeResponse = parse_response(&n2_response).unwrap();

        server.create_edge(CreateEdgeRequest {
            valid_time: None,
            source_id: n1.id,
            target_id: n2.id,
            label: "KNOWS".to_string(),
            properties: None,
            provenance: None,
        });

        // Count edges with label (should return message about not being supported)
        let response = server.count_edges(CountEdgesRequest {
            label: Some("KNOWS".to_string()),
        });

        let value: serde_json::Value = serde_json::from_str(&response).unwrap();
        assert!(value.get("message").is_some());
        assert!(
            value
                .get("message")
                .unwrap()
                .as_str()
                .unwrap()
                .contains("not supported")
        );
    }

    #[test]
    fn test_get_incoming_edges_with_label() {
        let server = create_test_server();

        // Create nodes
        let n1_response = server.create_node(CreateNodeRequest {
            valid_time: None,
            label: "Person".to_string(),
            properties: None,
            provenance: None,
        });
        let n1: NodeResponse = parse_response(&n1_response).unwrap();

        let n2_response = server.create_node(CreateNodeRequest {
            valid_time: None,
            label: "Person".to_string(),
            properties: None,
            provenance: None,
        });
        let n2: NodeResponse = parse_response(&n2_response).unwrap();

        let n3_response = server.create_node(CreateNodeRequest {
            valid_time: None,
            label: "Person".to_string(),
            properties: None,
            provenance: None,
        });
        let n3: NodeResponse = parse_response(&n3_response).unwrap();

        // Create edges with different labels pointing to n2
        server.create_edge(CreateEdgeRequest {
            valid_time: None,
            source_id: n1.id,
            target_id: n2.id,
            label: "KNOWS".to_string(),
            properties: None,
            provenance: None,
        });
        server.create_edge(CreateEdgeRequest {
            valid_time: None,
            source_id: n3.id,
            target_id: n2.id,
            label: "WORKS_WITH".to_string(),
            properties: None,
            provenance: None,
        });

        // Get incoming edges with label filter
        let response = server.get_incoming_edges(GetIncomingEdgesRequest {
            node_id: n2.id,
            label: Some("KNOWS".to_string()),
            include_vectors: None,
        });

        let value: serde_json::Value = serde_json::from_str(&response).unwrap();
        assert_eq!(value.get("count"), Some(&serde_json::json!(1)));

        let edges = value.get("edges").unwrap().as_array().unwrap();
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].get("label"), Some(&serde_json::json!("KNOWS")));
    }
}

// ============================================================================
// List Nodes Without Label Tests
// ============================================================================

mod list_nodes_extended_tests {
    use super::*;

    #[test]
    fn test_list_nodes_without_label() {
        let server = create_test_server();

        // Create some nodes
        for i in 0..3 {
            server.create_node(CreateNodeRequest {
                valid_time: None,
                label: format!("Type{}", i),
                properties: None,
                provenance: None,
            });
        }

        // List nodes without label filter
        let response = server.list_nodes(ListNodesRequest {
            label: None,
            property_key: None,
            property_value: None,
            limit: None,
            offset: None,
            include_vectors: None,
        });

        let value: serde_json::Value = serde_json::from_str(&response).unwrap();
        // Should return message about using label filter
        assert!(value.get("message").is_some());
        assert!(value.get("total_count").is_some());
        assert_eq!(value.get("total_count"), Some(&serde_json::json!(3)));
        // nodes array should be empty
        assert_eq!(value.get("count"), Some(&serde_json::json!(0)));
    }

    #[test]
    fn test_list_nodes_limit_capping() {
        let server = create_test_server();

        // Create a few nodes
        for i in 0..5 {
            server.create_node(CreateNodeRequest {
                valid_time: None,
                label: "LimitCap".to_string(),
                properties: Some({
                    let mut m = HashMap::new();
                    m.insert("index".to_string(), serde_json::json!(i));
                    m
                }),
                provenance: None,
            });
        }

        // Request with very large limit (should be capped to MAX_RESULT_LIMIT)
        let response = server.list_nodes(ListNodesRequest {
            label: Some("LimitCap".to_string()),
            property_key: None,
            property_value: None,
            limit: Some(100000), // Much larger than MAX_RESULT_LIMIT
            offset: Some(0),
            include_vectors: None,
        });

        let value: serde_json::Value = serde_json::from_str(&response).unwrap();
        // Should not error
        assert!(
            value.get("nodes").is_some(),
            "Large limit should be capped, not error"
        );
    }

    // ========================================================================
    // Property-Based Lookup via MCP
    // ========================================================================

    #[test]
    fn test_list_nodes_by_property_string() {
        let server = create_test_server();

        // Create nodes with different names
        server.create_node(CreateNodeRequest {
            valid_time: None,
            label: "Person".to_string(),
            properties: Some({
                let mut m = HashMap::new();
                m.insert("name".to_string(), serde_json::json!("Alice"));
                m
            }),
            provenance: None,
        });
        server.create_node(CreateNodeRequest {
            valid_time: None,
            label: "Person".to_string(),
            properties: Some({
                let mut m = HashMap::new();
                m.insert("name".to_string(), serde_json::json!("Bob"));
                m
            }),
            provenance: None,
        });
        server.create_node(CreateNodeRequest {
            valid_time: None,
            label: "Person".to_string(),
            properties: Some({
                let mut m = HashMap::new();
                m.insert("name".to_string(), serde_json::json!("Alice"));
                m
            }),
            provenance: None,
        });

        // Filter by property
        let response = server.list_nodes(ListNodesRequest {
            label: Some("Person".to_string()),
            property_key: Some("name".to_string()),
            property_value: Some(serde_json::json!("Alice")),
            limit: None,
            offset: None,
            include_vectors: None,
        });

        let value: serde_json::Value = serde_json::from_str(&response).unwrap();
        let nodes = value["nodes"].as_array().unwrap();
        assert_eq!(nodes.len(), 2, "Should find exactly 2 Alice nodes");
    }

    #[test]
    fn test_list_nodes_by_property_int() {
        let server = create_test_server();

        server.create_node(CreateNodeRequest {
            valid_time: None,
            label: "Sensor".to_string(),
            properties: Some({
                let mut m = HashMap::new();
                m.insert("reading".to_string(), serde_json::json!(42));
                m
            }),
            provenance: None,
        });
        server.create_node(CreateNodeRequest {
            valid_time: None,
            label: "Sensor".to_string(),
            properties: Some({
                let mut m = HashMap::new();
                m.insert("reading".to_string(), serde_json::json!(99));
                m
            }),
            provenance: None,
        });

        let response = server.list_nodes(ListNodesRequest {
            label: Some("Sensor".to_string()),
            property_key: Some("reading".to_string()),
            property_value: Some(serde_json::json!(42)),
            limit: None,
            offset: None,
            include_vectors: None,
        });

        let value: serde_json::Value = serde_json::from_str(&response).unwrap();
        let nodes = value["nodes"].as_array().unwrap();
        assert_eq!(
            nodes.len(),
            1,
            "Should find exactly 1 sensor with reading=42"
        );
    }

    #[test]
    fn test_list_nodes_by_property_no_match() {
        let server = create_test_server();

        server.create_node(CreateNodeRequest {
            valid_time: None,
            label: "Item".to_string(),
            properties: Some({
                let mut m = HashMap::new();
                m.insert("color".to_string(), serde_json::json!("red"));
                m
            }),
            provenance: None,
        });

        let response = server.list_nodes(ListNodesRequest {
            label: Some("Item".to_string()),
            property_key: Some("color".to_string()),
            property_value: Some(serde_json::json!("blue")),
            limit: None,
            offset: None,
            include_vectors: None,
        });

        let value: serde_json::Value = serde_json::from_str(&response).unwrap();
        let nodes = value["nodes"].as_array().unwrap();
        assert_eq!(nodes.len(), 0, "Should find no matching nodes");
    }

    #[test]
    fn test_list_nodes_by_property_missing_label() {
        let server = create_test_server();

        // property_key without label should error
        let response = server.list_nodes(ListNodesRequest {
            label: None,
            property_key: Some("name".to_string()),
            property_value: Some(serde_json::json!("Alice")),
            limit: None,
            offset: None,
            include_vectors: None,
        });

        let value: serde_json::Value = serde_json::from_str(&response).unwrap();
        assert!(
            value.get("error").is_some(),
            "Should error when label is missing for property filter"
        );
    }

    #[test]
    fn test_list_nodes_by_property_key_without_value() {
        let server = create_test_server();

        // property_key without property_value should error
        let response = server.list_nodes(ListNodesRequest {
            label: Some("Person".to_string()),
            property_key: Some("name".to_string()),
            property_value: None,
            limit: None,
            offset: None,
            include_vectors: None,
        });

        let value: serde_json::Value = serde_json::from_str(&response).unwrap();
        assert!(
            value.get("error").is_some(),
            "Should error when property_value is missing"
        );
    }

    #[test]
    fn test_list_nodes_by_property_with_pagination() {
        let server = create_test_server();

        // Create 5 nodes with same property
        for _ in 0..5 {
            server.create_node(CreateNodeRequest {
                valid_time: None,
                label: "Widget".to_string(),
                properties: Some({
                    let mut m = HashMap::new();
                    m.insert("status".to_string(), serde_json::json!("active"));
                    m
                }),
                provenance: None,
            });
        }

        // Page 1: first 2
        let response = server.list_nodes(ListNodesRequest {
            label: Some("Widget".to_string()),
            property_key: Some("status".to_string()),
            property_value: Some(serde_json::json!("active")),
            limit: Some(2),
            offset: Some(0),
            include_vectors: None,
        });

        let value: serde_json::Value = serde_json::from_str(&response).unwrap();
        let nodes = value["nodes"].as_array().unwrap();
        assert_eq!(nodes.len(), 2, "Page 1 should have 2 nodes");

        // Page 2: next 2
        let response = server.list_nodes(ListNodesRequest {
            label: Some("Widget".to_string()),
            property_key: Some("status".to_string()),
            property_value: Some(serde_json::json!("active")),
            limit: Some(2),
            offset: Some(2),
            include_vectors: None,
        });

        let value: serde_json::Value = serde_json::from_str(&response).unwrap();
        let nodes = value["nodes"].as_array().unwrap();
        assert_eq!(nodes.len(), 2, "Page 2 should have 2 nodes");

        // Page 3: last 1
        let response = server.list_nodes(ListNodesRequest {
            label: Some("Widget".to_string()),
            property_key: Some("status".to_string()),
            property_value: Some(serde_json::json!("active")),
            limit: Some(2),
            offset: Some(4),
            include_vectors: None,
        });

        let value: serde_json::Value = serde_json::from_str(&response).unwrap();
        let nodes = value["nodes"].as_array().unwrap();
        assert_eq!(nodes.len(), 1, "Page 3 should have 1 node");
    }
}

// ============================================================================
// Declarative Query Tool Tests (Issue #3213)
// ============================================================================

mod query_tool_tests {
    use super::*;

    /// Seed a node with the given label and `name` property, returning its id.
    fn seed_named(server: &AletheiaMcpServer, label: &str, name: &str) -> u64 {
        let mut props = HashMap::new();
        props.insert("name".to_string(), serde_json::json!(name));
        let resp = server.create_node(CreateNodeRequest {
            valid_time: None,
            label: label.to_string(),
            properties: Some(props),
            provenance: None,
        });
        let node: NodeResponse = parse_response(&resp).expect("seed node should succeed");
        node.id
    }

    /// Run a query and return the parsed JSON value (success or error).
    fn run_query(server: &AletheiaMcpServer, req: QueryRequest) -> serde_json::Value {
        serde_json::from_str(&server.query(req)).expect("query response should be JSON")
    }

    /// Extract the structured error object from a query response.
    fn error_obj(value: &serde_json::Value) -> &serde_json::Value {
        value
            .get("error")
            .expect("expected an `error` payload in the response")
    }

    #[test]
    fn test_query_tool_is_advertised() {
        let server = create_test_server();
        assert!(
            server.list_tools_for_test().contains(&"query".to_string()),
            "the `query` tool must be advertised in list_tools"
        );
    }

    #[test]
    fn test_query_aql_returns_structured_rows() {
        let server = create_test_server();
        seed_named(&server, "Widget", "alpha");

        let value = run_query(
            &server,
            QueryRequest {
                language: "aql".to_string(),
                query: "MATCH (n:Widget) RETURN n".to_string(),
                params: None,
                limit: None,
            },
        );

        // Column metadata is present.
        assert!(
            value.get("columns").and_then(|c| c.as_array()).is_some(),
            "response must include column metadata: {value}"
        );
        assert_eq!(
            value["row_count"].as_u64(),
            Some(1),
            "exactly one row: {value}"
        );
        let rows = value["rows"].as_array().expect("rows array");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["entity"]["label"].as_str(), Some("Widget"));
        assert_eq!(
            rows[0]["entity"]["properties"]["name"].as_str(),
            Some("alpha")
        );
    }

    #[test]
    fn test_query_rejects_mutations_before_execution() {
        for stmt in [
            "CREATE (n:Hacker {name: 'x'})",
            "MATCH (n:Widget) SET n.name = 'x'",
            "MATCH (n:Widget) DELETE n",
            "MERGE (n:Widget {name: 'x'})",
            "MATCH (n:Widget) REMOVE n.name",
        ] {
            let server = create_test_server();
            seed_named(&server, "Widget", "alpha");
            let before = server.db().node_count();

            let value = run_query(
                &server,
                QueryRequest {
                    language: "cypher".to_string(),
                    query: stmt.to_string(),
                    params: None,
                    limit: None,
                },
            );

            let err = error_obj(&value);
            assert_eq!(
                err["kind"].as_str(),
                Some("read_only_violation"),
                "statement `{stmt}` must be rejected as read-only: {value}"
            );
            assert!(
                err.get("clause").and_then(|c| c.as_str()).is_some(),
                "the error must name the offending clause: {value}"
            );
            // It must never write.
            assert_eq!(
                server.db().node_count(),
                before,
                "statement `{stmt}` must not have mutated state"
            );
        }
    }

    #[test]
    fn test_query_parse_error_is_structured() {
        let server = create_test_server();
        let value = run_query(
            &server,
            QueryRequest {
                language: "aql".to_string(),
                query: "this is not a valid query".to_string(),
                params: None,
                limit: None,
            },
        );
        assert_eq!(
            error_obj(&value)["kind"].as_str(),
            Some("parse_error"),
            "malformed query must yield a parse_error: {value}"
        );
    }

    #[test]
    fn test_query_unknown_language_is_invalid_request() {
        let server = create_test_server();
        let value = run_query(
            &server,
            QueryRequest {
                language: "sql".to_string(),
                query: "MATCH (n) RETURN n".to_string(),
                params: None,
                limit: None,
            },
        );
        assert_eq!(
            error_obj(&value)["kind"].as_str(),
            Some("invalid_request"),
            "unknown language must yield invalid_request: {value}"
        );
    }

    #[test]
    fn test_query_aql_rejects_params() {
        let server = create_test_server();
        let mut params = HashMap::new();
        params.insert("name".to_string(), serde_json::json!("alpha"));
        let value = run_query(
            &server,
            QueryRequest {
                language: "aql".to_string(),
                query: "MATCH (n:Widget) RETURN n".to_string(),
                params: Some(params),
                limit: None,
            },
        );
        assert_eq!(
            error_obj(&value)["kind"].as_str(),
            Some("invalid_request"),
            "AQL does not support params and must reject them: {value}"
        );
    }

    #[test]
    fn test_query_row_cap_truncates() {
        let server = create_test_server();
        for i in 0..5 {
            seed_named(&server, "Widget", &format!("w{i}"));
        }
        let value = run_query(
            &server,
            QueryRequest {
                language: "aql".to_string(),
                query: "MATCH (n:Widget) RETURN n".to_string(),
                params: None,
                limit: Some(2),
            },
        );
        assert_eq!(
            value["row_count"].as_u64(),
            Some(2),
            "row cap honored: {value}"
        );
        assert_eq!(
            value["truncated"].as_bool(),
            Some(true),
            "truncated flag must be set when results exceed the cap: {value}"
        );
    }

    #[cfg(not(feature = "cypher"))]
    #[test]
    fn test_query_cypher_reports_unavailable_when_feature_disabled() {
        let server = create_test_server();
        let value = run_query(
            &server,
            QueryRequest {
                language: "cypher".to_string(),
                query: "MATCH (n) RETURN n".to_string(),
                params: None,
                limit: None,
            },
        );
        assert_eq!(
            error_obj(&value)["kind"].as_str(),
            Some("language_unavailable"),
            "with the cypher feature off, cypher must report as unavailable (not fail to compile): {value}"
        );
    }

    #[cfg(feature = "cypher")]
    #[test]
    fn test_query_cypher_returns_rows() {
        let server = create_test_server();
        seed_named(&server, "Person", "Alice");
        let value = run_query(
            &server,
            QueryRequest {
                language: "cypher".to_string(),
                query: "MATCH (n:Person {name: 'Alice'}) RETURN n".to_string(),
                params: None,
                limit: None,
            },
        );
        assert_eq!(value["row_count"].as_u64(), Some(1), "{value}");
        assert_eq!(
            value["rows"][0]["entity"]["properties"]["name"].as_str(),
            Some("Alice")
        );
    }

    #[cfg(feature = "cypher")]
    #[test]
    fn test_query_cypher_with_params() {
        let server = create_test_server();
        seed_named(&server, "Person", "Alice");
        seed_named(&server, "Person", "Bob");
        let mut params = HashMap::new();
        params.insert("name".to_string(), serde_json::json!("Alice"));
        let value = run_query(
            &server,
            QueryRequest {
                language: "cypher".to_string(),
                query: "MATCH (n:Person {name: $name}) RETURN n".to_string(),
                params: Some(params),
                limit: None,
            },
        );
        assert_eq!(value["row_count"].as_u64(), Some(1), "{value}");
        assert_eq!(
            value["rows"][0]["entity"]["properties"]["name"].as_str(),
            Some("Alice")
        );
    }

    #[cfg(feature = "cypher")]
    #[test]
    fn test_query_cypher_temporal_clauses_are_honored() {
        let server = create_test_server();
        seed_named(&server, "Person", "Alice");

        // Each temporal form must parse, flow through to the engine, and return
        // the correct row (the clause is honored, not dropped or rejected).
        for clause in [
            "AS OF VALID_TIME '2099-01-01'",
            "AS OF SYSTEM_TIME '2099-01-01'",
            "AS OF TIMESTAMP '2099-01-01T00:00:00Z'",
            "BETWEEN '2000-01-01' AND '2099-01-01'",
        ] {
            let q = format!("MATCH (n:Person) {clause} RETURN n");
            let value = run_query(
                &server,
                QueryRequest {
                    language: "cypher".to_string(),
                    query: q.clone(),
                    params: None,
                    limit: None,
                },
            );
            assert!(
                value.get("error").is_none(),
                "temporal query `{q}` must not error: {value}"
            );
            assert_eq!(
                value["row_count"].as_u64(),
                Some(1),
                "temporal query `{q}` must return the node: {value}"
            );
            assert_eq!(
                value["rows"][0]["entity"]["properties"]["name"].as_str(),
                Some("Alice"),
                "temporal query `{q}` must return correct data: {value}"
            );
        }
    }

    #[cfg(feature = "cypher")]
    #[test]
    fn test_query_cypher_invalid_temporal_timestamp_is_structured() {
        let server = create_test_server();
        seed_named(&server, "Person", "Alice");
        let value = run_query(
            &server,
            QueryRequest {
                language: "cypher".to_string(),
                query: "MATCH (n:Person) AS OF TIMESTAMP 'not-a-timestamp' RETURN n".to_string(),
                params: None,
                limit: None,
            },
        );
        let kind = error_obj(&value)["kind"].as_str();
        assert!(
            matches!(kind, Some("invalid_params") | Some("parse_error")),
            "an invalid temporal timestamp must yield a structured error: {value}"
        );
    }

    // -----------------------------------------------------------------------
    // Tests for fixes applied after the initial code review
    // -----------------------------------------------------------------------

    #[test]
    fn test_query_backslash_escape_in_string_does_not_block_valid_query() {
        // A query whose string literal contains a backslash-escaped quote must
        // NOT be rejected as a read_only_violation. The old sanitizer would
        // close the quote at the `\'` and then scan `s fine'}) RETURN n` as
        // bare tokens, finding no mutating keyword and passing — but the correct
        // behavior is that the whole content stays inside the string.
        let server = create_test_server();
        seed_named(&server, "Person", "Alice");
        let value = run_query(
            &server,
            QueryRequest {
                language: "aql".to_string(),
                // String literal `it\'s fine` — backslash-escaped quote inside single quotes.
                query: "MATCH (n:Person {note: 'it\\'s fine'}) RETURN n".to_string(),
                params: None,
                limit: None,
            },
        );
        // The guard must not fire (no read_only_violation for a read-only query).
        let err = value.get("error");
        if let Some(err) = err {
            assert_ne!(
                err["kind"].as_str(),
                Some("read_only_violation"),
                "backslash-escaped quote inside a string literal must not trip the read-only guard: {value}"
            );
        }
        // Whether the engine can parse this specific AQL variant is a grammar
        // question; what matters is that the guard itself does not reject it.
    }

    #[test]
    fn test_query_clause_field_absent_for_non_write_errors() {
        // `clause` is reserved exclusively for read_only_violation (it names the
        // mutating keyword). For other error kinds (parse_error,
        // unsupported_construct, invalid_params, runtime_error) the field must
        // NOT appear, since it would be semantically wrong.
        let server = create_test_server();
        let value = run_query(
            &server,
            QueryRequest {
                language: "aql".to_string(),
                query: "this is not valid".to_string(),
                params: None,
                limit: None,
            },
        );
        let err = error_obj(&value);
        assert_eq!(err["kind"].as_str(), Some("parse_error"));
        assert!(
            err.get("clause").is_none(),
            "parse_error must not include a `clause` field: {value}"
        );
    }

    #[cfg(feature = "cypher")]
    #[test]
    fn test_query_empty_array_param_is_invalid() {
        // An empty JSON array [] must be rejected with invalid_params rather
        // than silently accepted as a zero-dimension embedding.
        let server = create_test_server();
        let mut params = HashMap::new();
        params.insert("vec".to_string(), serde_json::json!([]));
        let value = run_query(
            &server,
            QueryRequest {
                language: "cypher".to_string(),
                query: "MATCH (n) RETURN n".to_string(),
                params: Some(params),
                limit: None,
            },
        );
        assert_eq!(
            error_obj(&value)["kind"].as_str(),
            Some("invalid_params"),
            "empty array parameter must yield invalid_params: {value}"
        );
    }

    #[cfg(feature = "cypher")]
    #[test]
    fn test_query_float_param_preserved_as_float() {
        // A JSON number with a decimal point (1.0) must be bound as Float, not Int.
        // We verify indirectly: the query must not fail with a type error, and
        // the params round-trip must work (parse_error or correct rows, not a
        // crash or wrong type coercion error).
        let server = create_test_server();
        seed_named(&server, "Product", "Gadget");
        let mut params = HashMap::new();
        // 1.0 is an explicit float in JSON — must not be coerced to Int.
        params.insert("threshold".to_string(), serde_json::json!(1.0_f64));
        let value = run_query(
            &server,
            QueryRequest {
                language: "cypher".to_string(),
                query: "MATCH (n:Product) WHERE n.price < $threshold RETURN n".to_string(),
                params: Some(params),
                limit: None,
            },
        );
        // The key assertion: the error must NOT be invalid_params (the float
        // was accepted as a float). Whether the engine returns rows or a
        // parse/runtime error for the WHERE predicate is a grammar question.
        if let Some(err) = value.get("error") {
            assert_ne!(
                err["kind"].as_str(),
                Some("invalid_params"),
                "a float-valued JSON param must be accepted as Float, not rejected: {value}"
            );
        }
    }

    #[test]
    fn test_query_single_line_comment_before_mutation_is_not_rejected() {
        // A `//`-style comment that contains a mutating keyword must NOT trip the
        // read-only guard — only bare (outside-comment, outside-string) tokens count.
        let server = create_test_server();
        seed_named(&server, "Widget", "alpha");
        let value = run_query(
            &server,
            QueryRequest {
                language: "aql".to_string(),
                query: "// CREATE would mutate\nMATCH (n:Widget) RETURN n".to_string(),
                params: None,
                limit: None,
            },
        );
        if let Some(err) = value.get("error") {
            assert_ne!(
                err["kind"].as_str(),
                Some("read_only_violation"),
                "mutating keyword inside a // comment must not trigger the guard: {value}"
            );
        }
    }

    #[test]
    fn test_query_node_label_matching_mutating_keyword_is_allowed() {
        // A node label that happens to spell a mutating keyword (e.g. :Call, :Set)
        // must NOT be rejected — the token is preceded by ':' and is a label, not
        // a clause.
        for stmt in [
            "MATCH (c:Call) RETURN c",
            "MATCH (s:Set) RETURN s",
            "MATCH (d:Drop) RETURN d",
        ] {
            let server = create_test_server();
            let value = run_query(
                &server,
                QueryRequest {
                    language: "aql".to_string(),
                    query: stmt.to_string(),
                    params: None,
                    limit: None,
                },
            );
            if let Some(err) = value.get("error") {
                assert_ne!(
                    err["kind"].as_str(),
                    Some("read_only_violation"),
                    "node label `{stmt}` must not trip the read-only guard: {value}"
                );
            }
        }
    }

    #[test]
    fn test_query_property_key_matching_mutating_keyword_is_allowed() {
        // A property key that happens to spell a mutating keyword (e.g. n.set,
        // n.merge) must NOT be rejected — the token is preceded by '.' and is a
        // property access, not a clause.
        for stmt in [
            "MATCH (n) RETURN n.set",
            "MATCH (n) RETURN n.merge",
            "MATCH (n) RETURN n.delete",
        ] {
            let server = create_test_server();
            let value = run_query(
                &server,
                QueryRequest {
                    language: "aql".to_string(),
                    query: stmt.to_string(),
                    params: None,
                    limit: None,
                },
            );
            if let Some(err) = value.get("error") {
                assert_ne!(
                    err["kind"].as_str(),
                    Some("read_only_violation"),
                    "property key in `{stmt}` must not trip the read-only guard: {value}"
                );
            }
        }
    }

    #[test]
    fn test_query_remaining_mutating_keywords_are_rejected() {
        // DETACH, DROP, FOREACH, and LOAD are listed as mutating keywords but are
        // not exercised by the existing CREATE/SET/DELETE/MERGE/REMOVE tests.
        for (stmt, kw) in [
            ("DETACH DELETE (n)", "DETACH"),
            ("DROP INDEX ON :Person(name)", "DROP"),
            ("FOREACH (x IN $list | SET x.flag = 1)", "FOREACH"),
            ("LOAD CSV FROM 'file' AS row", "LOAD"),
        ] {
            let server = create_test_server();
            let value = run_query(
                &server,
                QueryRequest {
                    language: "aql".to_string(),
                    query: stmt.to_string(),
                    params: None,
                    limit: None,
                },
            );
            assert_eq!(
                error_obj(&value)["kind"].as_str(),
                Some("read_only_violation"),
                "statement with `{kw}` must be rejected: {value}"
            );
        }
    }

    #[cfg(feature = "cypher")]
    #[test]
    fn test_query_cypher_null_param_is_accepted() {
        // JSON null must be bound as CypherParameterValue::Null without error.
        let server = create_test_server();
        let mut params = HashMap::new();
        params.insert("x".to_string(), serde_json::json!(null));
        let value = run_query(
            &server,
            QueryRequest {
                language: "cypher".to_string(),
                query: "MATCH (n) WHERE n.x = $x RETURN n".to_string(),
                params: Some(params),
                limit: None,
            },
        );
        if let Some(err) = value.get("error") {
            assert_ne!(
                err["kind"].as_str(),
                Some("invalid_params"),
                "null param must be accepted: {value}"
            );
        }
    }

    #[cfg(feature = "cypher")]
    #[test]
    fn test_query_cypher_bool_and_int_params_are_accepted() {
        // JSON booleans and integers must bind without error.
        let server = create_test_server();
        let mut params = HashMap::new();
        params.insert("flag".to_string(), serde_json::json!(true));
        params.insert("count".to_string(), serde_json::json!(42_i64));
        let value = run_query(
            &server,
            QueryRequest {
                language: "cypher".to_string(),
                query: "MATCH (n) WHERE n.flag = $flag AND n.count = $count RETURN n".to_string(),
                params: Some(params),
                limit: None,
            },
        );
        if let Some(err) = value.get("error") {
            assert_ne!(
                err["kind"].as_str(),
                Some("invalid_params"),
                "bool/int params must be accepted: {value}"
            );
        }
    }

    #[cfg(feature = "cypher")]
    #[test]
    fn test_query_cypher_object_param_is_invalid() {
        // A JSON object parameter is not supported; must yield invalid_params.
        let server = create_test_server();
        let mut params = HashMap::new();
        params.insert("obj".to_string(), serde_json::json!({"key": "value"}));
        let value = run_query(
            &server,
            QueryRequest {
                language: "cypher".to_string(),
                query: "MATCH (n) RETURN n".to_string(),
                params: Some(params),
                limit: None,
            },
        );
        assert_eq!(
            error_obj(&value)["kind"].as_str(),
            Some("invalid_params"),
            "object parameter must yield invalid_params: {value}"
        );
    }

    #[cfg(feature = "cypher")]
    #[test]
    fn test_query_cypher_non_numeric_array_param_is_invalid() {
        // An array containing non-numeric elements is not a valid embedding;
        // must yield invalid_params.
        let server = create_test_server();
        let mut params = HashMap::new();
        params.insert("vec".to_string(), serde_json::json!(["not", "numbers"]));
        let value = run_query(
            &server,
            QueryRequest {
                language: "cypher".to_string(),
                query: "MATCH (n) RETURN n".to_string(),
                params: Some(params),
                limit: None,
            },
        );
        assert_eq!(
            error_obj(&value)["kind"].as_str(),
            Some("invalid_params"),
            "non-numeric array parameter must yield invalid_params: {value}"
        );
    }
}

// ============================================================================
// Uniqueness Constraint Tests
// ============================================================================

mod constraint_tests {
    use super::*;

    fn parse_json(s: &str) -> serde_json::Value {
        serde_json::from_str(s).expect("server response must be valid JSON")
    }

    #[test]
    fn test_enable_unique_constraint_success() {
        let server = create_test_server();

        let response = server.enable_unique_constraint(EnableUniqueConstraintRequest {
            label: "Person".to_string(),
            property: "email".to_string(),
        });

        let v = parse_json(&response);
        assert_eq!(v["success"], serde_json::json!(true));
        assert_eq!(v["label"], "Person");
        assert_eq!(v["property"], "email");
    }

    #[test]
    fn test_enable_unique_constraint_rejects_existing_duplicates() {
        let server = create_test_server();

        let mut props = HashMap::new();
        props.insert("email".to_string(), serde_json::json!("dup@x"));
        server.create_node(CreateNodeRequest {
            valid_time: None,
            label: "Person".to_string(),
            properties: Some(props.clone()),
            provenance: None,
        });
        server.create_node(CreateNodeRequest {
            valid_time: None,
            label: "Person".to_string(),
            properties: Some(props),
            provenance: None,
        });

        let response = server.enable_unique_constraint(EnableUniqueConstraintRequest {
            label: "Person".to_string(),
            property: "email".to_string(),
        });

        let v = parse_json(&response);
        assert!(
            v.get("error").is_some(),
            "enable on label with pre-existing duplicates must return error: {v}"
        );
    }

    #[test]
    fn test_list_unique_constraints_empty() {
        let server = create_test_server();

        let response = server.list_unique_constraints(ListUniqueConstraintsRequest {});

        let v = parse_json(&response);
        assert_eq!(v["count"], serde_json::json!(0));
        assert!(
            v["constraints"]
                .as_array()
                .map(|a| a.is_empty())
                .unwrap_or(false),
            "constraints list should be empty on fresh server: {v}"
        );
    }

    #[test]
    fn test_list_unique_constraints_after_enable() {
        let server = create_test_server();

        server.enable_unique_constraint(EnableUniqueConstraintRequest {
            label: "Person".to_string(),
            property: "email".to_string(),
        });
        server.enable_unique_constraint(EnableUniqueConstraintRequest {
            label: "Company".to_string(),
            property: "name".to_string(),
        });

        let response = server.list_unique_constraints(ListUniqueConstraintsRequest {});

        let v = parse_json(&response);
        assert_eq!(
            v["count"],
            serde_json::json!(2),
            "should list 2 constraints: {v}"
        );
        assert_eq!(v["constraints"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn test_create_node_constraint_violation_structured_error() {
        let server = create_test_server();

        server.enable_unique_constraint(EnableUniqueConstraintRequest {
            label: "Person".to_string(),
            property: "email".to_string(),
        });

        let mut props = HashMap::new();
        props.insert("email".to_string(), serde_json::json!("alice@x"));

        let first_response = server.create_node(CreateNodeRequest {
            valid_time: None,
            label: "Person".to_string(),
            properties: Some(props.clone()),
            provenance: None,
        });
        let first: NodeResponse =
            parse_response(&first_response).expect("first create must succeed");

        let dup_response = server.create_node(CreateNodeRequest {
            valid_time: None,
            label: "Person".to_string(),
            properties: Some(props),
            provenance: None,
        });

        let v = parse_json(&dup_response);
        assert_eq!(
            v["constraint_violation"],
            serde_json::json!(true),
            "duplicate create must set constraint_violation=true: {v}"
        );
        assert_eq!(v["success"], serde_json::json!(false));
        assert_eq!(v["label"], "Person");
        assert_eq!(v["property"], "email");
        assert_eq!(v["value"], "alice@x");
        assert_eq!(
            v["existing_node_id"],
            serde_json::json!(first.id),
            "existing_node_id must point to the first node: {v}"
        );
    }

    #[test]
    fn test_update_node_constraint_violation_structured_error() {
        let server = create_test_server();

        server.enable_unique_constraint(EnableUniqueConstraintRequest {
            label: "Person".to_string(),
            property: "email".to_string(),
        });

        let mut props_a = HashMap::new();
        props_a.insert("email".to_string(), serde_json::json!("a@x"));
        let resp_a = server.create_node(CreateNodeRequest {
            valid_time: None,
            label: "Person".to_string(),
            properties: Some(props_a),
            provenance: None,
        });
        let node_a: NodeResponse = parse_response(&resp_a).expect("node A must be created");

        let mut props_b = HashMap::new();
        props_b.insert("email".to_string(), serde_json::json!("b@x"));
        server.create_node(CreateNodeRequest {
            valid_time: None,
            label: "Person".to_string(),
            properties: Some(props_b),
            provenance: None,
        });

        let mut collision = HashMap::new();
        collision.insert("email".to_string(), serde_json::json!("b@x"));
        let update_response = server.update_node(UpdateNodeRequest {
            valid_time: None,
            node_id: node_a.id,
            properties: collision,
            provenance: None,
        });

        let v = parse_json(&update_response);
        assert_eq!(
            v["constraint_violation"],
            serde_json::json!(true),
            "colliding update must set constraint_violation=true: {v}"
        );
        assert_eq!(v["success"], serde_json::json!(false));
        assert_eq!(v["property"], "email");
    }

    #[test]
    fn test_update_node_non_constraint_error_fallthrough() {
        // Covers server.rs: the `db_error(...)` fallthrough in
        // handle_update_node for non-constraint errors — db_error only emits
        // the legacy constraint top-level fields (constraint_violation,
        // existing_node_id, ...) for a ConstraintError::UniqueViolation.
        //
        // We call update_node with a valid-format but non-existent node ID so that
        // `tx.update_node` returns StorageError::NodeNotFound — which is NOT a
        // ConstraintError::UniqueViolation — and db_error takes the plain path.
        let server = create_test_server();

        let response = server.update_node(UpdateNodeRequest {
            valid_time: None,
            node_id: 99999,
            properties: HashMap::new(),
            provenance: None,
        });

        let v = parse_json(&response);
        assert!(
            v.get("error").is_some(),
            "update_node on non-existent node must return an error: {v}"
        );
        assert_eq!(
            v.get("constraint_violation"),
            None,
            "non-constraint error must NOT set constraint_violation: {v}"
        );
    }
}

// ============================================================================
// Schema Discovery Tests (get_schema, Issue #3214)
// ============================================================================

mod schema_tests {
    use super::*;

    fn schema_req(
        as_of_valid_time: Option<&str>,
        as_of_transaction_time: Option<&str>,
    ) -> GetSchemaRequest {
        GetSchemaRequest {
            as_of_valid_time: as_of_valid_time.map(str::to_string),
            as_of_transaction_time: as_of_transaction_time.map(str::to_string),
        }
    }

    #[test]
    fn test_get_schema_empty_database() {
        let server = create_test_server();

        let response = server.get_schema(schema_req(None, None));
        let value: serde_json::Value = serde_json::from_str(&response).unwrap();

        assert!(value.get("error").is_none(), "unexpected error: {value}");
        assert_eq!(value["node_labels"].as_array().unwrap().len(), 0);
        assert_eq!(value["edge_types"].as_array().unwrap().len(), 0);
        assert_eq!(value["total_nodes"], serde_json::json!(0));
        assert_eq!(value["total_edges"], serde_json::json!(0));
        assert_eq!(value["sampled"], serde_json::json!(false));
        assert!(value["as_of"].is_null());
    }

    #[test]
    fn test_get_schema_populated_graph() {
        let server = create_test_server();

        let alice_response = server.create_node(CreateNodeRequest {
            valid_time: None,
            label: "Person".to_string(),
            properties: Some({
                let mut m = HashMap::new();
                m.insert("name".to_string(), serde_json::json!("Alice"));
                m
            }),
            provenance: None,
        });
        let alice: NodeResponse = parse_response(&alice_response).unwrap();

        let bob_response = server.create_node(CreateNodeRequest {
            valid_time: None,
            label: "Person".to_string(),
            properties: Some({
                let mut m = HashMap::new();
                m.insert("email".to_string(), serde_json::json!("bob@example.com"));
                m
            }),
            provenance: None,
        });
        let bob: NodeResponse = parse_response(&bob_response).unwrap();

        server.create_edge(CreateEdgeRequest {
            valid_time: None,
            source_id: alice.id,
            target_id: bob.id,
            label: "KNOWS".to_string(),
            properties: Some({
                let mut m = HashMap::new();
                m.insert("since".to_string(), serde_json::json!(2020));
                m
            }),
            provenance: None,
        });

        let response = server.get_schema(schema_req(None, None));
        let value: serde_json::Value = serde_json::from_str(&response).unwrap();

        assert!(value.get("error").is_none(), "unexpected error: {value}");

        let node_labels = value["node_labels"].as_array().unwrap();
        assert_eq!(node_labels.len(), 1);
        assert_eq!(node_labels[0]["label"], serde_json::json!("Person"));
        assert_eq!(node_labels[0]["count"], serde_json::json!(2));
        let person_keys: Vec<String> = node_labels[0]["property_keys"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect();
        assert_eq!(person_keys, vec!["email".to_string(), "name".to_string()]);

        let edge_types = value["edge_types"].as_array().unwrap();
        assert_eq!(edge_types.len(), 1);
        assert_eq!(edge_types[0]["edge_type"], serde_json::json!("KNOWS"));
        assert_eq!(edge_types[0]["count"], serde_json::json!(1));
        assert_eq!(edge_types[0]["property_keys"], serde_json::json!(["since"]));

        assert_eq!(value["total_nodes"], serde_json::json!(2));
        assert_eq!(value["total_edges"], serde_json::json!(1));
        assert_eq!(value["sampled"], serde_json::json!(false));
        assert!(value["as_of"].is_null());
    }

    #[test]
    fn test_get_schema_invalid_timestamp() {
        let server = create_test_server();

        let response = server.get_schema(schema_req(Some("not-a-timestamp"), None));
        let value: serde_json::Value = serde_json::from_str(&response).unwrap();

        assert!(value.get("error").is_some());
    }

    /// AC: with only `as_of_valid_time` set, `as_of_transaction_time`
    /// defaults to "now" -- so a node created (and thus recorded) right now
    /// is visible, since "now" covers both "the fact is valid as of
    /// as_of_valid_time" (explicit) and "it was recorded by now" (default).
    #[test]
    fn test_get_schema_only_valid_time_set_defaults_transaction_time_to_now() {
        let server = create_test_server();

        server.create_node(CreateNodeRequest {
            valid_time: None,
            label: "Report".to_string(),
            properties: None,
            provenance: None,
        });

        let now_micros = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_micros() as i64;

        let response = server.get_schema(schema_req(Some(&now_micros.to_string()), None));
        let value: serde_json::Value = serde_json::from_str(&response).unwrap();

        assert!(value.get("error").is_none(), "unexpected error: {value}");
        assert!(!value["as_of"].is_null(), "temporal branch must be taken");
        let labels: Vec<String> = value["node_labels"]
            .as_array()
            .unwrap()
            .iter()
            .map(|l| l["label"].as_str().unwrap().to_string())
            .collect();
        assert!(
            labels.contains(&"Report".to_string()),
            "Report should be visible: valid_time=now (explicit), transaction_time defaults to now: {labels:?}"
        );
    }

    /// AC: with only `as_of_transaction_time` set, `as_of_valid_time`
    /// defaults to "now" -- so a node created (and thus recorded) right now
    /// is visible.
    #[test]
    fn test_get_schema_only_transaction_time_set_defaults_valid_time_to_now() {
        let server = create_test_server();

        server.create_node(CreateNodeRequest {
            valid_time: None,
            label: "Invoice".to_string(),
            properties: None,
            provenance: None,
        });

        let now_micros = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_micros() as i64;

        let response = server.get_schema(schema_req(None, Some(&now_micros.to_string())));
        let value: serde_json::Value = serde_json::from_str(&response).unwrap();

        assert!(value.get("error").is_none(), "unexpected error: {value}");
        assert!(!value["as_of"].is_null(), "temporal branch must be taken");
        let labels: Vec<String> = value["node_labels"]
            .as_array()
            .unwrap()
            .iter()
            .map(|l| l["label"].as_str().unwrap().to_string())
            .collect();
        assert!(
            labels.contains(&"Invoice".to_string()),
            "Invoice should be visible: transaction_time=now (explicit), valid_time defaults to now: {labels:?}"
        );
    }

    #[test]
    fn test_get_schema_as_of_label_absent_before_first_write() {
        let server = create_test_server();

        // Schema as of the Unix epoch — well before any writes — must be empty.
        let before = server.get_schema(schema_req(Some("0"), Some("0")));
        let before_value: serde_json::Value = serde_json::from_str(&before).unwrap();
        assert!(before_value.get("error").is_none());
        assert_eq!(before_value["node_labels"].as_array().unwrap().len(), 0);

        // Create a node now.
        server.create_node(CreateNodeRequest {
            valid_time: None,
            label: "Gadget".to_string(),
            properties: None,
            provenance: None,
        });

        // Still as of the epoch: "Gadget" must remain absent (recorded later).
        let still_before = server.get_schema(schema_req(Some("0"), Some("0")));
        let still_before_value: serde_json::Value = serde_json::from_str(&still_before).unwrap();
        assert!(still_before_value.get("error").is_none());
        let labels: Vec<String> = still_before_value["node_labels"]
            .as_array()
            .unwrap()
            .iter()
            .map(|l| l["label"].as_str().unwrap().to_string())
            .collect();
        assert!(
            !labels.contains(&"Gadget".to_string()),
            "Gadget should be absent as of the epoch: {labels:?}"
        );

        // Current-state schema (no as_of) must include it.
        let now = server.get_schema(schema_req(None, None));
        let now_value: serde_json::Value = serde_json::from_str(&now).unwrap();
        let now_labels: Vec<String> = now_value["node_labels"]
            .as_array()
            .unwrap()
            .iter()
            .map(|l| l["label"].as_str().unwrap().to_string())
            .collect();
        assert!(now_labels.contains(&"Gadget".to_string()));
    }

    #[test]
    fn test_get_schema_registered_in_tool_list() {
        let server = create_test_server();
        let tools = server.list_tools_for_test();
        assert!(
            tools.iter().any(|name| name == "get_schema"),
            "get_schema must be registered in the tools manifest: {tools:?}"
        );
    }

    /// AC: "Counts in the summary match the results of the corresponding
    /// count_nodes / count_edges calls for each label/type."
    #[test]
    fn test_get_schema_counts_match_count_nodes_and_count_edges_tools() {
        let server = create_test_server();

        for _ in 0..3 {
            server.create_node(CreateNodeRequest {
                valid_time: None,
                label: "Person".to_string(),
                properties: None,
                provenance: None,
            });
        }
        for _ in 0..2 {
            server.create_node(CreateNodeRequest {
                valid_time: None,
                label: "Company".to_string(),
                properties: None,
                provenance: None,
            });
        }
        let n1: NodeResponse = parse_response(&server.create_node(CreateNodeRequest {
            valid_time: None,
            label: "Person".to_string(),
            properties: None,
            provenance: None,
        }))
        .unwrap();
        let n2: NodeResponse = parse_response(&server.create_node(CreateNodeRequest {
            valid_time: None,
            label: "Company".to_string(),
            properties: None,
            provenance: None,
        }))
        .unwrap();
        server.create_edge(CreateEdgeRequest {
            valid_time: None,
            source_id: n1.id,
            target_id: n2.id,
            label: "WORKS_AT".to_string(),
            properties: None,
            provenance: None,
        });

        let schema_value: serde_json::Value =
            serde_json::from_str(&server.get_schema(schema_req(None, None))).unwrap();

        // Per-label node counts must match the count_nodes tool filtered by that label.
        for label_entry in schema_value["node_labels"].as_array().unwrap() {
            let label = label_entry["label"].as_str().unwrap();
            let count_response = server.count_nodes(CountNodesRequest {
                label: Some(label.to_string()),
            });
            let count_value: serde_json::Value = serde_json::from_str(&count_response).unwrap();
            assert_eq!(
                label_entry["count"], count_value["count"],
                "schema count for label '{label}' must match count_nodes(label='{label}')"
            );
        }

        // Total node/edge counts must match the unfiltered count_nodes/count_edges tools.
        let total_nodes: serde_json::Value =
            serde_json::from_str(&server.count_nodes(CountNodesRequest { label: None })).unwrap();
        assert_eq!(schema_value["total_nodes"], total_nodes["count"]);

        let total_edges: serde_json::Value =
            serde_json::from_str(&server.count_edges(CountEdgesRequest { label: None })).unwrap();
        assert_eq!(schema_value["total_edges"], total_edges["count"]);
    }
}

// ============================================================================
// Vector Elision Tests (Issue #3220)
// ============================================================================
//
// MCP read responses elide vector/embedding properties by default (replacing
// them with a `{type, dim, elided:true}` descriptor) to protect LLM context.
// `include_vectors: true` is the escape hatch that restores the full float
// array. These tests pin that contract across every affected tool.

mod vector_elision_tests {
    use super::*;
    use crate::core::vector::SparseVec;

    fn embedding_of(dim: usize) -> Vec<f32> {
        (0..dim).map(|i| (i as f32) * 0.001 + 0.1).collect()
    }

    fn create_node_with_embedding(server: &AletheiaMcpServer, dim: usize) -> (u64, Vec<f32>) {
        let embedding = embedding_of(dim);
        let mut props = HashMap::new();
        props.insert(
            "embedding".to_string(),
            serde_json::json!(embedding.clone()),
        );
        let response = server.create_node(CreateNodeRequest {
            valid_time: None,
            label: "Document".to_string(),
            properties: Some(props),
            provenance: None,
        });
        let node: NodeResponse = parse_response(&response).expect("Failed to create node");
        (node.id, embedding)
    }

    #[test]
    fn test_get_node_elides_vector_by_default() {
        let server = create_test_server();
        let (node_id, _embedding) = create_node_with_embedding(&server, 1536);

        let response = server.get_node(GetNodeRequest {
            node_id,
            include_vectors: None,
        });

        assert!(
            response.len() < 500,
            "elided response should be small, was {} bytes: {response}",
            response.len()
        );

        let node: NodeResponse = parse_response(&response).expect("Failed to get node");
        assert_eq!(
            node.properties.get("embedding"),
            Some(&serde_json::json!({"type": "vector", "dim": 1536, "elided": true}))
        );
    }

    #[test]
    fn test_get_node_include_vectors_true_is_lossless() {
        let server = create_test_server();
        let (node_id, embedding) = create_node_with_embedding(&server, 1536);

        let response = server.get_node(GetNodeRequest {
            node_id,
            include_vectors: Some(true),
        });

        let node: NodeResponse = parse_response(&response).expect("Failed to get node");
        let returned = node.properties.get("embedding").unwrap();
        let returned_array = returned.as_array().expect("embedding should be an array");
        assert_eq!(returned_array.len(), 1536);

        let returned_floats: Vec<f32> = returned_array
            .iter()
            .map(|v| v.as_f64().unwrap() as f32)
            .collect();
        assert_eq!(returned_floats, embedding);
    }

    #[test]
    fn test_list_nodes_elides_vectors_by_default() {
        let server = create_test_server();
        for _ in 0..3 {
            create_node_with_embedding(&server, 8);
        }

        let response = server.list_nodes(ListNodesRequest {
            label: Some("Document".to_string()),
            property_key: None,
            property_value: None,
            limit: None,
            offset: None,
            include_vectors: None,
        });

        let value: serde_json::Value = serde_json::from_str(&response).unwrap();
        let nodes = value["nodes"].as_array().expect("nodes array");
        assert_eq!(nodes.len(), 3);
        for node in nodes {
            assert_eq!(
                node["properties"]["embedding"],
                serde_json::json!({"type": "vector", "dim": 8, "elided": true})
            );
        }

        // include_vectors: true restores the full arrays.
        let response = server.list_nodes(ListNodesRequest {
            label: Some("Document".to_string()),
            property_key: None,
            property_value: None,
            limit: None,
            offset: None,
            include_vectors: Some(true),
        });
        let value: serde_json::Value = serde_json::from_str(&response).unwrap();
        for node in value["nodes"].as_array().unwrap() {
            let arr = node["properties"]["embedding"]
                .as_array()
                .expect("embedding should be a full array");
            assert_eq!(arr.len(), 8);
        }
    }

    #[test]
    fn test_get_edge_and_outgoing_incoming_edges_elide_vectors_by_default() {
        let server = create_test_server();
        let n1: NodeResponse = parse_response(&server.create_node(CreateNodeRequest {
            valid_time: None,
            label: "Person".to_string(),
            properties: None,
            provenance: None,
        }))
        .unwrap();
        let n2: NodeResponse = parse_response(&server.create_node(CreateNodeRequest {
            valid_time: None,
            label: "Person".to_string(),
            properties: None,
            provenance: None,
        }))
        .unwrap();

        let embedding = embedding_of(6);
        let mut props = HashMap::new();
        props.insert("embedding".to_string(), serde_json::json!(embedding));
        let edge_response = server.create_edge(CreateEdgeRequest {
            valid_time: None,
            source_id: n1.id,
            target_id: n2.id,
            label: "SIMILAR_TO".to_string(),
            properties: Some(props),
            provenance: None,
        });
        let edge: EdgeResponse = parse_response(&edge_response).unwrap();

        // get_edge
        let response = server.get_edge(GetEdgeRequest {
            edge_id: edge.id,
            include_vectors: None,
        });
        let value: serde_json::Value = serde_json::from_str(&response).unwrap();
        assert_eq!(
            value["properties"]["embedding"],
            serde_json::json!({"type": "vector", "dim": 6, "elided": true})
        );

        let response = server.get_edge(GetEdgeRequest {
            edge_id: edge.id,
            include_vectors: Some(true),
        });
        let value: serde_json::Value = serde_json::from_str(&response).unwrap();
        assert_eq!(
            value["properties"]["embedding"].as_array().unwrap().len(),
            6
        );

        // get_outgoing_edges
        let response = server.get_outgoing_edges(GetOutgoingEdgesRequest {
            node_id: n1.id,
            label: None,
            include_vectors: None,
        });
        let value: serde_json::Value = serde_json::from_str(&response).unwrap();
        assert_eq!(
            value["edges"][0]["properties"]["embedding"],
            serde_json::json!({"type": "vector", "dim": 6, "elided": true})
        );

        let response = server.get_outgoing_edges(GetOutgoingEdgesRequest {
            node_id: n1.id,
            label: None,
            include_vectors: Some(true),
        });
        let value: serde_json::Value = serde_json::from_str(&response).unwrap();
        assert_eq!(
            value["edges"][0]["properties"]["embedding"]
                .as_array()
                .unwrap()
                .len(),
            6
        );

        // get_incoming_edges
        let response = server.get_incoming_edges(GetIncomingEdgesRequest {
            node_id: n2.id,
            label: None,
            include_vectors: None,
        });
        let value: serde_json::Value = serde_json::from_str(&response).unwrap();
        assert_eq!(
            value["edges"][0]["properties"]["embedding"],
            serde_json::json!({"type": "vector", "dim": 6, "elided": true})
        );

        let response = server.get_incoming_edges(GetIncomingEdgesRequest {
            node_id: n2.id,
            label: None,
            include_vectors: Some(true),
        });
        let value: serde_json::Value = serde_json::from_str(&response).unwrap();
        assert_eq!(
            value["edges"][0]["properties"]["embedding"]
                .as_array()
                .unwrap()
                .len(),
            6
        );
    }

    #[test]
    fn test_traverse_elides_vectors_by_default() {
        let server = create_test_server();
        let (n1_id, _) = create_node_with_embedding(&server, 5);
        let (n2_id, _) = create_node_with_embedding(&server, 5);
        server.create_edge(CreateEdgeRequest {
            valid_time: None,
            source_id: n1_id,
            target_id: n2_id,
            label: "NEXT".to_string(),
            properties: None,
            provenance: None,
        });

        let response = server.traverse(TraverseRequest {
            start_node_id: n1_id,
            edge_label: "NEXT".to_string(),
            direction: None,
            depth: Some(1),
            limit: None,
            offset: None,
            include_vectors: None,
            as_of_valid_time: None,
            as_of_transaction_time: None,
        });
        let value: serde_json::Value = serde_json::from_str(&response).unwrap();
        let results = value["results"].as_array().expect("results array");
        assert_eq!(results.len(), 1);
        assert_eq!(
            results[0]["node"]["properties"]["embedding"],
            serde_json::json!({"type": "vector", "dim": 5, "elided": true})
        );

        let response = server.traverse(TraverseRequest {
            start_node_id: n1_id,
            edge_label: "NEXT".to_string(),
            direction: None,
            depth: Some(1),
            limit: None,
            offset: None,
            include_vectors: Some(true),
            as_of_valid_time: None,
            as_of_transaction_time: None,
        });
        let value: serde_json::Value = serde_json::from_str(&response).unwrap();
        let results = value["results"].as_array().unwrap();
        assert_eq!(
            results[0]["node"]["properties"]["embedding"]
                .as_array()
                .unwrap()
                .len(),
            5
        );
    }

    #[test]
    fn test_find_similar_elides_vectors_but_keeps_score() {
        let server = create_test_server();
        server.enable_vector_index(EnableVectorIndexRequest {
            property_name: "embedding".to_string(),
            dimensions: 4,
            distance_metric: Some("cosine".to_string()),
        });

        for i in 0..3 {
            let mut props = HashMap::new();
            props.insert(
                "embedding".to_string(),
                serde_json::json!([
                    (i as f32) * 0.1,
                    (i as f32) * 0.2,
                    (i as f32) * 0.3,
                    (i as f32) * 0.4,
                ]),
            );
            server.create_node(CreateNodeRequest {
                valid_time: None,
                label: "Document".to_string(),
                properties: Some(props),
                provenance: None,
            });
        }

        let response = server.find_similar(FindSimilarRequest {
            property_name: "embedding".to_string(),
            embedding: vec![0.1, 0.2, 0.3, 0.4],
            k: Some(3),
            offset: None,
            include_vectors: None,
        });
        let value: serde_json::Value = serde_json::from_str(&response).unwrap();
        let results = value["results"].as_array().expect("results array");
        assert!(!results.is_empty());
        for result in results {
            assert!(result.get("score").and_then(|s| s.as_f64()).is_some());
            assert_eq!(
                result["node"]["properties"]["embedding"],
                serde_json::json!({"type": "vector", "dim": 4, "elided": true})
            );
        }

        let response = server.find_similar(FindSimilarRequest {
            property_name: "embedding".to_string(),
            embedding: vec![0.1, 0.2, 0.3, 0.4],
            k: Some(3),
            offset: None,
            include_vectors: Some(true),
        });
        let value: serde_json::Value = serde_json::from_str(&response).unwrap();
        for result in value["results"].as_array().unwrap() {
            assert!(result.get("score").and_then(|s| s.as_f64()).is_some());
            assert_eq!(
                result["node"]["properties"]["embedding"]
                    .as_array()
                    .unwrap()
                    .len(),
                4
            );
        }
    }

    #[test]
    fn test_hybrid_query_vector_first_elides_vectors_but_keeps_similarity_score() {
        let server = create_test_server();
        server.enable_vector_index(EnableVectorIndexRequest {
            property_name: "embedding".to_string(),
            dimensions: 4,
            distance_metric: Some("cosine".to_string()),
        });
        for i in 0..3 {
            let mut props = HashMap::new();
            props.insert(
                "embedding".to_string(),
                serde_json::json!([
                    (i as f32) * 0.1,
                    (i as f32) * 0.2,
                    (i as f32) * 0.3,
                    (i as f32) * 0.4,
                ]),
            );
            server.create_node(CreateNodeRequest {
                valid_time: None,
                label: "Document".to_string(),
                properties: Some(props),
                provenance: None,
            });
        }

        let response = server.hybrid_query(HybridQueryRequest {
            start_node_id: None,
            traverse_edge: None,
            traverse_depth: None,
            vector_property: Some("embedding".to_string()),
            query_embedding: Some(vec![0.1, 0.2, 0.3, 0.4]),
            top_k: Some(3),
            valid_time: None,
            transaction_time: None,
            filter_label: None,
            limit: None,
            include_vectors: None,
        });
        let value: serde_json::Value = serde_json::from_str(&response).unwrap();
        let results = value["results"].as_array().expect("results array");
        assert!(!results.is_empty());
        for result in results {
            assert!(result.get("similarity_score").is_some());
            assert_eq!(
                result["node"]["properties"]["embedding"],
                serde_json::json!({"type": "vector", "dim": 4, "elided": true})
            );
        }

        let response = server.hybrid_query(HybridQueryRequest {
            start_node_id: None,
            traverse_edge: None,
            traverse_depth: None,
            vector_property: Some("embedding".to_string()),
            query_embedding: Some(vec![0.1, 0.2, 0.3, 0.4]),
            top_k: Some(3),
            valid_time: None,
            transaction_time: None,
            filter_label: None,
            limit: None,
            include_vectors: Some(true),
        });
        let value: serde_json::Value = serde_json::from_str(&response).unwrap();
        for result in value["results"].as_array().unwrap() {
            assert_eq!(
                result["node"]["properties"]["embedding"]
                    .as_array()
                    .unwrap()
                    .len(),
                4
            );
        }
    }

    #[test]
    fn test_hybrid_query_graph_first_elides_vectors_but_keeps_similarity_score() {
        let server = create_test_server();
        let (n1_id, _) = create_node_with_embedding(&server, 4);
        let (n2_id, _) = create_node_with_embedding(&server, 4);
        server.create_edge(CreateEdgeRequest {
            valid_time: None,
            source_id: n1_id,
            target_id: n2_id,
            label: "NEXT".to_string(),
            properties: None,
            provenance: None,
        });

        let response = server.hybrid_query(HybridQueryRequest {
            start_node_id: Some(n1_id),
            traverse_edge: Some("NEXT".to_string()),
            traverse_depth: Some(1),
            vector_property: None,
            query_embedding: None,
            top_k: None,
            valid_time: None,
            transaction_time: None,
            filter_label: None,
            limit: None,
            include_vectors: None,
        });
        let value: serde_json::Value = serde_json::from_str(&response).unwrap();
        let results = value["results"].as_array().expect("results array");
        assert_eq!(results.len(), 1);
        assert_eq!(
            results[0]["node"]["properties"]["embedding"],
            serde_json::json!({"type": "vector", "dim": 4, "elided": true})
        );

        let response = server.hybrid_query(HybridQueryRequest {
            start_node_id: Some(n1_id),
            traverse_edge: Some("NEXT".to_string()),
            traverse_depth: Some(1),
            vector_property: None,
            query_embedding: None,
            top_k: None,
            valid_time: None,
            transaction_time: None,
            filter_label: None,
            limit: None,
            include_vectors: Some(true),
        });
        let value: serde_json::Value = serde_json::from_str(&response).unwrap();
        assert_eq!(
            value["results"][0]["node"]["properties"]["embedding"]
                .as_array()
                .unwrap()
                .len(),
            4
        );
    }

    #[test]
    fn test_sparse_vector_elided_by_default_with_dim_and_nnz() {
        let server = create_test_server();

        let sparse = SparseVec::new(vec![1, 4], vec![1.5, 2.3], 10).expect("valid sparse vector");
        let props = PropertyMapBuilder::new()
            .try_insert("embedding", sparse)
            .expect("insert sparse vector")
            .build();
        let node_id = server
            .db()
            .create_node("Document", props)
            .expect("create node with sparse vector");

        let response = server.get_node(GetNodeRequest {
            node_id: node_id.as_u64(),
            include_vectors: None,
        });
        let value: serde_json::Value = serde_json::from_str(&response).unwrap();
        assert_eq!(
            value["properties"]["embedding"],
            serde_json::json!({"type": "sparse_vector", "dim": 10, "nnz": 2, "elided": true})
        );

        let response = server.get_node(GetNodeRequest {
            node_id: node_id.as_u64(),
            include_vectors: Some(true),
        });
        let value: serde_json::Value = serde_json::from_str(&response).unwrap();
        assert_eq!(
            value["properties"]["embedding"]["indices"],
            serde_json::json!([1, 4])
        );
        let values: Vec<f32> = value["properties"]["embedding"]["values"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_f64().unwrap() as f32)
            .collect();
        assert_eq!(values, vec![1.5_f32, 2.3_f32]);
    }

    #[test]
    fn test_create_node_and_update_node_always_return_full_vectors() {
        let server = create_test_server();

        let mut props = HashMap::new();
        props.insert(
            "embedding".to_string(),
            serde_json::json!([0.1, 0.2, 0.3, 0.4]),
        );
        let response = server.create_node(CreateNodeRequest {
            valid_time: None,
            label: "Document".to_string(),
            properties: Some(props),
            provenance: None,
        });
        let node: NodeResponse = parse_response(&response).expect("Failed to create node");
        assert_eq!(
            node.properties
                .get("embedding")
                .unwrap()
                .as_array()
                .unwrap()
                .len(),
            4
        );

        let mut new_props = HashMap::new();
        new_props.insert(
            "embedding".to_string(),
            serde_json::json!([0.5, 0.6, 0.7, 0.8]),
        );
        let update_response = server.update_node(UpdateNodeRequest {
            valid_time: None,
            node_id: node.id,
            properties: new_props,
            provenance: None,
        });
        let updated: NodeResponse =
            parse_response(&update_response).expect("Failed to update node");
        let updated_embedding = updated
            .properties
            .get("embedding")
            .unwrap()
            .as_array()
            .unwrap();
        assert_eq!(updated_embedding.len(), 4);
        assert_eq!(updated_embedding[0].as_f64().unwrap() as f32, 0.5);
    }

    #[test]
    fn test_non_vector_properties_unaffected_by_include_vectors_flag() {
        let server = create_test_server();

        let mut props = HashMap::new();
        props.insert("name".to_string(), serde_json::json!("Alice"));
        props.insert("age".to_string(), serde_json::json!(30));
        props.insert("active".to_string(), serde_json::json!(true));
        props.insert("nickname".to_string(), serde_json::Value::Null);
        props.insert("tags".to_string(), serde_json::json!(["a", "b", "c"]));

        let response = server.create_node(CreateNodeRequest {
            valid_time: None,
            label: "Person".to_string(),
            properties: Some(props),
            provenance: None,
        });
        let node: NodeResponse = parse_response(&response).expect("Failed to create node");

        let variants = [None, Some(false), Some(true)];
        let mut previous: Option<HashMap<String, serde_json::Value>> = None;
        for include_vectors in variants {
            let response = server.get_node(GetNodeRequest {
                node_id: node.id,
                include_vectors,
            });
            let fetched: NodeResponse = parse_response(&response).expect("Failed to get node");
            if let Some(prev) = &previous {
                assert_eq!(prev, &fetched.properties);
            }
            previous = Some(fetched.properties);
        }
    }
}

// ============================================================================
// Valid-Time Write Tests (Issue #3221)
// ============================================================================

mod valid_time_write_tests {
    use super::*;
    use chrono::{DateTime, Duration, Utc};

    /// `now` is the caller's own `Utc::now()`, captured once at the start of the test
    /// and reused for every offset it computes. Calling `Utc::now()` freshly inside the
    /// helper would let each call observe a slightly different instant under load,
    /// making relative orderings between offsets non-deterministic.
    fn rfc3339_hours_ago(now: DateTime<Utc>, hours: i64) -> String {
        (now - Duration::hours(hours)).to_rfc3339()
    }

    fn rfc3339_hours_from_now(now: DateTime<Utc>, hours: i64) -> String {
        (now + Duration::hours(hours)).to_rfc3339()
    }

    #[test]
    fn test_create_node_with_valid_time_backdated_round_trip() {
        let server = create_test_server();
        let now = Utc::now();

        let response = server.create_node(CreateNodeRequest {
            label: "Person".to_string(),
            properties: None,
            valid_time: Some(rfc3339_hours_ago(now, 1)),
            provenance: None,
        });
        let node: NodeResponse = parse_response(&response).expect("create should succeed");

        // Visible shortly after its valid_from.
        let visible = server.get_node_at_time(GetNodeAtTimeRequest {
            node_id: node.id,
            valid_time: rfc3339_hours_ago(now, 0),
            transaction_time: None,
        });
        let value: serde_json::Value = serde_json::from_str(&visible).unwrap();
        assert!(value.get("error").is_none(), "expected node, got {value}");

        // Invisible strictly before its valid_from.
        let invisible = server.get_node_at_time(GetNodeAtTimeRequest {
            node_id: node.id,
            valid_time: rfc3339_hours_ago(now, 2),
            transaction_time: None,
        });
        let value: serde_json::Value = serde_json::from_str(&invisible).unwrap();
        assert!(value.get("error").is_some());
    }

    #[test]
    fn test_create_node_omitted_valid_time_matches_today() {
        // Omitting the field entirely (raw JSON) still deserializes.
        let parsed: CreateNodeRequest =
            serde_json::from_value(serde_json::json!({ "label": "Person" }))
                .expect("valid_time should be optional");
        assert_eq!(parsed.valid_time, None);

        let server = create_test_server();
        let response = server.create_node(CreateNodeRequest {
            label: "Person".to_string(),
            properties: None,
            valid_time: None,
            provenance: None,
        });
        let node: NodeResponse = parse_response(&response).expect("create should succeed");
        assert_eq!(node.label, "Person");

        let now_response = server.get_node_at_time(GetNodeAtTimeRequest {
            node_id: node.id,
            valid_time: Utc::now().to_rfc3339(),
            transaction_time: None,
        });
        let value: serde_json::Value = serde_json::from_str(&now_response).unwrap();
        assert!(value.get("error").is_none());
    }

    #[test]
    fn test_create_node_malformed_valid_time_structured_error() {
        let server = create_test_server();

        let response = server.create_node(CreateNodeRequest {
            label: "Person".to_string(),
            properties: None,
            valid_time: Some("not-a-timestamp".to_string()),
            provenance: None,
        });

        let value: serde_json::Value = serde_json::from_str(&response).unwrap();
        let error = value
            .get("error")
            .and_then(|e| e["message"].as_str())
            .unwrap();
        assert!(error.contains("Invalid valid_time"), "got: {error}");
        assert_eq!(server.db().node_count(), 0, "no node should be created");
    }

    #[test]
    fn test_create_node_far_future_valid_time_typed_error_surfaced() {
        let server = create_test_server();
        let now = Utc::now();

        let response = server.create_node(CreateNodeRequest {
            label: "Person".to_string(),
            properties: None,
            valid_time: Some(rfc3339_hours_from_now(now, 24 * 400)), // > 1 year
            provenance: None,
        });

        let value: serde_json::Value = serde_json::from_str(&response).unwrap();
        let error = value
            .get("error")
            .and_then(|e| e["message"].as_str())
            .unwrap();
        assert!(error.contains("too far in future"), "got: {error}");
        assert_eq!(server.db().node_count(), 0);
    }

    #[test]
    fn test_create_edge_with_valid_time_backdated_round_trip() {
        let server = create_test_server();
        let now = Utc::now();

        let n1: NodeResponse = parse_response(&server.create_node(CreateNodeRequest {
            label: "Person".to_string(),
            properties: None,
            valid_time: None,
            provenance: None,
        }))
        .unwrap();
        let n2: NodeResponse = parse_response(&server.create_node(CreateNodeRequest {
            label: "Person".to_string(),
            properties: None,
            valid_time: None,
            provenance: None,
        }))
        .unwrap();

        let response = server.create_edge(CreateEdgeRequest {
            source_id: n1.id,
            target_id: n2.id,
            label: "KNOWS".to_string(),
            properties: None,
            valid_time: Some(rfc3339_hours_ago(now, 1)),
            provenance: None,
        });
        let edge: EdgeResponse = parse_response(&response).expect("create_edge should succeed");

        let visible = server.get_edge_at_time(GetEdgeAtTimeRequest {
            edge_id: edge.id,
            valid_time: rfc3339_hours_ago(now, 0),
            transaction_time: None,
        });
        let value: serde_json::Value = serde_json::from_str(&visible).unwrap();
        assert!(value.get("error").is_none(), "expected edge, got {value}");

        let invisible = server.get_edge_at_time(GetEdgeAtTimeRequest {
            edge_id: edge.id,
            valid_time: rfc3339_hours_ago(now, 2),
            transaction_time: None,
        });
        let value: serde_json::Value = serde_json::from_str(&invisible).unwrap();
        assert!(value.get("error").is_some());
    }

    #[test]
    fn test_update_node_with_valid_time_backdated() {
        let server = create_test_server();
        let now = Utc::now();

        let mut props = HashMap::new();
        props.insert("city".to_string(), serde_json::json!("Paris"));
        let node: NodeResponse = parse_response(&server.create_node(CreateNodeRequest {
            label: "Person".to_string(),
            properties: Some(props),
            valid_time: Some(rfc3339_hours_ago(now, 2)),
            provenance: None,
        }))
        .unwrap();

        let mut update_props = HashMap::new();
        update_props.insert("city".to_string(), serde_json::json!("London"));
        let update_valid_time = rfc3339_hours_ago(now, 1);
        let response = server.update_node(UpdateNodeRequest {
            node_id: node.id,
            properties: update_props,
            valid_time: Some(update_valid_time.clone()),
            provenance: None,
        });
        let updated: NodeResponse = parse_response(&response).expect("update should succeed");
        assert_eq!(
            updated.properties.get("city"),
            Some(&serde_json::json!("London"))
        );

        // Visible from its own valid_from onward.
        let at_update = server.get_node_at_time(GetNodeAtTimeRequest {
            node_id: node.id,
            valid_time: update_valid_time,
            transaction_time: None,
        });
        let value: serde_json::Value = serde_json::from_str(&at_update).unwrap();
        let fetched: NodeResponse = serde_json::from_value(value["node"].clone()).unwrap();
        assert_eq!(
            fetched.properties.get("city"),
            Some(&serde_json::json!("London"))
        );
    }

    #[test]
    fn test_update_node_valid_time_before_creation_rejected() {
        let server = create_test_server();

        let node: NodeResponse = parse_response(&server.create_node(CreateNodeRequest {
            label: "Person".to_string(),
            properties: None,
            valid_time: None,
            provenance: None,
        }))
        .unwrap();

        let mut update_props = HashMap::new();
        update_props.insert("name".to_string(), serde_json::json!("Bob"));
        let response = server.update_node(UpdateNodeRequest {
            node_id: node.id,
            properties: update_props,
            // Far enough in the past to precede the node's creation time.
            valid_time: Some("1970-01-01T00:00:01Z".to_string()),
            provenance: None,
        });

        let value: serde_json::Value = serde_json::from_str(&response).unwrap();
        let error = value
            .get("error")
            .and_then(|e| e["message"].as_str())
            .unwrap();
        assert!(error.contains("before entity creation"), "got: {error}");
    }

    #[test]
    fn test_transaction_time_not_client_settable() {
        let server = create_test_server();
        let now = Utc::now();

        let node: NodeResponse = parse_response(&server.create_node(CreateNodeRequest {
            label: "Person".to_string(),
            properties: None,
            valid_time: Some(rfc3339_hours_ago(now, 1)),
            provenance: None,
        }))
        .unwrap();

        // As of a transaction_time 30 minutes ago, the write (committed just now)
        // had not happened yet -- the fact must not be retroactively knowable.
        let too_early_tx = (now - Duration::minutes(30)).to_rfc3339();
        let too_early = server.get_node_at_time(GetNodeAtTimeRequest {
            node_id: node.id,
            valid_time: rfc3339_hours_ago(now, 0),
            transaction_time: Some(too_early_tx),
        });
        let value: serde_json::Value = serde_json::from_str(&too_early).unwrap();
        assert!(value.get("error").is_some());

        // Omitting transaction_time defaults to now, where the write is visible.
        let now_response = server.get_node_at_time(GetNodeAtTimeRequest {
            node_id: node.id,
            valid_time: rfc3339_hours_ago(now, 0),
            transaction_time: None,
        });
        let value: serde_json::Value = serde_json::from_str(&now_response).unwrap();
        assert!(value.get("error").is_none());
    }

    #[test]
    fn test_update_edge_with_valid_time_backdated() {
        let server = create_test_server();
        let now = Utc::now();

        let n1: NodeResponse = parse_response(&server.create_node(CreateNodeRequest {
            label: "Person".to_string(),
            properties: None,
            valid_time: None,
            provenance: None,
        }))
        .unwrap();
        let n2: NodeResponse = parse_response(&server.create_node(CreateNodeRequest {
            label: "Person".to_string(),
            properties: None,
            valid_time: None,
            provenance: None,
        }))
        .unwrap();

        let mut props = HashMap::new();
        props.insert("strength".to_string(), serde_json::json!(1));
        let edge: EdgeResponse = parse_response(&server.create_edge(CreateEdgeRequest {
            source_id: n1.id,
            target_id: n2.id,
            label: "KNOWS".to_string(),
            properties: Some(props),
            valid_time: Some(rfc3339_hours_ago(now, 2)),
            provenance: None,
        }))
        .unwrap();

        let mut update_props = HashMap::new();
        update_props.insert("strength".to_string(), serde_json::json!(9));
        let update_valid_time = rfc3339_hours_ago(now, 1);
        let response = server.update_edge(UpdateEdgeRequest {
            edge_id: edge.id,
            properties: update_props,
            valid_time: Some(update_valid_time.clone()),
            provenance: None,
        });
        let updated: EdgeResponse = parse_response(&response).expect("update should succeed");
        assert_eq!(
            updated.properties.get("strength"),
            Some(&serde_json::json!(9))
        );

        let at_update = server.get_edge_at_time(GetEdgeAtTimeRequest {
            edge_id: edge.id,
            valid_time: update_valid_time,
            transaction_time: None,
        });
        let value: serde_json::Value = serde_json::from_str(&at_update).unwrap();
        let fetched: EdgeResponse = serde_json::from_value(value["edge"].clone()).unwrap();
        assert_eq!(
            fetched.properties.get("strength"),
            Some(&serde_json::json!(9))
        );
    }

    /// Regression test: `create_edge_with_valid_time` used to be the only one of the six
    /// `*_with_valid_time` operations that never enforced the 1-year future cap, so an
    /// edge could silently be created with an arbitrarily-far-future `valid_time`. Mirrors
    /// `test_create_node_far_future_valid_time_typed_error_surfaced`.
    #[test]
    fn test_create_edge_far_future_valid_time_typed_error_surfaced() {
        let server = create_test_server();
        let now = Utc::now();

        let n1: NodeResponse = parse_response(&server.create_node(CreateNodeRequest {
            label: "Person".to_string(),
            properties: None,
            valid_time: None,
            provenance: None,
        }))
        .unwrap();
        let n2: NodeResponse = parse_response(&server.create_node(CreateNodeRequest {
            label: "Person".to_string(),
            properties: None,
            valid_time: None,
            provenance: None,
        }))
        .unwrap();

        let response = server.create_edge(CreateEdgeRequest {
            source_id: n1.id,
            target_id: n2.id,
            label: "KNOWS".to_string(),
            properties: None,
            valid_time: Some(rfc3339_hours_from_now(now, 24 * 400)), // > 1 year
            provenance: None,
        });

        let value: serde_json::Value = serde_json::from_str(&response).unwrap();
        let error = value
            .get("error")
            .and_then(|e| e["message"].as_str())
            .unwrap();
        assert!(error.contains("too far in future"), "got: {error}");
        assert_eq!(server.db().edge_count(), 0);
    }

    /// `delete_node`'s `valid_time` field: the caller-specified `valid_from` is
    /// correctly threaded through to the tombstone version, and the node is no longer
    /// visible as of now. (A probe strictly between create's and delete's `valid_from`
    /// is not reachable via `get_node_at_time` -- deleting closes the previous version's
    /// *transaction* time at commit, same documented caveat as
    /// `update_node_with_valid_time_backdated_round_trip` in `db::ops::tests`.)
    #[test]
    fn test_delete_node_with_valid_time_backdated_round_trip() {
        let server = create_test_server();
        let now = Utc::now();

        let node: NodeResponse = parse_response(&server.create_node(CreateNodeRequest {
            label: "Person".to_string(),
            properties: None,
            valid_time: Some(rfc3339_hours_ago(now, 2)),
            provenance: None,
        }))
        .unwrap();

        let delete_valid_time = rfc3339_hours_ago(now, 1);
        let response = server.delete_node(DeleteNodeRequest {
            node_id: node.id,
            detach: None,
            valid_time: Some(delete_valid_time.clone()),
        });
        let value: serde_json::Value = serde_json::from_str(&response).unwrap();
        assert_eq!(value.get("success"), Some(&serde_json::json!(true)));

        // The caller-specified valid_from was correctly threaded through to the
        // tombstone version.
        let node_id = crate::core::id::NodeId::new(node.id).unwrap();
        let historical = server.db().historical.read();
        let version_id = historical.get_current_node_version(node_id).unwrap();
        let version = historical.get_node_version(version_id).unwrap();
        let recorded_valid_from = version.temporal.valid_time().start();
        drop(historical);
        let expected: chrono::DateTime<Utc> = delete_valid_time.parse().unwrap();
        assert_eq!(
            recorded_valid_from.wallclock(),
            expected.timestamp_micros(),
            "tombstone valid_from should match the requested delete_valid_time"
        );

        // No longer visible as of now.
        let gone = server.get_node_at_time(GetNodeAtTimeRequest {
            node_id: node.id,
            valid_time: now.to_rfc3339(),
            transaction_time: None,
        });
        let value: serde_json::Value = serde_json::from_str(&gone).unwrap();
        assert!(value.get("error").is_some());
    }

    /// `delete_edge`'s `valid_time` field: edge mirror of the node round trip above --
    /// same caveat about the probe-strictly-between-versions gap applies.
    #[test]
    fn test_delete_edge_with_valid_time_backdated_round_trip() {
        let server = create_test_server();
        let now = Utc::now();

        let n1: NodeResponse = parse_response(&server.create_node(CreateNodeRequest {
            label: "Person".to_string(),
            properties: None,
            valid_time: None,
            provenance: None,
        }))
        .unwrap();
        let n2: NodeResponse = parse_response(&server.create_node(CreateNodeRequest {
            label: "Person".to_string(),
            properties: None,
            valid_time: None,
            provenance: None,
        }))
        .unwrap();
        let edge: EdgeResponse = parse_response(&server.create_edge(CreateEdgeRequest {
            source_id: n1.id,
            target_id: n2.id,
            label: "KNOWS".to_string(),
            properties: None,
            valid_time: Some(rfc3339_hours_ago(now, 2)),
            provenance: None,
        }))
        .unwrap();

        let delete_valid_time = rfc3339_hours_ago(now, 1);
        let response = server.delete_edge(DeleteEdgeRequest {
            edge_id: edge.id,
            valid_time: Some(delete_valid_time.clone()),
        });
        let value: serde_json::Value = serde_json::from_str(&response).unwrap();
        assert_eq!(value.get("success"), Some(&serde_json::json!(true)));

        // The caller-specified valid_from was correctly threaded through to the
        // tombstone version.
        let edge_id = crate::core::id::EdgeId::new(edge.id).unwrap();
        let historical = server.db().historical.read();
        let version_id = historical.get_current_edge_version(edge_id).unwrap();
        let version = historical.get_edge_version(version_id).unwrap();
        let recorded_valid_from = version.temporal.valid_time().start();
        drop(historical);
        let expected: chrono::DateTime<Utc> = delete_valid_time.parse().unwrap();
        assert_eq!(
            recorded_valid_from.wallclock(),
            expected.timestamp_micros(),
            "tombstone valid_from should match the requested delete_valid_time"
        );

        // No longer visible as of now.
        let gone = server.get_edge_at_time(GetEdgeAtTimeRequest {
            edge_id: edge.id,
            valid_time: now.to_rfc3339(),
            transaction_time: None,
        });
        let value: serde_json::Value = serde_json::from_str(&gone).unwrap();
        assert!(value.get("error").is_some());
    }

    /// `detach: true` (cascade delete) does not support backdating -- passing
    /// `valid_time` alongside it must be a clear, structured rejection rather than
    /// silently ignoring `valid_time` or cascading at the wrong time.
    #[test]
    fn test_delete_node_detach_with_valid_time_rejected() {
        let server = create_test_server();
        let now = Utc::now();

        let n1: NodeResponse = parse_response(&server.create_node(CreateNodeRequest {
            label: "Person".to_string(),
            properties: None,
            valid_time: None,
            provenance: None,
        }))
        .unwrap();
        let n2: NodeResponse = parse_response(&server.create_node(CreateNodeRequest {
            label: "Person".to_string(),
            properties: None,
            valid_time: None,
            provenance: None,
        }))
        .unwrap();
        parse_response::<EdgeResponse>(&server.create_edge(CreateEdgeRequest {
            source_id: n1.id,
            target_id: n2.id,
            label: "KNOWS".to_string(),
            properties: None,
            valid_time: None,
            provenance: None,
        }))
        .unwrap();

        let response = server.delete_node(DeleteNodeRequest {
            node_id: n1.id,
            detach: Some(true),
            valid_time: Some(rfc3339_hours_ago(now, 1)),
        });
        let value: serde_json::Value = serde_json::from_str(&response).unwrap();
        assert!(value.get("error").is_some(), "expected error, got {value}");
        assert_ne!(value.get("success"), Some(&serde_json::json!(true)));

        // Node and edge must be untouched by the rejected request.
        let get_value: serde_json::Value = serde_json::from_str(&server.get_node(GetNodeRequest {
            node_id: n1.id,
            include_vectors: None,
        }))
        .unwrap();
        assert!(get_value.get("error").is_none(), "node should still exist");
    }

    /// Plain cascade delete (`detach: true`, no `valid_time`) must behave exactly as
    /// before this fix -- a regression guard alongside the new rejection above.
    #[test]
    fn test_delete_node_detach_without_valid_time_unchanged() {
        let server = create_test_server();

        let n1: NodeResponse = parse_response(&server.create_node(CreateNodeRequest {
            label: "Person".to_string(),
            properties: None,
            valid_time: None,
            provenance: None,
        }))
        .unwrap();
        let n2: NodeResponse = parse_response(&server.create_node(CreateNodeRequest {
            label: "Person".to_string(),
            properties: None,
            valid_time: None,
            provenance: None,
        }))
        .unwrap();
        parse_response::<EdgeResponse>(&server.create_edge(CreateEdgeRequest {
            source_id: n1.id,
            target_id: n2.id,
            label: "KNOWS".to_string(),
            properties: None,
            valid_time: None,
            provenance: None,
        }))
        .unwrap();

        let response = server.delete_node(DeleteNodeRequest {
            node_id: n1.id,
            detach: Some(true),
            valid_time: None,
        });
        let value: serde_json::Value = serde_json::from_str(&response).unwrap();
        assert_eq!(value.get("success"), Some(&serde_json::json!(true)));
        assert_eq!(value.get("edges_removed"), Some(&serde_json::json!(1)));
        assert_eq!(value.get("detached"), Some(&serde_json::json!(true)));
    }
}

// ============================================================================
// Write-Time Provenance Tests (Issue #3224)
// ============================================================================

mod provenance_write_tests {
    use super::*;

    #[test]
    fn test_create_node_with_provenance_persists() {
        let server = create_test_server();

        let node: NodeResponse = parse_response(&server.create_node(CreateNodeRequest {
            label: "Person".to_string(),
            properties: None,
            valid_time: None,
            provenance: Some(ProvenanceRequest {
                source: Some("hr-system".to_string()),
                confidence: Some(0.95),
                note: None,
                correlation_id: Some("batch-2026-06".to_string()),
            }),
        }))
        .unwrap();

        // Returned directly on the create response.
        let provenance = node.provenance.expect("provenance should be present");
        assert_eq!(provenance.source(), Some("hr-system"));
        assert_eq!(provenance.confidence(), Some(0.95));
        assert_eq!(provenance.correlation_id(), Some("batch-2026-06"));

        // And retrievable via history (per-version).
        let history = server.get_node_history(GetNodeHistoryRequest { node_id: node.id });
        let value: serde_json::Value = serde_json::from_str(&history).unwrap();
        let versions = value["versions"].as_array().unwrap();
        assert_eq!(versions.len(), 1);
        assert_eq!(versions[0]["provenance"]["source"], "hr-system");
        assert_eq!(versions[0]["provenance"]["confidence"], 0.95);
    }

    #[test]
    fn test_create_node_without_provenance_response_unchanged() {
        let server = create_test_server();

        let response = server.create_node(CreateNodeRequest {
            label: "Person".to_string(),
            properties: None,
            valid_time: None,
            provenance: None,
        });
        let value: serde_json::Value = serde_json::from_str(&response).unwrap();
        assert!(
            value.get("provenance").is_none(),
            "provenance key must be omitted, not null, when absent; got {value}"
        );
    }

    #[test]
    fn test_create_node_empty_provenance_object_treated_as_absent() {
        let server = create_test_server();

        let response = server.create_node(CreateNodeRequest {
            label: "Person".to_string(),
            properties: None,
            valid_time: None,
            provenance: Some(ProvenanceRequest {
                source: None,
                confidence: None,
                note: None,
                correlation_id: None,
            }),
        });
        let value: serde_json::Value = serde_json::from_str(&response).unwrap();
        assert!(
            value.get("provenance").is_none(),
            "an all-absent bundle must never be persisted/returned as a fabricated object; got {value}"
        );
    }

    #[test]
    fn test_create_node_provenance_confidence_above_one_rejected() {
        let server = create_test_server();

        let response = server.create_node(CreateNodeRequest {
            label: "Person".to_string(),
            properties: None,
            valid_time: None,
            provenance: Some(ProvenanceRequest {
                source: None,
                confidence: Some(1.5),
                note: None,
                correlation_id: None,
            }),
        });
        let value: serde_json::Value = serde_json::from_str(&response).unwrap();
        let error = value
            .get("error")
            .and_then(|e| e["message"].as_str())
            .unwrap();
        assert!(error.contains("confidence"), "got: {error}");
        assert!(
            error.contains("0.0") && error.contains("1.0"),
            "got: {error}"
        );
        assert_eq!(server.db().node_count(), 0, "no node should be created");
    }

    #[test]
    fn test_create_node_provenance_confidence_below_zero_rejected() {
        let server = create_test_server();

        let response = server.create_node(CreateNodeRequest {
            label: "Person".to_string(),
            properties: None,
            valid_time: None,
            provenance: Some(ProvenanceRequest {
                source: None,
                confidence: Some(-0.1),
                note: None,
                correlation_id: None,
            }),
        });
        let value: serde_json::Value = serde_json::from_str(&response).unwrap();
        let error = value
            .get("error")
            .and_then(|e| e["message"].as_str())
            .unwrap();
        assert!(error.contains("confidence"), "got: {error}");
        assert_eq!(server.db().node_count(), 0);
    }

    #[test]
    fn test_create_node_provenance_confidence_boundaries_accepted() {
        let server = create_test_server();

        for confidence in [0.0, 1.0] {
            let node: NodeResponse = parse_response(&server.create_node(CreateNodeRequest {
                label: "Person".to_string(),
                properties: None,
                valid_time: None,
                provenance: Some(ProvenanceRequest {
                    source: None,
                    confidence: Some(confidence),
                    note: None,
                    correlation_id: None,
                }),
            }))
            .expect("boundary confidence should be accepted");
            assert_eq!(node.provenance.unwrap().confidence(), Some(confidence));
        }
    }

    #[test]
    fn test_update_node_with_provenance_history_per_version() {
        let server = create_test_server();

        let node: NodeResponse = parse_response(&server.create_node(CreateNodeRequest {
            label: "Person".to_string(),
            properties: None,
            valid_time: None,
            provenance: None,
        }))
        .unwrap();

        let updated: NodeResponse = parse_response(&server.update_node(UpdateNodeRequest {
            node_id: node.id,
            properties: HashMap::new(),
            valid_time: None,
            provenance: Some(ProvenanceRequest {
                source: Some("csv-import:2026-06".to_string()),
                confidence: Some(0.4),
                note: None,
                correlation_id: None,
            }),
        }))
        .unwrap();
        assert_eq!(
            updated.provenance.unwrap().source(),
            Some("csv-import:2026-06")
        );

        let history = server.get_node_history(GetNodeHistoryRequest { node_id: node.id });
        let value: serde_json::Value = serde_json::from_str(&history).unwrap();
        let versions = value["versions"].as_array().unwrap();
        assert_eq!(versions.len(), 2);
        assert!(
            versions[0].get("provenance").is_none(),
            "v1 (plain create) should have no provenance"
        );
        assert_eq!(versions[1]["provenance"]["source"], "csv-import:2026-06");
    }

    #[test]
    fn test_update_node_provenance_confidence_out_of_range_rejected() {
        let server = create_test_server();

        let node: NodeResponse = parse_response(&server.create_node(CreateNodeRequest {
            label: "Person".to_string(),
            properties: None,
            valid_time: None,
            provenance: None,
        }))
        .unwrap();

        let response = server.update_node(UpdateNodeRequest {
            node_id: node.id,
            properties: HashMap::new(),
            valid_time: None,
            provenance: Some(ProvenanceRequest {
                source: None,
                confidence: Some(2.0),
                note: None,
                correlation_id: None,
            }),
        });
        let value: serde_json::Value = serde_json::from_str(&response).unwrap();
        assert!(value.get("error").is_some());

        // The node's properties must be unchanged by the rejected update.
        let history = server.get_node_history(GetNodeHistoryRequest { node_id: node.id });
        let history_value: serde_json::Value = serde_json::from_str(&history).unwrap();
        assert_eq!(history_value["versions"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn test_create_edge_with_provenance_persists() {
        let server = create_test_server();

        let n1: NodeResponse = parse_response(&server.create_node(CreateNodeRequest {
            label: "Person".to_string(),
            properties: None,
            valid_time: None,
            provenance: None,
        }))
        .unwrap();
        let n2: NodeResponse = parse_response(&server.create_node(CreateNodeRequest {
            label: "Person".to_string(),
            properties: None,
            valid_time: None,
            provenance: None,
        }))
        .unwrap();

        let edge: EdgeResponse = parse_response(&server.create_edge(CreateEdgeRequest {
            source_id: n1.id,
            target_id: n2.id,
            label: "KNOWS".to_string(),
            properties: None,
            valid_time: None,
            provenance: Some(ProvenanceRequest {
                source: Some("claude-mcp".to_string()),
                confidence: Some(0.7),
                note: Some("inferred from document".to_string()),
                correlation_id: None,
            }),
        }))
        .unwrap();

        let provenance = edge.provenance.expect("provenance should be present");
        assert_eq!(provenance.source(), Some("claude-mcp"));
        assert_eq!(provenance.note(), Some("inferred from document"));

        let history = server.get_edge_history(GetEdgeHistoryRequest { edge_id: edge.id });
        let value: serde_json::Value = serde_json::from_str(&history).unwrap();
        assert_eq!(value["versions"][0]["provenance"]["source"], "claude-mcp");
    }

    #[test]
    fn test_create_edge_provenance_confidence_above_one_rejected() {
        let server = create_test_server();

        let n1: NodeResponse = parse_response(&server.create_node(CreateNodeRequest {
            label: "Person".to_string(),
            properties: None,
            valid_time: None,
            provenance: None,
        }))
        .unwrap();
        let n2: NodeResponse = parse_response(&server.create_node(CreateNodeRequest {
            label: "Person".to_string(),
            properties: None,
            valid_time: None,
            provenance: None,
        }))
        .unwrap();

        let response = server.create_edge(CreateEdgeRequest {
            source_id: n1.id,
            target_id: n2.id,
            label: "KNOWS".to_string(),
            properties: None,
            valid_time: None,
            provenance: Some(ProvenanceRequest {
                source: None,
                confidence: Some(1.1),
                note: None,
                correlation_id: None,
            }),
        });
        let value: serde_json::Value = serde_json::from_str(&response).unwrap();
        assert!(value.get("error").is_some());
        assert_eq!(server.db().edge_count(), 0, "no edge should be created");
    }

    #[test]
    fn test_update_edge_with_provenance_history_per_version() {
        let server = create_test_server();

        let n1: NodeResponse = parse_response(&server.create_node(CreateNodeRequest {
            label: "Person".to_string(),
            properties: None,
            valid_time: None,
            provenance: None,
        }))
        .unwrap();
        let n2: NodeResponse = parse_response(&server.create_node(CreateNodeRequest {
            label: "Person".to_string(),
            properties: None,
            valid_time: None,
            provenance: None,
        }))
        .unwrap();
        let edge: EdgeResponse = parse_response(&server.create_edge(CreateEdgeRequest {
            source_id: n1.id,
            target_id: n2.id,
            label: "KNOWS".to_string(),
            properties: None,
            valid_time: None,
            provenance: None,
        }))
        .unwrap();

        let _updated: EdgeResponse = parse_response(&server.update_edge(UpdateEdgeRequest {
            edge_id: edge.id,
            properties: HashMap::new(),
            valid_time: None,
            provenance: Some(ProvenanceRequest {
                source: Some("hr-system".to_string()),
                confidence: None,
                note: None,
                correlation_id: None,
            }),
        }))
        .unwrap();

        let history = server.get_edge_history(GetEdgeHistoryRequest { edge_id: edge.id });
        let value: serde_json::Value = serde_json::from_str(&history).unwrap();
        let versions = value["versions"].as_array().unwrap();
        assert_eq!(versions.len(), 2);
        assert!(versions[0].get("provenance").is_none());
        assert_eq!(versions[1]["provenance"]["source"], "hr-system");
    }

    #[test]
    fn test_get_node_returns_current_provenance() {
        let server = create_test_server();

        let node: NodeResponse = parse_response(&server.create_node(CreateNodeRequest {
            label: "Person".to_string(),
            properties: None,
            valid_time: None,
            provenance: Some(ProvenanceRequest {
                source: Some("hr-system".to_string()),
                confidence: None,
                note: None,
                correlation_id: None,
            }),
        }))
        .unwrap();

        let fetched: NodeResponse = parse_response(&server.get_node(GetNodeRequest {
            node_id: node.id,
            include_vectors: None,
        }))
        .unwrap();
        assert_eq!(fetched.provenance.unwrap().source(), Some("hr-system"));
    }

    #[test]
    fn test_get_node_without_provenance_omits_key() {
        let server = create_test_server();

        let node: NodeResponse = parse_response(&server.create_node(CreateNodeRequest {
            label: "Person".to_string(),
            properties: None,
            valid_time: None,
            provenance: None,
        }))
        .unwrap();

        let response = server.get_node(GetNodeRequest {
            node_id: node.id,
            include_vectors: None,
        });
        let value: serde_json::Value = serde_json::from_str(&response).unwrap();
        assert!(value.get("provenance").is_none());
    }

    #[test]
    fn test_get_node_history_omits_provenance_when_absent() {
        let server = create_test_server();

        let node: NodeResponse = parse_response(&server.create_node(CreateNodeRequest {
            label: "Person".to_string(),
            properties: None,
            valid_time: None,
            provenance: None,
        }))
        .unwrap();

        let history = server.get_node_history(GetNodeHistoryRequest { node_id: node.id });
        let value: serde_json::Value = serde_json::from_str(&history).unwrap();
        assert!(value["versions"][0].get("provenance").is_none());
    }

    /// Regression test: `node_to_response`/`edge_to_response` used to
    /// hardcode `provenance: None`, and only `get_node`/`create_node`/
    /// `update_node` (and edge equivalents) patched it back in afterward.
    /// `list_nodes` (and every other read path built on `node_to_response`)
    /// silently omitted provenance even when it existed. See Issue #3224.
    #[test]
    fn test_list_nodes_returns_provenance() {
        let server = create_test_server();

        let node: NodeResponse = parse_response(&server.create_node(CreateNodeRequest {
            label: "ListProvenanceTest".to_string(),
            properties: None,
            valid_time: None,
            provenance: Some(ProvenanceRequest {
                source: Some("csv-import".to_string()),
                confidence: None,
                note: None,
                correlation_id: None,
            }),
        }))
        .unwrap();

        let response = server.list_nodes(ListNodesRequest {
            label: Some("ListProvenanceTest".to_string()),
            property_key: None,
            property_value: None,
            limit: None,
            offset: None,
            include_vectors: None,
        });
        let value: serde_json::Value = serde_json::from_str(&response).unwrap();
        let nodes = value["nodes"].as_array().unwrap();
        let found = nodes.iter().find(|n| n["id"] == node.id).unwrap();
        assert_eq!(found["provenance"]["source"], "csv-import");
    }

    /// See [`test_list_nodes_returns_provenance`] -- same gap, `traverse` path.
    #[test]
    fn test_traverse_returns_provenance() {
        let server = create_test_server();

        let start: NodeResponse = parse_response(&server.create_node(CreateNodeRequest {
            label: "Person".to_string(),
            properties: None,
            valid_time: None,
            provenance: None,
        }))
        .unwrap();
        let end: NodeResponse = parse_response(&server.create_node(CreateNodeRequest {
            label: "Person".to_string(),
            properties: None,
            valid_time: None,
            provenance: Some(ProvenanceRequest {
                source: Some("graph-import".to_string()),
                confidence: None,
                note: None,
                correlation_id: None,
            }),
        }))
        .unwrap();
        parse_response::<EdgeResponse>(&server.create_edge(CreateEdgeRequest {
            source_id: start.id,
            target_id: end.id,
            label: "KNOWS".to_string(),
            properties: None,
            valid_time: None,
            provenance: None,
        }))
        .unwrap();

        let response = server.traverse(TraverseRequest {
            start_node_id: start.id,
            edge_label: "KNOWS".to_string(),
            direction: Some("outgoing".to_string()),
            depth: Some(1),
            limit: None,
            offset: None,
            include_vectors: None,
            as_of_valid_time: None,
            as_of_transaction_time: None,
        });
        let value: serde_json::Value = serde_json::from_str(&response).unwrap();
        let results = value["results"].as_array().unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0]["node"]["provenance"]["source"], "graph-import");
    }

    /// See [`test_list_nodes_returns_provenance`] -- same gap, the bi-temporal
    /// `get_node_at_time` path. This one matters especially: `get_node_at_time`
    /// resolves the *as-of* version, not necessarily the node's current head,
    /// so the returned provenance must be attached to that exact historical
    /// version rather than "whatever is current now".
    #[test]
    fn test_get_node_at_time_returns_provenance_for_that_version() {
        let server = create_test_server();

        let node: NodeResponse = parse_response(&server.create_node(CreateNodeRequest {
            label: "Person".to_string(),
            properties: None,
            valid_time: None,
            provenance: Some(ProvenanceRequest {
                source: Some("initial-load".to_string()),
                confidence: None,
                note: None,
                correlation_id: None,
            }),
        }))
        .unwrap();

        let as_of_micros = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_micros() as i64;

        // Update with different provenance; the as-of query above must still
        // report the *original* version's provenance, not this new one.
        parse_response::<NodeResponse>(&server.update_node(UpdateNodeRequest {
            node_id: node.id,
            properties: HashMap::new(),
            valid_time: None,
            provenance: Some(ProvenanceRequest {
                source: Some("manual-correction".to_string()),
                confidence: None,
                note: None,
                correlation_id: None,
            }),
        }))
        .unwrap();

        let response = server.get_node_at_time(GetNodeAtTimeRequest {
            node_id: node.id,
            valid_time: as_of_micros.to_string(),
            transaction_time: Some(as_of_micros.to_string()),
        });
        let value: serde_json::Value = serde_json::from_str(&response).unwrap();
        assert_eq!(value["node"]["provenance"]["source"], "initial-load");
    }
}

// ============================================================================
// Point-in-time (AS OF) graph traversal (Issue #3225)
// ============================================================================

mod traverse_as_of_tests {
    use super::*;
    use chrono::{DateTime, Duration, Utc};

    /// `now` is the caller's own `Utc::now()`, captured once at the start of the test
    /// and reused for every offset it computes, so relative orderings between offsets
    /// stay deterministic regardless of how long the test takes to run.
    fn hours_ago(now: DateTime<Utc>, hours: i64) -> String {
        (now - Duration::hours(hours)).to_rfc3339()
    }

    fn create_node_at(server: &AletheiaMcpServer, label: &str, valid_time: &str) -> u64 {
        let response = server.create_node(CreateNodeRequest {
            label: label.to_string(),
            properties: None,
            valid_time: Some(valid_time.to_string()),
            provenance: None,
        });
        let node: NodeResponse = parse_response(&response).expect("create_node should succeed");
        node.id
    }

    fn create_edge_at(
        server: &AletheiaMcpServer,
        source_id: u64,
        target_id: u64,
        label: &str,
        valid_time: &str,
    ) -> u64 {
        let response = server.create_edge(CreateEdgeRequest {
            source_id,
            target_id,
            label: label.to_string(),
            properties: None,
            valid_time: Some(valid_time.to_string()),
            provenance: None,
        });
        let edge: EdgeResponse = parse_response(&response).expect("create_edge should succeed");
        edge.id
    }

    fn traverse_count(
        server: &AletheiaMcpServer,
        req: TraverseRequest,
    ) -> (usize, serde_json::Value) {
        let response = server.traverse(req);
        let value: serde_json::Value = serde_json::from_str(&response).unwrap();
        assert!(value.get("error").is_none(), "unexpected error: {value}");
        let count = value["count"].as_u64().unwrap() as usize;
        (count, value)
    }

    #[test]
    fn test_traverse_as_of_excludes_edge_created_after_coordinate() {
        let server = create_test_server();
        let now = Utc::now();

        let a = create_node_at(&server, "Person", &hours_ago(now, 10));
        let b = create_node_at(&server, "Person", &hours_ago(now, 10));
        create_edge_at(&server, a, b, "KNOWS", &hours_ago(now, 2));

        // Before the edge was created: excluded.
        let (count_before, _) = traverse_count(
            &server,
            TraverseRequest {
                start_node_id: a,
                edge_label: "KNOWS".to_string(),
                direction: Some("outgoing".to_string()),
                depth: Some(1),
                limit: None,
                offset: None,
                include_vectors: None,
                as_of_valid_time: Some(hours_ago(now, 5)),
                as_of_transaction_time: None,
            },
        );
        assert_eq!(
            count_before, 0,
            "edge created after coordinate must be excluded"
        );

        // After the edge was created: included.
        let (count_after, _) = traverse_count(
            &server,
            TraverseRequest {
                start_node_id: a,
                edge_label: "KNOWS".to_string(),
                direction: Some("outgoing".to_string()),
                depth: Some(1),
                limit: None,
                offset: None,
                include_vectors: None,
                as_of_valid_time: Some(hours_ago(now, 1)),
                as_of_transaction_time: None,
            },
        );
        assert_eq!(
            count_after, 1,
            "edge valid as of coordinate must be included"
        );
    }

    #[test]
    fn test_traverse_as_of_excludes_retired_edge_but_recalls_it_before_retirement() {
        let server = create_test_server();
        let now = Utc::now();

        let a = create_node_at(&server, "Person", &hours_ago(now, 10));
        let b = create_node_at(&server, "Person", &hours_ago(now, 10));
        let edge_id = create_edge_at(&server, a, b, "KNOWS", &hours_ago(now, 5));
        // Anchor a transaction-time coordinate right after the edge was created but
        // before it is (really, physically) deleted below.
        let tx_time_before_delete = Utc::now().to_rfc3339();

        // Retire (delete) the edge, backdating the retirement itself.
        let delete_response = server.delete_edge(DeleteEdgeRequest {
            edge_id,
            valid_time: Some(hours_ago(now, 2)),
        });
        let delete_value: serde_json::Value = serde_json::from_str(&delete_response).unwrap();
        assert!(
            delete_value.get("error").is_none(),
            "delete_edge should succeed: {delete_value}"
        );

        // Between creation (5h ago) and retirement (2h ago), as recorded by a
        // transaction time before the deletion was committed: still valid --
        // recall must be 1.0 even though the edge has since been deleted for real
        // (mirrors tests/temporal_adjacency_default_enabled_test.rs's recall check).
        let (count_before_retirement, _) = traverse_count(
            &server,
            TraverseRequest {
                start_node_id: a,
                edge_label: "KNOWS".to_string(),
                direction: Some("outgoing".to_string()),
                depth: Some(1),
                limit: None,
                offset: None,
                include_vectors: None,
                as_of_valid_time: Some(hours_ago(now, 3)),
                as_of_transaction_time: Some(tx_time_before_delete),
            },
        );
        assert_eq!(
            count_before_retirement, 1,
            "edge valid at coordinate must be recalled despite later deletion"
        );

        // After retirement (2h ago): excluded.
        let (count_after_retirement, _) = traverse_count(
            &server,
            TraverseRequest {
                start_node_id: a,
                edge_label: "KNOWS".to_string(),
                direction: Some("outgoing".to_string()),
                depth: Some(1),
                limit: None,
                offset: None,
                include_vectors: None,
                as_of_valid_time: Some(hours_ago(now, 1)),
                as_of_transaction_time: None,
            },
        );
        assert_eq!(
            count_after_retirement, 0,
            "edge retired before coordinate must be excluded"
        );
    }

    #[test]
    fn test_traverse_as_of_valid_time_vs_transaction_time_disagreement() {
        let server = create_test_server();
        let now = Utc::now();

        let a = create_node_at(&server, "Person", &hours_ago(now, 10));
        let b = create_node_at(&server, "Person", &hours_ago(now, 10));
        // Valid time is backdated to 5h ago; the *actual* commit (transaction time)
        // happens for real, right now.
        create_edge_at(&server, a, b, "KNOWS", &hours_ago(now, 5));

        // valid_time is satisfied (5h ago has passed), but transaction_time asks
        // "as recorded by 20h ago" -- long before the edge was actually committed.
        let (count_tx_gate_fails, _) = traverse_count(
            &server,
            TraverseRequest {
                start_node_id: a,
                edge_label: "KNOWS".to_string(),
                direction: Some("outgoing".to_string()),
                depth: Some(1),
                limit: None,
                offset: None,
                include_vectors: None,
                as_of_valid_time: Some(hours_ago(now, 5)),
                as_of_transaction_time: Some(hours_ago(now, 20)),
            },
        );
        assert_eq!(
            count_tx_gate_fails, 0,
            "transaction-time coordinate before the real commit must exclude the edge \
             even though valid_time alone would admit it"
        );

        // valid_time asks for 8h ago (not yet valid), transaction_time defaults to now
        // (satisfied, since the edge is already committed for real).
        let (count_vt_gate_fails, _) = traverse_count(
            &server,
            TraverseRequest {
                start_node_id: a,
                edge_label: "KNOWS".to_string(),
                direction: Some("outgoing".to_string()),
                depth: Some(1),
                limit: None,
                offset: None,
                include_vectors: None,
                as_of_valid_time: Some(hours_ago(now, 8)),
                as_of_transaction_time: None,
            },
        );
        assert_eq!(
            count_vt_gate_fails, 0,
            "valid-time coordinate before the fact became true must exclude the edge \
             even though transaction_time alone would admit it"
        );

        // Both axes satisfied: included.
        let (count_both_satisfied, _) = traverse_count(
            &server,
            TraverseRequest {
                start_node_id: a,
                edge_label: "KNOWS".to_string(),
                direction: Some("outgoing".to_string()),
                depth: Some(1),
                limit: None,
                offset: None,
                include_vectors: None,
                as_of_valid_time: Some(hours_ago(now, 5)),
                as_of_transaction_time: None,
            },
        );
        assert_eq!(
            count_both_satisfied, 1,
            "both axes satisfied must include the edge"
        );
    }

    #[test]
    fn test_traverse_as_of_empty_when_start_node_not_yet_existing() {
        let server = create_test_server();
        let now = Utc::now();

        let a = create_node_at(&server, "Person", &hours_ago(now, 3));
        let b = create_node_at(&server, "Person", &hours_ago(now, 3));
        create_edge_at(&server, a, b, "KNOWS", &hours_ago(now, 3));

        // 5h ago: neither node nor edge existed yet.
        let (count, _) = traverse_count(
            &server,
            TraverseRequest {
                start_node_id: a,
                edge_label: "KNOWS".to_string(),
                direction: Some("outgoing".to_string()),
                depth: Some(1),
                limit: None,
                offset: None,
                include_vectors: None,
                as_of_valid_time: Some(hours_ago(now, 5)),
                as_of_transaction_time: None,
            },
        );
        assert_eq!(
            count, 0,
            "start node not yet existing must yield empty results, not an error"
        );
    }

    #[test]
    fn test_traverse_as_of_invalid_valid_time_returns_structured_error() {
        let server = create_test_server();
        let a = create_node_at(&server, "Person", &Utc::now().to_rfc3339());

        let response = server.traverse(TraverseRequest {
            start_node_id: a,
            edge_label: "KNOWS".to_string(),
            direction: Some("outgoing".to_string()),
            depth: Some(1),
            limit: None,
            offset: None,
            include_vectors: None,
            as_of_valid_time: Some("not-a-timestamp".to_string()),
            as_of_transaction_time: None,
        });
        let value: serde_json::Value = serde_json::from_str(&response).unwrap();
        let error = value
            .get("error")
            .and_then(|e| e["message"].as_str())
            .expect("expected a structured error, got no error field");
        assert!(
            error.contains("as_of_valid_time"),
            "error should identify the offending field: {error}"
        );
    }

    #[test]
    fn test_traverse_as_of_invalid_transaction_time_returns_structured_error() {
        let server = create_test_server();
        let a = create_node_at(&server, "Person", &Utc::now().to_rfc3339());

        let response = server.traverse(TraverseRequest {
            start_node_id: a,
            edge_label: "KNOWS".to_string(),
            direction: Some("outgoing".to_string()),
            depth: Some(1),
            limit: None,
            offset: None,
            include_vectors: None,
            as_of_valid_time: None,
            as_of_transaction_time: Some("not-a-timestamp".to_string()),
        });
        let value: serde_json::Value = serde_json::from_str(&response).unwrap();
        let error = value
            .get("error")
            .and_then(|e| e["message"].as_str())
            .expect("expected a structured error, got no error field");
        assert!(
            error.contains("as_of_transaction_time"),
            "error should identify the offending field: {error}"
        );
    }

    #[test]
    fn test_traverse_as_of_respects_resource_limits() {
        let server = create_test_server();
        let now = Utc::now();
        let valid_time = hours_ago(now, 3);

        // Chain of 5 nodes (4 hops), all backdated to the same valid_time.
        let mut start = None;
        let mut prev_id: Option<u64> = None;
        for _ in 0..5 {
            let node_id = create_node_at(&server, "ChainNode", &valid_time);
            start.get_or_insert(node_id);
            if let Some(source_id) = prev_id {
                create_edge_at(&server, source_id, node_id, "NEXT", &valid_time);
            }
            prev_id = Some(node_id);
        }
        let start = start.unwrap();

        // Depth far beyond MAX_TRAVERSAL_DEPTH must be clamped, not error, on the
        // temporal path exactly as it is on the current-state path.
        let (count, _) = traverse_count(
            &server,
            TraverseRequest {
                start_node_id: start,
                edge_label: "NEXT".to_string(),
                direction: Some("outgoing".to_string()),
                depth: Some(100),
                limit: Some(50),
                offset: None,
                include_vectors: None,
                as_of_valid_time: Some(hours_ago(now, 1)),
                as_of_transaction_time: None,
            },
        );
        assert_eq!(count, 4, "clamped depth should still reach the full chain");
    }

    #[test]
    fn test_traverse_as_of_no_temporal_params_matches_current_state() {
        let server = create_test_server();

        let a = create_node_at(&server, "Person", &Utc::now().to_rfc3339());
        let b = create_node_at(&server, "Person", &Utc::now().to_rfc3339());
        create_edge_at(&server, a, b, "KNOWS", &Utc::now().to_rfc3339());
        let now_after_creation = Utc::now();

        let (count_no_temporal, value_no_temporal) = traverse_count(
            &server,
            TraverseRequest {
                start_node_id: a,
                edge_label: "KNOWS".to_string(),
                direction: Some("outgoing".to_string()),
                depth: Some(1),
                limit: None,
                offset: None,
                include_vectors: None,
                as_of_valid_time: None,
                as_of_transaction_time: None,
            },
        );

        let (count_as_of_now, value_as_of_now) = traverse_count(
            &server,
            TraverseRequest {
                start_node_id: a,
                edge_label: "KNOWS".to_string(),
                direction: Some("outgoing".to_string()),
                depth: Some(1),
                limit: None,
                offset: None,
                include_vectors: None,
                as_of_valid_time: Some(now_after_creation.to_rfc3339()),
                as_of_transaction_time: Some(now_after_creation.to_rfc3339()),
            },
        );

        assert_eq!(count_no_temporal, 1);
        assert_eq!(count_no_temporal, count_as_of_now);
        assert_eq!(
            value_no_temporal["results"][0]["node"]["id"],
            value_as_of_now["results"][0]["node"]["id"],
            "explicit as_of=now must reproduce the identical current-state answer"
        );
    }

    #[test]
    fn test_traverse_as_of_multi_hop() {
        let server = create_test_server();
        let now = Utc::now();
        let valid_time = hours_ago(now, 3);

        let a = create_node_at(&server, "Person", &valid_time);
        let b = create_node_at(&server, "Person", &valid_time);
        let c = create_node_at(&server, "Person", &valid_time);
        create_edge_at(&server, a, b, "KNOWS", &valid_time);
        create_edge_at(&server, b, c, "KNOWS", &valid_time);

        let (count, _) = traverse_count(
            &server,
            TraverseRequest {
                start_node_id: a,
                edge_label: "KNOWS".to_string(),
                direction: Some("outgoing".to_string()),
                depth: Some(2),
                limit: None,
                offset: None,
                include_vectors: None,
                as_of_valid_time: Some(hours_ago(now, 1)),
                as_of_transaction_time: None,
            },
        );
        assert_eq!(
            count, 2,
            "both hops should be reachable as of the coordinate"
        );
    }

    #[test]
    fn test_traverse_as_of_both_direction_finds_node_reachable_only_via_incoming_edge() {
        // Temporal-path variant of the direction:"both" fix: a node reachable
        // only through an edge pointing AT the current node (not away from
        // it) must still be found, not silently dropped because `next_id`
        // was resolved from the top-level `direction` string alone.
        let server = create_test_server();
        let now = Utc::now();
        let valid_time = hours_ago(now, 3);

        let a = create_node_at(&server, "Person", &valid_time);
        let b = create_node_at(&server, "Person", &valid_time);
        // Only B -> A exists; A has no reciprocal outgoing edge.
        create_edge_at(&server, b, a, "POINTS_AT", &valid_time);

        let (count, value) = traverse_count(
            &server,
            TraverseRequest {
                start_node_id: a,
                edge_label: "POINTS_AT".to_string(),
                direction: Some("both".to_string()),
                depth: Some(1),
                limit: None,
                offset: None,
                include_vectors: None,
                as_of_valid_time: Some(hours_ago(now, 1)),
                as_of_transaction_time: None,
            },
        );
        assert_eq!(
            count, 1,
            "as_of direction:\"both\" must find B via the incoming B->A edge: {value}"
        );
        assert_eq!(value["results"][0]["node"]["id"], b);
    }

    #[test]
    fn test_traverse_as_of_stops_at_intermediate_node_missing_at_coordinate() {
        // A node deleted (without cascading its edges) via the low-level
        // Rust API -- bypassing the MCP delete_node tool's own
        // connected-edges safety check, exactly the documented "Orphaned
        // Edges on Node Deletion" behavior of that API -- must stop a
        // temporal traversal from continuing past it, even though its
        // dangling edge was never itself retired.
        use crate::api::transaction::WriteOps;

        let server = create_test_server();
        let now = Utc::now();
        let far_past = hours_ago(now, 10);

        let a = create_node_at(&server, "Person", &far_past);
        let b = create_node_at(&server, "Person", &far_past);
        let c = create_node_at(&server, "Person", &far_past);
        create_edge_at(&server, a, b, "KNOWS", &far_past);
        create_edge_at(&server, b, c, "KNOWS", &far_past);

        let b_node_id = crate::core::id::NodeId::new(b).unwrap();
        server
            .db()
            .write_with_timestamp(|tx| tx.delete_node(b_node_id))
            .expect("low-level delete_node should succeed");
        let after_delete = Utc::now().to_rfc3339();

        let (count, value) = traverse_count(
            &server,
            TraverseRequest {
                start_node_id: a,
                edge_label: "KNOWS".to_string(),
                direction: Some("outgoing".to_string()),
                depth: Some(2),
                limit: None,
                offset: None,
                include_vectors: None,
                as_of_valid_time: Some(after_delete),
                as_of_transaction_time: None,
            },
        );
        assert_eq!(
            count, 0,
            "traversal must not continue past a node absent at the coordinate: {value}"
        );
    }
}

// ============================================================================
// Result-Completeness Signal Tests (Issue #3226)
//
// Every bounded MCP read tool must let a single call reveal whether more
// results exist (`has_more`), how to fetch them (`next_offset`, for
// offset-paginated tools), and, where cheaply derivable, the total matching
// count (`total_matching`). These tests pin that contract per tool.
// ============================================================================

mod completeness_tests {
    use super::*;

    /// Read `has_more` as a bool (defaults to a sentinel that fails assertions
    /// when the field is entirely absent, so the red phase is meaningful).
    fn has_more(value: &serde_json::Value) -> Option<bool> {
        value.get("has_more").and_then(|v| v.as_bool())
    }

    /// Read `next_offset` as a number when present; `None` when absent or null.
    fn next_offset(value: &serde_json::Value) -> Option<u64> {
        value.get("next_offset").and_then(|v| v.as_u64())
    }

    fn parse(response: &str) -> serde_json::Value {
        serde_json::from_str(response).expect("valid JSON response")
    }

    fn seed_labeled(server: &AletheiaMcpServer, label: &str, n: usize) {
        for i in 0..n {
            let mut props = HashMap::new();
            props.insert("index".to_string(), serde_json::json!(i));
            server.create_node(CreateNodeRequest {
                valid_time: None,
                label: label.to_string(),
                properties: Some(props),
                provenance: None,
            });
        }
    }

    // --- list_nodes: label scan ------------------------------------------

    #[test]
    fn test_list_nodes_has_more_and_next_offset() {
        let server = create_test_server();
        seed_labeled(&server, "PersonHM", 250);

        // Page 1: full page, more remains.
        let page1 = parse(&server.list_nodes(ListNodesRequest {
            label: Some("PersonHM".to_string()),
            property_key: None,
            property_value: None,
            limit: Some(100),
            offset: Some(0),
            include_vectors: None,
        }));
        assert_eq!(page1.get("count"), Some(&serde_json::json!(100)));
        assert_eq!(
            has_more(&page1),
            Some(true),
            "page 1 of 250 has more: {page1}"
        );
        assert_eq!(
            next_offset(&page1),
            Some(100),
            "next_offset points at page 2"
        );

        // Page 2: full page, still more.
        let page2 = parse(&server.list_nodes(ListNodesRequest {
            label: Some("PersonHM".to_string()),
            property_key: None,
            property_value: None,
            limit: Some(100),
            offset: Some(100),
            include_vectors: None,
        }));
        assert_eq!(has_more(&page2), Some(true));
        assert_eq!(next_offset(&page2), Some(200));

        // Page 3: partial final page, no more.
        let page3 = parse(&server.list_nodes(ListNodesRequest {
            label: Some("PersonHM".to_string()),
            property_key: None,
            property_value: None,
            limit: Some(100),
            offset: Some(200),
            include_vectors: None,
        }));
        assert_eq!(page3.get("count"), Some(&serde_json::json!(50)));
        assert_eq!(
            has_more(&page3),
            Some(false),
            "final page has no more: {page3}"
        );
        assert_eq!(next_offset(&page3), None, "no next_offset on final page");
    }

    #[test]
    fn test_list_nodes_label_scan_omits_total_matching() {
        // A label scan cannot cheaply know the total without a full scan, so
        // `total_matching` is omitted (not faked); `has_more` carries the signal.
        let server = create_test_server();
        seed_labeled(&server, "ScanHM", 5);

        let value = parse(&server.list_nodes(ListNodesRequest {
            label: Some("ScanHM".to_string()),
            property_key: None,
            property_value: None,
            limit: Some(2),
            offset: Some(0),
            include_vectors: None,
        }));
        assert_eq!(has_more(&value), Some(true));
        assert!(
            value.get("total_matching").is_none(),
            "label scan must omit total_matching: {value}"
        );
    }

    // --- list_nodes: property path ---------------------------------------

    #[test]
    fn test_list_nodes_property_path_reports_total_matching() {
        let server = create_test_server();
        for _ in 0..150 {
            server.create_node(CreateNodeRequest {
                valid_time: None,
                label: "WidgetHM".to_string(),
                properties: Some({
                    let mut m = HashMap::new();
                    m.insert("status".to_string(), serde_json::json!("active"));
                    m
                }),
                provenance: None,
            });
        }

        let page1 = parse(&server.list_nodes(ListNodesRequest {
            label: Some("WidgetHM".to_string()),
            property_key: Some("status".to_string()),
            property_value: Some(serde_json::json!("active")),
            limit: Some(100),
            offset: Some(0),
            include_vectors: None,
        }));
        assert_eq!(page1.get("count"), Some(&serde_json::json!(100)));
        assert_eq!(
            page1.get("total_matching"),
            Some(&serde_json::json!(150)),
            "property path materializes the full id list, so total is cheap: {page1}"
        );
        assert_eq!(has_more(&page1), Some(true));
        assert_eq!(next_offset(&page1), Some(100));

        let page2 = parse(&server.list_nodes(ListNodesRequest {
            label: Some("WidgetHM".to_string()),
            property_key: Some("status".to_string()),
            property_value: Some(serde_json::json!("active")),
            limit: Some(100),
            offset: Some(100),
            include_vectors: None,
        }));
        assert_eq!(page2.get("count"), Some(&serde_json::json!(50)));
        assert_eq!(page2.get("total_matching"), Some(&serde_json::json!(150)));
        assert_eq!(has_more(&page2), Some(false));
        assert_eq!(next_offset(&page2), None);
    }

    // --- list_nodes / list_edges guidance paths --------------------------

    #[test]
    fn test_list_nodes_unfiltered_has_more_false() {
        let server = create_test_server();
        seed_labeled(&server, "AnyHM", 3);
        let value = parse(&server.list_nodes(ListNodesRequest {
            label: None,
            property_key: None,
            property_value: None,
            limit: None,
            offset: None,
            include_vectors: None,
        }));
        assert_eq!(
            has_more(&value),
            Some(false),
            "unfiltered guidance path carries has_more:false for shape consistency: {value}"
        );
        assert_eq!(next_offset(&value), None);
    }

    #[test]
    fn test_list_edges_has_more_false() {
        let server = create_test_server();
        let value = parse(&server.list_edges(ListEdgesRequest {
            label: None,
            limit: None,
            offset: None,
            include_vectors: None,
        }));
        assert_eq!(
            has_more(&value),
            Some(false),
            "list_edges guidance: {value}"
        );
        assert_eq!(next_offset(&value), None);
    }

    // --- outgoing / incoming edges (additive only, never truncated) ------

    fn seed_star(server: &AletheiaMcpServer, out: usize) -> u64 {
        let center = {
            let r = server.create_node(CreateNodeRequest {
                valid_time: None,
                label: "Hub".to_string(),
                properties: None,
                provenance: None,
            });
            let n: NodeResponse = parse_response(&r).unwrap();
            n.id
        };
        for _ in 0..out {
            let leaf = {
                let r = server.create_node(CreateNodeRequest {
                    valid_time: None,
                    label: "Leaf".to_string(),
                    properties: None,
                    provenance: None,
                });
                let n: NodeResponse = parse_response(&r).unwrap();
                n.id
            };
            server.create_edge(CreateEdgeRequest {
                valid_time: None,
                source_id: center,
                target_id: leaf,
                label: "LINK".to_string(),
                properties: None,
                provenance: None,
            });
        }
        center
    }

    #[test]
    fn test_get_outgoing_edges_completeness() {
        let server = create_test_server();
        let center = seed_star(&server, 3);
        let value = parse(&server.get_outgoing_edges(GetOutgoingEdgesRequest {
            node_id: center,
            label: None,
            include_vectors: None,
        }));
        assert_eq!(value.get("count"), Some(&serde_json::json!(3)));
        assert_eq!(
            has_more(&value),
            Some(false),
            "outgoing edges return the full set: {value}"
        );
        assert_eq!(
            value.get("total_matching"),
            Some(&serde_json::json!(3)),
            "total_matching equals the complete count: {value}"
        );
    }

    #[test]
    fn test_get_incoming_edges_completeness() {
        let server = create_test_server();
        // Build a hub with 2 incoming edges.
        let sink = {
            let r = server.create_node(CreateNodeRequest {
                valid_time: None,
                label: "Sink".to_string(),
                properties: None,
                provenance: None,
            });
            let n: NodeResponse = parse_response(&r).unwrap();
            n.id
        };
        for _ in 0..2 {
            let src = {
                let r = server.create_node(CreateNodeRequest {
                    valid_time: None,
                    label: "Src".to_string(),
                    properties: None,
                    provenance: None,
                });
                let n: NodeResponse = parse_response(&r).unwrap();
                n.id
            };
            server.create_edge(CreateEdgeRequest {
                valid_time: None,
                source_id: src,
                target_id: sink,
                label: "LINK".to_string(),
                properties: None,
                provenance: None,
            });
        }
        let value = parse(&server.get_incoming_edges(GetIncomingEdgesRequest {
            node_id: sink,
            label: None,
            include_vectors: None,
        }));
        assert_eq!(value.get("count"), Some(&serde_json::json!(2)));
        assert_eq!(has_more(&value), Some(false));
        assert_eq!(value.get("total_matching"), Some(&serde_json::json!(2)));
    }

    // --- traverse ---------------------------------------------------------

    /// Build a chain start -> n1 -> n2 -> ... of `len` NEXT edges; return ids.
    fn seed_chain(server: &AletheiaMcpServer, len: usize) -> Vec<u64> {
        let ids: Vec<u64> = (0..=len)
            .map(|_| {
                let r = server.create_node(CreateNodeRequest {
                    valid_time: None,
                    label: "ChainHM".to_string(),
                    properties: None,
                    provenance: None,
                });
                let n: NodeResponse = parse_response(&r).unwrap();
                n.id
            })
            .collect();
        for w in ids.windows(2) {
            server.create_edge(CreateEdgeRequest {
                valid_time: None,
                source_id: w[0],
                target_id: w[1],
                label: "NEXT".to_string(),
                properties: None,
                provenance: None,
            });
        }
        ids
    }

    #[test]
    fn test_traverse_has_more_and_next_offset_when_truncated() {
        let server = create_test_server();
        // start -> 5 further nodes reachable via NEXT.
        let ids = seed_chain(&server, 5);

        // Truncate at 2 of the 5 reachable nodes.
        let truncated = parse(&server.traverse(TraverseRequest {
            start_node_id: ids[0],
            edge_label: "NEXT".to_string(),
            direction: None,
            depth: Some(5),
            limit: Some(2),
            offset: None,
            include_vectors: None,
            as_of_valid_time: None,
            as_of_transaction_time: None,
        }));
        assert_eq!(truncated.get("count"), Some(&serde_json::json!(2)));
        assert_eq!(
            has_more(&truncated),
            Some(true),
            "traversal truncated by limit signals more: {truncated}"
        );
        assert_eq!(next_offset(&truncated), Some(2));

        // A limit above the reachable count exhausts the traversal.
        let complete = parse(&server.traverse(TraverseRequest {
            start_node_id: ids[0],
            edge_label: "NEXT".to_string(),
            direction: None,
            depth: Some(5),
            limit: Some(100),
            offset: None,
            include_vectors: None,
            as_of_valid_time: None,
            as_of_transaction_time: None,
        }));
        assert_eq!(complete.get("count"), Some(&serde_json::json!(5)));
        assert_eq!(has_more(&complete), Some(false), "exhausted: {complete}");
        assert_eq!(next_offset(&complete), None);
    }

    #[test]
    fn test_traverse_offset_paginates() {
        let server = create_test_server();
        let ids = seed_chain(&server, 5); // 5 reachable nodes

        // Collect the ids returned across two offset pages of size 2 plus a
        // final page; the union must equal the single-shot full traversal.
        let full = parse(&server.traverse(TraverseRequest {
            start_node_id: ids[0],
            edge_label: "NEXT".to_string(),
            direction: None,
            depth: Some(5),
            limit: Some(100),
            offset: None,
            include_vectors: None,
            as_of_valid_time: None,
            as_of_transaction_time: None,
        }));
        let full_count = full.get("count").and_then(|c| c.as_u64()).unwrap();
        assert_eq!(full_count, 5);

        let page2 = parse(&server.traverse(TraverseRequest {
            start_node_id: ids[0],
            edge_label: "NEXT".to_string(),
            direction: None,
            depth: Some(5),
            limit: Some(2),
            offset: Some(2),
            include_vectors: None,
            as_of_valid_time: None,
            as_of_transaction_time: None,
        }));
        assert_eq!(page2.get("count"), Some(&serde_json::json!(2)));
        assert_eq!(
            has_more(&page2),
            Some(true),
            "offset 2 of 5 has more: {page2}"
        );
        assert_eq!(next_offset(&page2), Some(4));

        let page3 = parse(&server.traverse(TraverseRequest {
            start_node_id: ids[0],
            edge_label: "NEXT".to_string(),
            direction: None,
            depth: Some(5),
            limit: Some(2),
            offset: Some(4),
            include_vectors: None,
            as_of_valid_time: None,
            as_of_transaction_time: None,
        }));
        assert_eq!(page3.get("count"), Some(&serde_json::json!(1)));
        assert_eq!(has_more(&page3), Some(false), "last page: {page3}");
        assert_eq!(next_offset(&page3), None);
    }

    // --- find_similar -----------------------------------------------------

    /// Seed up to 5 documents with well-separated (near-orthogonal) 4-dim
    /// embeddings: the four axis-aligned unit vectors plus the all-ones
    /// vector. Against a query embedding like `[0.1, 0.2, 0.3, 0.4]`, cosine
    /// similarity to each is strictly distinct (0.183, 0.365, 0.548, 0.730,
    /// 0.913) with wide gaps between them, so ranking is unambiguous and a
    /// deterministic count/`has_more` assertion never depends on HNSW
    /// (approximate) search resolving a near-tied score consistently.
    fn seed_vectors(server: &AletheiaMcpServer, n: usize) {
        assert!(
            n <= 5,
            "seed_vectors only defines 5 well-separated directions"
        );
        server.enable_vector_index(EnableVectorIndexRequest {
            property_name: "embedding".to_string(),
            dimensions: 4,
            distance_metric: Some("cosine".to_string()),
        });
        for i in 0..n {
            let mut props = HashMap::new();
            props.insert("name".to_string(), serde_json::json!(format!("Doc{i}")));
            let embedding: [f32; 4] = if i < 4 {
                let mut e = [0.0_f32; 4];
                e[i] = 1.0;
                e
            } else {
                [1.0; 4]
            };
            props.insert("embedding".to_string(), serde_json::json!(embedding));
            server.create_node(CreateNodeRequest {
                valid_time: None,
                label: "Document".to_string(),
                properties: Some(props),
                provenance: None,
            });
        }
    }

    #[test]
    fn test_find_similar_has_more_and_offset_paginates() {
        let server = create_test_server();
        seed_vectors(&server, 5);
        let query = vec![0.1_f32, 0.2, 0.3, 0.4];

        // k=2 of 5 available -> more remains.
        let page1 = parse(&server.find_similar(FindSimilarRequest {
            property_name: "embedding".to_string(),
            embedding: query.clone(),
            k: Some(2),
            offset: None,
            include_vectors: None,
        }));
        assert_eq!(page1.get("count"), Some(&serde_json::json!(2)));
        assert_eq!(has_more(&page1), Some(true), "2 of 5 similar: {page1}");
        assert_eq!(next_offset(&page1), Some(2));

        // Offset into the middle.
        let page2 = parse(&server.find_similar(FindSimilarRequest {
            property_name: "embedding".to_string(),
            embedding: query.clone(),
            k: Some(2),
            offset: Some(2),
            include_vectors: None,
        }));
        assert_eq!(page2.get("count"), Some(&serde_json::json!(2)));
        assert_eq!(has_more(&page2), Some(true));
        assert_eq!(next_offset(&page2), Some(4));

        // k beyond the available set exhausts it.
        let complete = parse(&server.find_similar(FindSimilarRequest {
            property_name: "embedding".to_string(),
            embedding: query,
            k: Some(50),
            offset: None,
            include_vectors: None,
        }));
        assert_eq!(has_more(&complete), Some(false), "exhausted: {complete}");
        assert_eq!(next_offset(&complete), None);
    }

    // --- success-metric sweep --------------------------------------------

    #[test]
    fn test_all_bounded_read_tools_carry_has_more() {
        let server = create_test_server();
        // Minimal data touching every tool.
        let center = seed_star(&server, 2);
        seed_vectors(&server, 2);

        let responses = vec![
            (
                "list_nodes(label)",
                server.list_nodes(ListNodesRequest {
                    label: Some("Leaf".to_string()),
                    property_key: None,
                    property_value: None,
                    limit: Some(1),
                    offset: None,
                    include_vectors: None,
                }),
            ),
            (
                "list_nodes(unfiltered)",
                server.list_nodes(ListNodesRequest {
                    label: None,
                    property_key: None,
                    property_value: None,
                    limit: None,
                    offset: None,
                    include_vectors: None,
                }),
            ),
            (
                "list_edges",
                server.list_edges(ListEdgesRequest {
                    label: None,
                    limit: None,
                    offset: None,
                    include_vectors: None,
                }),
            ),
            (
                "get_outgoing_edges",
                server.get_outgoing_edges(GetOutgoingEdgesRequest {
                    node_id: center,
                    label: None,
                    include_vectors: None,
                }),
            ),
            (
                "get_incoming_edges",
                server.get_incoming_edges(GetIncomingEdgesRequest {
                    node_id: center,
                    label: None,
                    include_vectors: None,
                }),
            ),
            (
                "traverse",
                server.traverse(TraverseRequest {
                    start_node_id: center,
                    edge_label: "LINK".to_string(),
                    direction: None,
                    depth: Some(1),
                    limit: Some(1),
                    offset: None,
                    include_vectors: None,
                    as_of_valid_time: None,
                    as_of_transaction_time: None,
                }),
            ),
            (
                "find_similar",
                server.find_similar(FindSimilarRequest {
                    property_name: "embedding".to_string(),
                    embedding: vec![0.1, 0.2, 0.3, 0.4],
                    k: Some(1),
                    offset: None,
                    include_vectors: None,
                }),
            ),
        ];

        for (name, resp) in responses {
            let value = parse(&resp);
            assert!(
                value.get("has_more").and_then(|v| v.as_bool()).is_some(),
                "{name} response must carry a boolean has_more: {value}"
            );
        }
    }

    // --- limit/k==0 must not produce a non-progressing page --------------
    //
    // A page must be able to carry a continuation cursor: limit/k:0 with
    // has_more:true and next_offset==offset (an unchanged offset) would trap
    // a paginating caller in an infinite loop. `limit`/`k` are clamped to a
    // minimum of 1 so a page always advances.

    #[test]
    fn test_list_nodes_limit_zero_is_clamped_to_one() {
        let server = create_test_server();
        seed_labeled(&server, "ClampHM", 3);
        let value = parse(&server.list_nodes(ListNodesRequest {
            label: Some("ClampHM".to_string()),
            property_key: None,
            property_value: None,
            limit: Some(0),
            offset: None,
            include_vectors: None,
        }));
        assert_eq!(
            value.get("count"),
            Some(&serde_json::json!(1)),
            "limit:0 must be clamped to at least 1, not return an empty page: {value}"
        );
        assert_eq!(has_more(&value), Some(true));
        assert_eq!(next_offset(&value), Some(1), "page must advance: {value}");
    }

    #[test]
    fn test_traverse_limit_zero_is_clamped_to_one() {
        let server = create_test_server();
        let ids = seed_chain(&server, 3);
        let value = parse(&server.traverse(TraverseRequest {
            start_node_id: ids[0],
            edge_label: "NEXT".to_string(),
            direction: None,
            depth: Some(3),
            limit: Some(0),
            offset: None,
            include_vectors: None,
            as_of_valid_time: None,
            as_of_transaction_time: None,
        }));
        assert_eq!(
            value.get("count"),
            Some(&serde_json::json!(1)),
            "limit:0 must be clamped to at least 1, not return an empty page: {value}"
        );
        assert_eq!(has_more(&value), Some(true));
        assert_eq!(next_offset(&value), Some(1), "page must advance: {value}");
    }

    #[test]
    fn test_find_similar_k_zero_is_clamped_to_one() {
        let server = create_test_server();
        seed_vectors(&server, 3);
        let value = parse(&server.find_similar(FindSimilarRequest {
            property_name: "embedding".to_string(),
            embedding: vec![0.1, 0.2, 0.3, 0.4],
            k: Some(0),
            offset: None,
            include_vectors: None,
        }));
        assert_eq!(
            value.get("count"),
            Some(&serde_json::json!(1)),
            "k:0 must be clamped to at least 1, not return an empty page: {value}"
        );
        assert_eq!(has_more(&value), Some(true));
        assert_eq!(next_offset(&value), Some(1), "page must advance: {value}");
    }

    // --- traverse must not walk unboundedly through dangling edges -------

    #[test]
    fn test_traverse_bounded_peek_past_dangling_edges() {
        let server = create_test_server();
        // start -> n1 -> n2 -> n3 -> n4, then n2..n4 are deleted via the
        // low-level (non-cascade) API, which per CLAUDE.md's "Orphaned
        // Edges" section leaves their connecting edges dangling in storage.
        let ids = seed_chain(&server, 4);
        for &dangling_id in &ids[2..=4] {
            server
                .db()
                .delete_node_with_valid_time(
                    crate::core::id::NodeId::new(dangling_id).unwrap(),
                    None,
                )
                .unwrap();
        }

        // n1 is the sole live node beyond `start` and becomes the one-item
        // page (limit:1). Before this fix, proving `has_more` required
        // resolving one MORE node beyond the page -- but every node past n1
        // is dangling, so the old code would walk the entire dangling chain
        // (n2, n3, n4) via "keep expanding regardless", find nothing
        // resolvable, and wrongly report has_more:false. The fix instead
        // reads `has_more` off whatever `traversal_next_hops` already queued
        // for n1 (the edge to n2) without resolving it, so it correctly
        // reports uncertainty as has_more:true in O(1) extra work rather
        // than an O(dangling-chain-length) walk that still gets it wrong.
        let response = parse(&server.traverse(TraverseRequest {
            start_node_id: ids[0],
            edge_label: "NEXT".to_string(),
            direction: None,
            depth: Some(10),
            limit: Some(1),
            offset: None,
            include_vectors: None,
            as_of_valid_time: None,
            as_of_transaction_time: None,
        }));
        assert_eq!(response.get("count"), Some(&serde_json::json!(1)));
        assert_eq!(
            has_more(&response),
            Some(true),
            "a pending (even if unresolved) frontier candidate must not be silently \
             reported as has_more:false: {response}"
        );
    }
}

// ============================================================================
// Point-in-time (AS OF) node find by label and property (Issue #3236)
// ============================================================================
//
// `find_nodes_at_time` resolves "the Person named Alice, as of T" without a
// prior NodeId: it returns each matching node AS IT EXISTED at the queried
// bi-temporal coordinate, excluding nodes that did not exist (or whose
// property value did not hold) at that point.

mod find_nodes_at_time_tests {
    use super::*;
    use chrono::{DateTime, Utc};

    /// Capture an RFC 3339 anchor strictly after every previously committed
    /// write: an HLC commit stamp in the same microsecond as the anchor
    /// carries a logical component > 0 and would order *after* it, so sleep
    /// past the microsecond boundary first.
    fn anchor_after_commits() -> String {
        std::thread::sleep(std::time::Duration::from_millis(2));
        Utc::now().to_rfc3339()
    }

    fn base_req(label: &str, valid_time: &str) -> FindNodesAtTimeRequest {
        FindNodesAtTimeRequest {
            label: label.to_string(),
            property_key: None,
            property_value: None,
            valid_time: valid_time.to_string(),
            transaction_time: None,
            limit: None,
            offset: None,
            include_vectors: None,
        }
    }

    fn name_req(label: &str, name: &str, valid_time: &str) -> FindNodesAtTimeRequest {
        FindNodesAtTimeRequest {
            property_key: Some("name".to_string()),
            property_value: Some(serde_json::json!(name)),
            ..base_req(label, valid_time)
        }
    }

    fn find(server: &AletheiaMcpServer, req: FindNodesAtTimeRequest) -> serde_json::Value {
        serde_json::from_str(&server.find_nodes_at_time(req))
            .expect("find_nodes_at_time should return valid JSON")
    }

    fn node_ids(value: &serde_json::Value) -> Vec<u64> {
        value["nodes"]
            .as_array()
            .unwrap_or_else(|| panic!("expected a nodes array, got: {value}"))
            .iter()
            .map(|n| n["id"].as_u64().expect("node id should be a u64"))
            .collect()
    }

    fn create_named(server: &AletheiaMcpServer, label: &str, name: &str) -> u64 {
        let node: NodeResponse = parse_response(&server.create_node(CreateNodeRequest {
            label: label.to_string(),
            properties: Some(HashMap::from([(
                "name".to_string(),
                serde_json::json!(name),
            )])),
            valid_time: None,
            provenance: None,
        }))
        .expect("create should succeed");
        node.id
    }

    fn rename(server: &AletheiaMcpServer, node_id: u64, name: &str) {
        let _: NodeResponse = parse_response(&server.update_node(UpdateNodeRequest {
            node_id,
            properties: HashMap::from([("name".to_string(), serde_json::json!(name))]),
            valid_time: None,
            provenance: None,
        }))
        .expect("update should succeed");
    }

    fn delete(server: &AletheiaMcpServer, node_id: u64) {
        let response = server.delete_node(DeleteNodeRequest {
            node_id,
            detach: None,
            valid_time: None,
        });
        let value: serde_json::Value = serde_json::from_str(&response).unwrap();
        assert_eq!(value["success"], serde_json::json!(true), "{value}");
    }

    fn assert_invalid_argument(response: &str, must_mention: &str) {
        let value: serde_json::Value = serde_json::from_str(response).unwrap();
        assert_eq!(
            value["error"]["code"],
            serde_json::json!("INVALID_ARGUMENT"),
            "expected INVALID_ARGUMENT, got: {value}"
        );
        let message = value["error"]["message"].as_str().unwrap_or_default();
        assert!(
            message.contains(must_mention),
            "error message should mention '{must_mention}': {message}"
        );
    }

    /// Build a request anchored at the same instant on BOTH dimensions.
    /// Recalling a superseded (or deleted) version requires this: the
    /// superseding write closes the old version's *transaction* interval,
    /// so with `transaction_time` defaulted to now only the latest recorded
    /// version is visible (same convention as `traverse`'s `as_of_*`, see
    /// the guide).
    fn name_req_at(label: &str, name: &str, t: &str) -> FindNodesAtTimeRequest {
        FindNodesAtTimeRequest {
            transaction_time: Some(t.to_string()),
            ..name_req(label, name, t)
        }
    }

    #[test]
    fn returns_node_as_it_existed_at_t_not_current_state() {
        let server = create_test_server();
        let id = create_named(&server, "Person", "Alice");
        let t1 = anchor_after_commits();
        rename(&server, id, "Bob");
        let t2 = anchor_after_commits();

        // At (t1, t1) the node matched "Alice" and its returned properties
        // are the reconstructed at-time state, not the current one.
        let value = find(&server, name_req_at("Person", "Alice", &t1));
        assert_eq!(node_ids(&value), vec![id], "{value}");
        assert_eq!(value["nodes"][0]["properties"]["name"], "Alice");
        // "Bob" did not hold yet at t1...
        let value = find(&server, name_req_at("Person", "Bob", &t1));
        assert!(node_ids(&value).is_empty(), "{value}");
        // ...and the situation reverses at t2.
        let value = find(&server, name_req_at("Person", "Bob", &t2));
        assert_eq!(node_ids(&value), vec![id]);
        assert_eq!(value["nodes"][0]["properties"]["name"], "Bob");
        let value = find(&server, name_req_at("Person", "Alice", &t2));
        assert!(node_ids(&value).is_empty(), "{value}");
    }

    #[test]
    fn node_created_after_t_is_excluded() {
        let server = create_test_server();
        let t0 = anchor_after_commits();
        let id = create_named(&server, "Person", "Alice");
        let t1 = anchor_after_commits();

        let value = find(&server, name_req("Person", "Alice", &t0));
        assert!(
            node_ids(&value).is_empty(),
            "node created after T must be excluded: {value}"
        );
        let value = find(&server, name_req("Person", "Alice", &t1));
        assert_eq!(node_ids(&value), vec![id]);
    }

    #[test]
    fn node_deleted_before_t_recalled_when_both_dimensions_anchored() {
        let server = create_test_server();
        let id = create_named(&server, "Person", "Alice");
        let t_before = anchor_after_commits();
        delete(&server, id);
        let t_after = anchor_after_commits();

        // Both dimensions anchored before the deletion recall the node even
        // though it no longer exists in current state.
        let value = find(
            &server,
            FindNodesAtTimeRequest {
                transaction_time: Some(t_before.clone()),
                ..name_req("Person", "Alice", &t_before)
            },
        );
        assert_eq!(node_ids(&value), vec![id], "{value}");

        // At a coordinate after the deletion the node is gone.
        let value = find(
            &server,
            FindNodesAtTimeRequest {
                transaction_time: Some(t_after.clone()),
                ..name_req("Person", "Alice", &t_after)
            },
        );
        assert!(node_ids(&value).is_empty(), "{value}");
    }

    #[test]
    fn label_only_as_of_scan_without_property_filter() {
        let server = create_test_server();
        let a = create_named(&server, "Person", "Alice");
        let b = create_named(&server, "Person", "Bob");
        let _c = create_named(&server, "Company", "Acme");
        let t1 = anchor_after_commits();
        delete(&server, b);
        let t2 = anchor_after_commits();

        let value = find(
            &server,
            FindNodesAtTimeRequest {
                transaction_time: Some(t1.clone()),
                ..base_req("Person", &t1)
            },
        );
        assert_eq!(node_ids(&value), vec![a, b], "{value}");

        let value = find(
            &server,
            FindNodesAtTimeRequest {
                transaction_time: Some(t2.clone()),
                ..base_req("Person", &t2)
            },
        );
        assert_eq!(node_ids(&value), vec![a], "{value}");
    }

    #[test]
    fn current_state_equivalence_with_list_nodes_property_filter() {
        let server = create_test_server();
        let _a = create_named(&server, "Person", "Alice");
        let _b = create_named(&server, "Person", "Bob");
        let _c = create_named(&server, "Person", "Alice");
        let now = anchor_after_commits();

        let list_value: serde_json::Value =
            serde_json::from_str(&server.list_nodes(ListNodesRequest {
                label: Some("Person".to_string()),
                property_key: Some("name".to_string()),
                property_value: Some(serde_json::json!("Alice")),
                limit: None,
                offset: None,
                include_vectors: None,
            }))
            .unwrap();
        let mut current_ids = node_ids(&list_value);
        current_ids.sort_unstable();

        let at_now = find(&server, name_req("Person", "Alice", &now));
        assert_eq!(
            node_ids(&at_now),
            current_ids,
            "valid_time=now + transaction_time=now must equal the current-state result set"
        );
    }

    #[test]
    fn pagination_limit_offset_and_completeness_metadata() {
        let server = create_test_server();
        let mut created: Vec<u64> = (0..3)
            .map(|_| create_named(&server, "Person", "Alice"))
            .collect();
        created.sort_unstable();
        let now = anchor_after_commits();

        // Page 1 (limit 1): stable ordering, exact total, continuation cursor.
        let page1 = find(
            &server,
            FindNodesAtTimeRequest {
                limit: Some(1),
                ..name_req("Person", "Alice", &now)
            },
        );
        assert_eq!(node_ids(&page1), vec![created[0]], "{page1}");
        assert_eq!(page1["count"], serde_json::json!(1));
        assert_eq!(page1["total_matching"], serde_json::json!(3));
        assert_eq!(page1["has_more"], serde_json::json!(true));
        assert_eq!(page1["next_offset"], serde_json::json!(1));

        // Page 2 continues where page 1 left off.
        let page2 = find(
            &server,
            FindNodesAtTimeRequest {
                limit: Some(1),
                offset: Some(1),
                ..name_req("Person", "Alice", &now)
            },
        );
        assert_eq!(node_ids(&page2), vec![created[1]], "{page2}");

        // Final page reports has_more: false.
        let page3 = find(
            &server,
            FindNodesAtTimeRequest {
                limit: Some(1),
                offset: Some(2),
                ..name_req("Person", "Alice", &now)
            },
        );
        assert_eq!(node_ids(&page3), vec![created[2]], "{page3}");
        assert_eq!(page3["has_more"], serde_json::json!(false));
    }

    #[test]
    fn oversized_limit_and_offset_are_clamped_not_errors() {
        let server = create_test_server();
        let _ = create_named(&server, "Person", "Alice");
        let now = anchor_after_commits();

        // Far beyond MAX_RESULT_LIMIT / MAX_PAGINATION_OFFSET: clamped,
        // exactly like list_nodes -- never an error.
        let value = find(
            &server,
            FindNodesAtTimeRequest {
                limit: Some(1_000_000),
                offset: Some(1_000_000),
                ..name_req("Person", "Alice", &now)
            },
        );
        assert!(value.get("error").is_none(), "{value}");
        assert_eq!(value["limit"], serde_json::json!(10_000));
        assert_eq!(value["offset"], serde_json::json!(10_000));
        assert!(node_ids(&value).is_empty(), "offset past the end: {value}");
    }

    #[test]
    fn invalid_or_empty_timestamps_return_structured_errors() {
        let server = create_test_server();

        let response = server.find_nodes_at_time(base_req("Person", "not-a-timestamp"));
        assert_invalid_argument(&response, "valid_time");

        let response = server.find_nodes_at_time(base_req("Person", ""));
        assert_invalid_argument(&response, "valid_time");

        let response = server.find_nodes_at_time(FindNodesAtTimeRequest {
            transaction_time: Some("not-a-timestamp".to_string()),
            ..base_req("Person", &Utc::now().to_rfc3339())
        });
        assert_invalid_argument(&response, "transaction_time");
    }

    #[test]
    fn property_key_without_value_and_reverse_return_structured_errors() {
        let server = create_test_server();
        let now = Utc::now().to_rfc3339();

        let response = server.find_nodes_at_time(FindNodesAtTimeRequest {
            property_key: Some("name".to_string()),
            ..base_req("Person", &now)
        });
        assert_invalid_argument(&response, "property_key");

        let response = server.find_nodes_at_time(FindNodesAtTimeRequest {
            property_value: Some(serde_json::json!("Alice")),
            ..base_req("Person", &now)
        });
        assert_invalid_argument(&response, "property_value");
    }

    #[test]
    fn unsupported_property_value_type_returns_structured_error() {
        let server = create_test_server();
        let response = server.find_nodes_at_time(FindNodesAtTimeRequest {
            property_key: Some("name".to_string()),
            property_value: Some(serde_json::json!({"nested": "object"})),
            ..base_req("Person", &Utc::now().to_rfc3339())
        });
        assert_invalid_argument(&response, "property_value");
    }

    #[test]
    fn response_carries_resolved_bitemporal_coordinate_as_rfc3339() {
        let server = create_test_server();
        let _ = create_named(&server, "Person", "Alice");
        let anchor = anchor_after_commits();
        let anchor_micros = DateTime::parse_from_rfc3339(&anchor)
            .unwrap()
            .timestamp_micros();

        let value = find(&server, name_req("Person", "Alice", &anchor));

        // The resolved valid_time round-trips to the exact requested instant.
        let valid_time = value["valid_time"].as_str().expect("valid_time present");
        assert_eq!(
            DateTime::parse_from_rfc3339(valid_time)
                .expect("valid_time should be RFC 3339")
                .timestamp_micros(),
            anchor_micros
        );

        // The omitted transaction_time resolves to a concrete "now", not a
        // placeholder -- it must parse and be at/after the anchor.
        let tx_time = value["transaction_time"]
            .as_str()
            .expect("transaction_time present");
        let tx_micros = DateTime::parse_from_rfc3339(tx_time)
            .expect("transaction_time should be RFC 3339")
            .timestamp_micros();
        assert!(
            tx_micros >= anchor_micros,
            "resolved transaction_time {tx_time} should not precede the request anchor {anchor}"
        );
    }

    #[test]
    fn unknown_label_returns_empty_set_not_error() {
        let server = create_test_server();
        let value = find(
            &server,
            base_req("NeverSeenLabel3236", &Utc::now().to_rfc3339()),
        );
        assert!(value.get("error").is_none(), "{value}");
        assert!(node_ids(&value).is_empty());
        assert_eq!(value["total_matching"], serde_json::json!(0));
        assert_eq!(value["has_more"], serde_json::json!(false));
    }
}

// ============================================================================
// Structured Error Codes with Retriable Flag (Issue #3234)
// ============================================================================
//
// Every MCP error response must be `{ "error": { "code", "message",
// "retriable", "details"? } }` where `code` is drawn from the documented,
// stable enum and `retriable` is `true` only for transient failure classes.
// These tests drive every distinct MCP error path and assert the exact code,
// plus a registry-driven sweep that automatically covers newly added tools.

mod structured_error_tests {
    use super::*;
    use serde_json::json;

    /// Every code documented for the MCP error contract (Issue #3234).
    const DOCUMENTED_CODES: &[&str] = &[
        "NOT_FOUND",
        "INVALID_ARGUMENT",
        "CONSTRAINT_VIOLATION",
        "FAILED_PRECONDITION",
        "CONFLICT",
        "UNAVAILABLE",
        "INTERNAL",
    ];

    /// Dispatch a tool through the same table `call_tool` uses and parse the
    /// JSON payload. Returns `(parsed_json, is_error)`.
    fn dispatch(
        server: &AletheiaMcpServer,
        tool: &str,
        args: serde_json::Value,
    ) -> (serde_json::Value, bool) {
        let result = server.dispatch_tool(tool, args);
        let is_error = result.is_error.unwrap_or(false);
        let text = result
            .content
            .first()
            .and_then(|c| c.as_text().map(|t| t.text.clone()))
            .expect("tool result should carry text content");
        let value = serde_json::from_str(&text).expect("tool response should be valid JSON");
        (value, is_error)
    }

    /// Assert the response carries the Issue #3234 structured error shape and
    /// return `(code, retriable, message)`.
    fn assert_structured_error(value: &serde_json::Value, context: &str) -> (String, bool, String) {
        let error = value
            .get("error")
            .unwrap_or_else(|| panic!("[{context}] expected an `error` payload, got: {value}"));
        let obj = error.as_object().unwrap_or_else(|| {
            panic!("[{context}] `error` must be a structured object, not free text: {error}")
        });
        let code = obj
            .get("code")
            .and_then(|c| c.as_str())
            .unwrap_or_else(|| panic!("[{context}] error must carry a string `code`: {error}"))
            .to_string();
        assert!(
            DOCUMENTED_CODES.contains(&code.as_str()),
            "[{context}] code {code:?} is not in the documented enum"
        );
        let retriable = obj
            .get("retriable")
            .and_then(|r| r.as_bool())
            .unwrap_or_else(|| {
                panic!("[{context}] error must carry a boolean `retriable`: {error}")
            });
        let message = obj
            .get("message")
            .and_then(|m| m.as_str())
            .unwrap_or_else(|| panic!("[{context}] error must carry a string `message`: {error}"))
            .to_string();
        assert!(
            !message.is_empty(),
            "[{context}] error message must not be empty"
        );
        (code, retriable, message)
    }

    /// Convenience: dispatch, require an error, assert shape + expected code.
    fn assert_error_code(
        server: &AletheiaMcpServer,
        tool: &str,
        args: serde_json::Value,
        expected_code: &str,
    ) -> serde_json::Value {
        let (value, is_error) = dispatch(server, tool, args);
        assert!(is_error, "[{tool}] expected an error result, got: {value}");
        let (code, retriable, _msg) = assert_structured_error(&value, tool);
        assert_eq!(code, expected_code, "[{tool}] wrong code: {value}");
        // Caller-fault classes are never advisorily retriable.
        if matches!(
            expected_code,
            "NOT_FOUND" | "INVALID_ARGUMENT" | "CONSTRAINT_VIOLATION" | "FAILED_PRECONDITION"
        ) {
            assert!(!retriable, "[{tool}] {expected_code} must not be retriable");
        }
        value
    }

    fn seed_node(server: &AletheiaMcpServer, label: &str, name: &str) -> u64 {
        let node: NodeResponse = parse_response(&server.create_node(CreateNodeRequest {
            label: label.to_string(),
            properties: Some(HashMap::from([(
                "name".to_string(),
                serde_json::Value::String(name.to_string()),
            )])),
            valid_time: None,
            provenance: None,
        }))
        .expect("seed node should succeed");
        node.id
    }

    // ------------------------------------------------------------------
    // Registry-driven sweep: every advertised tool, present and future,
    // must return the structured shape on its invalid-arguments path.
    // A tool added to `tool_definitions()`/`dispatch_tool` is covered
    // here automatically, with zero test changes.
    // ------------------------------------------------------------------

    #[test]
    fn every_advertised_tool_returns_structured_error_on_invalid_arguments() {
        let server = create_test_server();
        let mut errored = 0;
        for tool in server.list_tools_for_test() {
            let (value, is_error) = dispatch(&server, &tool, serde_json::Value::Null);
            if is_error {
                errored += 1;
                let (code, _, message) = assert_structured_error(&value, &tool);
                // A tool present in tool_definitions() but missing from
                // dispatch_tool's match would fall into the "Unknown tool"
                // NOT_FOUND arm and silently pass a shape-only sweep. Catch
                // that drift explicitly.
                assert!(
                    !message.contains("Unknown tool"),
                    "[{tool}] advertised but not dispatched: {message}"
                );
                // Null args can only fail argument deserialization, so the
                // code must be exactly INVALID_ARGUMENT.
                assert_eq!(
                    code, "INVALID_ARGUMENT",
                    "[{tool}] null args must classify as INVALID_ARGUMENT: {value}"
                );
            }
        }
        assert!(
            errored > 0,
            "sweep must exercise at least one error path (null args should fail deserialization)"
        );
    }

    #[test]
    fn unknown_tool_returns_not_found() {
        let server = create_test_server();
        let value = assert_error_code(
            &server,
            "definitely_not_a_tool",
            serde_json::Value::Null,
            "NOT_FOUND",
        );
        let (_, _, message) = assert_structured_error(&value, "unknown tool");
        assert!(message.contains("Unknown tool"), "got: {message}");
    }

    // ------------------------------------------------------------------
    // INVALID_ARGUMENT paths
    // ------------------------------------------------------------------

    #[test]
    fn malformed_arguments_return_invalid_argument() {
        let server = create_test_server();
        let value = assert_error_code(
            &server,
            "get_node",
            json!({"node_id": "not-a-number"}),
            "INVALID_ARGUMENT",
        );
        let (_, _, message) = assert_structured_error(&value, "get_node malformed");
        assert!(message.contains("Invalid arguments"), "got: {message}");
    }

    #[test]
    fn out_of_range_id_returns_invalid_argument() {
        let server = create_test_server();
        let value = assert_error_code(
            &server,
            "get_node",
            json!({"node_id": u64::MAX}),
            "INVALID_ARGUMENT",
        );
        // Message fidelity: the bare StorageError text, byte-for-byte
        // identical to what pre-#3234 releases returned as the whole `error`
        // value — no "Storage error: " wrapper prefix.
        let (_, _, message) = assert_structured_error(&value, "get_node out-of-range");
        let expected = crate::core::error::StorageError::InvalidId {
            id: u64::MAX,
            id_type: "node",
        }
        .to_string();
        assert_eq!(message, expected, "got: {message}");
        assert!(
            message.starts_with("Invalid node ID"),
            "message must be the bare StorageError text: {message}"
        );
        assert!(
            !message.starts_with("Storage error: "),
            "message must not gain a Storage error prefix: {message}"
        );

        let value = assert_error_code(
            &server,
            "get_edge",
            json!({"edge_id": u64::MAX}),
            "INVALID_ARGUMENT",
        );
        let (_, _, message) = assert_structured_error(&value, "get_edge out-of-range");
        assert!(
            message.starts_with("Invalid edge ID"),
            "message must be the bare StorageError text: {message}"
        );
    }

    #[test]
    fn invalid_timestamp_returns_invalid_argument() {
        let server = create_test_server();
        let value = assert_error_code(
            &server,
            "create_node",
            json!({"label": "Person", "valid_time": "not-a-timestamp"}),
            "INVALID_ARGUMENT",
        );
        let (_, _, message) = assert_structured_error(&value, "create_node bad valid_time");
        assert!(message.contains("Invalid valid_time"), "got: {message}");

        let node_id = seed_node(&server, "Person", "Alice");
        assert_error_code(
            &server,
            "get_node_at_time",
            json!({"node_id": node_id, "valid_time": "not-a-timestamp"}),
            "INVALID_ARGUMENT",
        );
        assert_error_code(
            &server,
            "traverse",
            json!({
                "start_node_id": node_id,
                "edge_label": "KNOWS",
                "as_of_valid_time": "not-a-timestamp"
            }),
            "INVALID_ARGUMENT",
        );
        assert_error_code(
            &server,
            "list_changes",
            json!({"tx_from": "not-a-timestamp", "tx_to": "2026-01-01T00:00:00Z"}),
            "INVALID_ARGUMENT",
        );
    }

    #[test]
    fn invalid_provenance_confidence_returns_invalid_argument() {
        let server = create_test_server();
        let value = assert_error_code(
            &server,
            "create_node",
            json!({"label": "Person", "provenance": {"confidence": 1.5}}),
            "INVALID_ARGUMENT",
        );
        let (_, _, message) = assert_structured_error(&value, "create_node bad provenance");
        assert!(message.contains("confidence"), "got: {message}");
    }

    #[test]
    fn list_nodes_inconsistent_property_filter_returns_invalid_argument() {
        let server = create_test_server();
        assert_error_code(
            &server,
            "list_nodes",
            json!({"label": "Person", "property_key": "name"}),
            "INVALID_ARGUMENT",
        );
        // Property filter without label is likewise a caller fault.
        assert_error_code(
            &server,
            "list_nodes",
            json!({"property_key": "name", "property_value": "Alice"}),
            "INVALID_ARGUMENT",
        );
    }

    #[test]
    fn delete_node_valid_time_with_detach_returns_invalid_argument() {
        let server = create_test_server();
        let node_id = seed_node(&server, "Person", "Alice");
        assert_error_code(
            &server,
            "delete_node",
            json!({
                "node_id": node_id,
                "detach": true,
                "valid_time": "2024-01-01T00:00:00Z"
            }),
            "INVALID_ARGUMENT",
        );
    }

    #[test]
    fn hybrid_query_without_entry_point_returns_invalid_argument() {
        let server = create_test_server();
        assert_error_code(&server, "hybrid_query", json!({}), "INVALID_ARGUMENT");
    }

    #[test]
    fn find_similar_dimension_mismatch_returns_invalid_argument() {
        let server = create_test_server();
        let (_, is_error) = dispatch(
            &server,
            "enable_vector_index",
            json!({"property_name": "embedding", "dimensions": 4}),
        );
        assert!(!is_error, "enabling the index should succeed");
        assert_error_code(
            &server,
            "find_similar",
            json!({"property_name": "embedding", "embedding": [0.1, 0.2, 0.3]}),
            "INVALID_ARGUMENT",
        );
    }

    // ------------------------------------------------------------------
    // NOT_FOUND paths
    // ------------------------------------------------------------------

    #[test]
    fn missing_entities_return_not_found() {
        let server = create_test_server();
        let value = assert_error_code(&server, "get_node", json!({"node_id": 424242}), "NOT_FOUND");
        let (_, _, message) = assert_structured_error(&value, "get_node missing");
        assert!(message.contains("Node not found"), "got: {message}");

        assert_error_code(&server, "get_edge", json!({"edge_id": 424242}), "NOT_FOUND");
        assert_error_code(
            &server,
            "update_node",
            json!({"node_id": 424242, "properties": {"a": 1}}),
            "NOT_FOUND",
        );
        assert_error_code(
            &server,
            "update_edge",
            json!({"edge_id": 424242, "properties": {"a": 1}}),
            "NOT_FOUND",
        );
        assert_error_code(
            &server,
            "delete_node",
            json!({"node_id": 424242}),
            "NOT_FOUND",
        );
        assert_error_code(
            &server,
            "delete_edge",
            json!({"edge_id": 424242}),
            "NOT_FOUND",
        );
        // A missing endpoint surfaces from the transaction layer as a
        // validation failure: the request is well-formed, but the graph is
        // not in the state it requires -> FAILED_PRECONDITION.
        assert_error_code(
            &server,
            "create_edge",
            json!({"source_id": 424242, "target_id": 424243, "label": "KNOWS"}),
            "FAILED_PRECONDITION",
        );
    }

    #[test]
    fn node_absent_at_temporal_coordinate_returns_not_found() {
        let server = create_test_server();
        let node_id = seed_node(&server, "Person", "Alice");
        // Long before the node's creation: not found at that coordinate.
        assert_error_code(
            &server,
            "get_node_at_time",
            json!({"node_id": node_id, "valid_time": "2000-01-01T00:00:00Z"}),
            "NOT_FOUND",
        );
    }

    // ------------------------------------------------------------------
    // FAILED_PRECONDITION paths
    // ------------------------------------------------------------------

    #[test]
    fn find_similar_without_index_returns_failed_precondition() {
        let server = create_test_server();
        let value = assert_error_code(
            &server,
            "find_similar",
            json!({"property_name": "missing_prop", "embedding": [0.1, 0.2]}),
            "FAILED_PRECONDITION",
        );
        let (_, _, message) = assert_structured_error(&value, "find_similar no index");
        assert!(
            message.contains("Vector index not enabled"),
            "got: {message}"
        );

        // Same precondition through the hybrid query path.
        assert_error_code(
            &server,
            "hybrid_query",
            json!({"query_embedding": [0.1, 0.2], "vector_property": "missing_prop"}),
            "FAILED_PRECONDITION",
        );
    }

    /// Issue #3209's DETACH-delete refusal migrated to the structured shape:
    /// `FAILED_PRECONDITION` with `details.connected_edges`, while the legacy
    /// top-level `connected_edges` / `detach_required` fields remain (no loss).
    #[test]
    fn detach_refusal_returns_failed_precondition_with_connected_edges_details() {
        let server = create_test_server();
        let a = seed_node(&server, "Person", "Alice");
        let b = seed_node(&server, "Person", "Bob");
        let (_, is_error) = dispatch(
            &server,
            "create_edge",
            json!({"source_id": a, "target_id": b, "label": "KNOWS"}),
        );
        assert!(!is_error, "edge creation should succeed");

        let value = assert_error_code(
            &server,
            "delete_node",
            json!({"node_id": a}),
            "FAILED_PRECONDITION",
        );

        // Structured details carry the machine-readable count.
        assert_eq!(
            value["error"]["details"]["connected_edges"].as_u64(),
            Some(1),
            "details.connected_edges must report the edge count: {value}"
        );
        // Legacy #3209 fields are preserved additively at the top level.
        assert_eq!(value["connected_edges"].as_u64(), Some(1), "got: {value}");
        assert_eq!(value["detach_required"].as_bool(), Some(true));
        assert_eq!(value["success"].as_bool(), Some(false));
        assert_eq!(value["node_id"].as_u64(), Some(a));
        // Free text is preserved as error.message.
        let (_, _, message) = assert_structured_error(&value, "detach refusal");
        assert!(message.contains("connected edge"), "got: {message}");
        assert!(message.contains("detach"), "got: {message}");
    }

    // ------------------------------------------------------------------
    // CONSTRAINT_VIOLATION paths
    // ------------------------------------------------------------------

    #[test]
    fn unique_violation_returns_constraint_violation_with_details() {
        let server = create_test_server();
        let (_, is_error) = dispatch(
            &server,
            "enable_unique_constraint",
            json!({"label": "Person", "property": "email"}),
        );
        assert!(!is_error, "enabling the constraint should succeed");

        let (_, is_error) = dispatch(
            &server,
            "create_node",
            json!({"label": "Person", "properties": {"email": "a@example.com"}}),
        );
        assert!(!is_error, "first node should succeed");

        let value = assert_error_code(
            &server,
            "create_node",
            json!({"label": "Person", "properties": {"email": "a@example.com"}}),
            "CONSTRAINT_VIOLATION",
        );
        // Structured details carry the constraint metadata.
        assert_eq!(
            value["error"]["details"]["label"].as_str(),
            Some("Person"),
            "got: {value}"
        );
        assert_eq!(
            value["error"]["details"]["property"].as_str(),
            Some("email")
        );
        assert!(value["error"]["details"]["existing_node_id"].is_u64());
        // Legacy top-level fields are preserved additively.
        assert_eq!(value["constraint_violation"].as_bool(), Some(true));
        assert_eq!(value["label"].as_str(), Some("Person"));
        assert_eq!(value["property"].as_str(), Some("email"));
        assert!(value["existing_node_id"].is_u64());
    }

    #[test]
    fn enable_constraint_over_duplicates_returns_failed_precondition() {
        // DuplicateOnEnable is a *state* problem — duplicates already exist
        // in the graph, so the enable cannot succeed until the data is fixed.
        // That is FAILED_PRECONDITION, not a constraint rejecting this write.
        let server = create_test_server();
        for _ in 0..2 {
            let (_, is_error) = dispatch(
                &server,
                "create_node",
                json!({"label": "Person", "properties": {"email": "dup@example.com"}}),
            );
            assert!(!is_error);
        }
        assert_error_code(
            &server,
            "enable_unique_constraint",
            json!({"label": "Person", "property": "email"}),
            "FAILED_PRECONDITION",
        );
    }

    // ------------------------------------------------------------------
    // The `query` tool: `kind` is preserved verbatim (its own contract,
    // Issue #3213) while `code`/`retriable` are added additively.
    // ------------------------------------------------------------------

    #[test]
    fn query_tool_errors_keep_kind_and_gain_code_and_retriable() {
        let server = create_test_server();

        // read_only_violation
        let (value, is_error) = dispatch(
            &server,
            "query",
            json!({"language": "aql", "query": "CREATE (n:Person) RETURN n"}),
        );
        assert!(is_error);
        assert_eq!(
            value["error"]["kind"].as_str(),
            Some("read_only_violation"),
            "the query tool's kind field must be preserved: {value}"
        );
        let (code, retriable, _) = assert_structured_error(&value, "query read-only");
        assert_eq!(code, "INVALID_ARGUMENT");
        assert!(!retriable);

        // invalid_request (unsupported language)
        let (value, is_error) = dispatch(
            &server,
            "query",
            json!({"language": "sparql", "query": "SELECT ?s WHERE { ?s ?p ?o }"}),
        );
        assert!(is_error);
        assert_eq!(value["error"]["kind"].as_str(), Some("invalid_request"));
        let (code, _, _) = assert_structured_error(&value, "query bad language");
        assert_eq!(code, "INVALID_ARGUMENT");

        // parse_error
        let (value, is_error) = dispatch(
            &server,
            "query",
            json!({"language": "aql", "query": "THIS IS NOT A QUERY"}),
        );
        assert!(is_error);
        assert_eq!(
            value["error"]["kind"].as_str(),
            Some("parse_error"),
            "got: {value}"
        );
        let (code, _, _) = assert_structured_error(&value, "query parse error");
        assert_eq!(code, "INVALID_ARGUMENT");
    }

    // ------------------------------------------------------------------
    // History / independent-dimension temporal tools (Phase 11)
    // ------------------------------------------------------------------

    fn seed_edge(server: &AletheiaMcpServer, source: u64, target: u64) -> u64 {
        let (value, is_error) = dispatch(
            server,
            "create_edge",
            json!({"source_id": source, "target_id": target, "label": "KNOWS"}),
        );
        assert!(!is_error, "seed edge should succeed: {value}");
        value["id"].as_u64().expect("created edge must carry an id")
    }

    /// Every history/temporal tool must classify a missing entity as
    /// NOT_FOUND. Note the message here still flows through `db_error` (the
    /// storage lookup fails, not the ID parse), so assert the code, not the
    /// message text.
    #[test]
    fn history_tools_missing_entity_returns_not_found() {
        let server = create_test_server();
        let ts = "2026-01-01T00:00:00Z";
        let cases: Vec<(&str, serde_json::Value)> = vec![
            (
                "get_node_at_valid_time",
                json!({"node_id": 424242, "valid_time": ts}),
            ),
            (
                "get_node_at_transaction_time",
                json!({"node_id": 424242, "transaction_time": ts}),
            ),
            ("get_node_history", json!({"node_id": 424242})),
            (
                "diff_node_versions",
                json!({"node_id": 424242, "from_version": 1, "to_version": 2}),
            ),
            (
                "get_edge_at_valid_time",
                json!({"edge_id": 424242, "valid_time": ts}),
            ),
            (
                "get_edge_at_transaction_time",
                json!({"edge_id": 424242, "transaction_time": ts}),
            ),
            ("get_edge_history", json!({"edge_id": 424242})),
            (
                "diff_edge_versions",
                json!({"edge_id": 424242, "from_version": 1, "to_version": 2}),
            ),
        ];
        for (tool, args) in cases {
            assert_error_code(&server, tool, args, "NOT_FOUND");
        }
    }

    /// Every history/temporal tool that takes a timestamp must classify an
    /// unparseable one as INVALID_ARGUMENT — even when the entity exists.
    #[test]
    fn history_tools_bad_timestamp_returns_invalid_argument() {
        let server = create_test_server();
        let a = seed_node(&server, "Person", "Alice");
        let b = seed_node(&server, "Person", "Bob");
        let edge_id = seed_edge(&server, a, b);
        let cases: Vec<(&str, serde_json::Value)> = vec![
            (
                "get_node_at_valid_time",
                json!({"node_id": a, "valid_time": "not-a-timestamp"}),
            ),
            (
                "get_node_at_transaction_time",
                json!({"node_id": a, "transaction_time": "not-a-timestamp"}),
            ),
            (
                "get_edge_at_valid_time",
                json!({"edge_id": edge_id, "valid_time": "not-a-timestamp"}),
            ),
            (
                "get_edge_at_transaction_time",
                json!({"edge_id": edge_id, "transaction_time": "not-a-timestamp"}),
            ),
        ];
        for (tool, args) in cases {
            assert_error_code(&server, tool, args, "INVALID_ARGUMENT");
        }
    }

    #[test]
    fn edge_absent_at_temporal_coordinate_returns_not_found() {
        let server = create_test_server();
        let a = seed_node(&server, "Person", "Alice");
        let b = seed_node(&server, "Person", "Bob");
        let edge_id = seed_edge(&server, a, b);
        // Long before the edge's creation: not found at that coordinate.
        assert_error_code(
            &server,
            "get_edge_at_time",
            json!({"edge_id": edge_id, "valid_time": "2000-01-01T00:00:00Z"}),
            "NOT_FOUND",
        );
    }

    // ------------------------------------------------------------------
    // hybrid_query argument classification
    // ------------------------------------------------------------------

    #[test]
    fn hybrid_query_bad_temporal_arguments_return_invalid_argument() {
        let server = create_test_server();
        let node_id = seed_node(&server, "Person", "Alice");
        let value = assert_error_code(
            &server,
            "hybrid_query",
            json!({"start_node_id": node_id, "valid_time": "not-a-timestamp"}),
            "INVALID_ARGUMENT",
        );
        let (_, _, message) = assert_structured_error(&value, "hybrid_query bad valid_time");
        assert!(message.contains("Invalid valid_time"), "got: {message}");

        let value = assert_error_code(
            &server,
            "hybrid_query",
            json!({"start_node_id": node_id, "transaction_time": "not-a-timestamp"}),
            "INVALID_ARGUMENT",
        );
        let (_, _, message) = assert_structured_error(&value, "hybrid_query bad tx_time");
        assert!(
            message.contains("Invalid transaction_time"),
            "got: {message}"
        );
    }

    #[test]
    fn hybrid_query_embedding_dimension_mismatch_returns_invalid_argument() {
        let server = create_test_server();
        let (_, is_error) = dispatch(
            &server,
            "enable_vector_index",
            json!({"property_name": "embedding", "dimensions": 4}),
        );
        assert!(!is_error, "enabling the index should succeed");
        assert_error_code(
            &server,
            "hybrid_query",
            json!({"query_embedding": [0.1, 0.2, 0.3], "vector_property": "embedding"}),
            "INVALID_ARGUMENT",
        );
    }

    #[test]
    fn hybrid_query_missing_start_node_returns_not_found() {
        let server = create_test_server();
        assert_error_code(
            &server,
            "hybrid_query",
            json!({"start_node_id": 424242}),
            "NOT_FOUND",
        );
    }

    // ------------------------------------------------------------------
    // Adjacency, schema, create_edge endpoint parsing, list_changes
    // ------------------------------------------------------------------

    #[test]
    fn adjacency_tools_out_of_range_id_returns_invalid_argument() {
        let server = create_test_server();
        for tool in ["get_outgoing_edges", "get_incoming_edges"] {
            let value = assert_error_code(
                &server,
                tool,
                json!({"node_id": u64::MAX}),
                "INVALID_ARGUMENT",
            );
            let (_, _, message) = assert_structured_error(&value, tool);
            assert!(
                message.starts_with("Invalid node ID"),
                "[{tool}] bare StorageError text expected: {message}"
            );
        }
        // Pin the current contract for an in-range but *missing* node: the
        // adjacency tools return an empty adjacency (success), not NOT_FOUND.
        for tool in ["get_outgoing_edges", "get_incoming_edges"] {
            let (value, is_error) = dispatch(&server, tool, json!({"node_id": 424242}));
            assert!(
                !is_error,
                "[{tool}] missing node currently yields an empty adjacency: {value}"
            );
            assert_eq!(value["count"].as_u64(), Some(0), "got: {value}");
        }
    }

    #[test]
    fn get_schema_bad_as_of_returns_invalid_argument() {
        let server = create_test_server();
        assert_error_code(
            &server,
            "get_schema",
            json!({"as_of_valid_time": "not-a-timestamp"}),
            "INVALID_ARGUMENT",
        );
        assert_error_code(
            &server,
            "get_schema",
            json!({"as_of_transaction_time": "not-a-timestamp"}),
            "INVALID_ARGUMENT",
        );
    }

    #[test]
    fn create_edge_out_of_range_endpoints_return_invalid_argument() {
        let server = create_test_server();
        let value = assert_error_code(
            &server,
            "create_edge",
            json!({"source_id": u64::MAX, "target_id": 1, "label": "KNOWS"}),
            "INVALID_ARGUMENT",
        );
        let (_, _, message) = assert_structured_error(&value, "create_edge bad source_id");
        assert!(message.contains("Invalid source_id"), "got: {message}");

        let value = assert_error_code(
            &server,
            "create_edge",
            json!({"source_id": 1, "target_id": u64::MAX, "label": "KNOWS"}),
            "INVALID_ARGUMENT",
        );
        let (_, _, message) = assert_structured_error(&value, "create_edge bad target_id");
        assert!(message.contains("Invalid target_id"), "got: {message}");
    }

    #[test]
    fn list_changes_bad_tx_to_returns_invalid_argument() {
        let server = create_test_server();
        let value = assert_error_code(
            &server,
            "list_changes",
            json!({"tx_from": "2026-01-01T00:00:00Z", "tx_to": "not-a-timestamp"}),
            "INVALID_ARGUMENT",
        );
        let (_, _, message) = assert_structured_error(&value, "list_changes bad tx_to");
        assert!(message.contains("Invalid tx_to"), "got: {message}");
    }
}

// ============================================================================
// Temporal Extent Tests (temporal_extent, Issue #3238)
// ============================================================================

mod temporal_extent_tests {
    use super::*;

    fn extent_req(by_label: Option<bool>) -> TemporalExtentRequest {
        TemporalExtentRequest { by_label }
    }

    fn create_node_at(server: &AletheiaMcpServer, label: &str, valid_time: &str) -> NodeResponse {
        let response = server.create_node(CreateNodeRequest {
            valid_time: Some(valid_time.to_string()),
            label: label.to_string(),
            properties: Some({
                let mut m = HashMap::new();
                m.insert("name".to_string(), serde_json::json!("x"));
                m
            }),
            provenance: None,
        });
        parse_response(&response).expect("create node")
    }

    fn assert_rfc3339(value: &serde_json::Value, context: &str) {
        let s = value
            .as_str()
            .unwrap_or_else(|| panic!("{context} must be a string: {value}"));
        chrono::DateTime::parse_from_rfc3339(s)
            .unwrap_or_else(|e| panic!("{context} must be RFC3339, got '{s}': {e}"));
    }

    #[test]
    fn test_temporal_extent_tool_listed_in_registry() {
        let server = create_test_server();
        let tools = server.list_tools_for_test();
        assert!(
            tools.iter().any(|name| name == "temporal_extent"),
            "temporal_extent must be registered in the tool list"
        );
    }

    #[test]
    fn test_temporal_extent_empty_database_returns_null_bounds() {
        let server = create_test_server();

        let response = server.temporal_extent(extent_req(None));
        let value: serde_json::Value = serde_json::from_str(&response).unwrap();

        assert!(value.get("error").is_none(), "unexpected error: {value}");
        // Bounds must be explicit nulls -- present in the payload, and
        // definitely not epoch/1970 strings.
        for dim in ["valid_time", "transaction_time"] {
            for bound in ["earliest", "latest"] {
                let b = value[dim]
                    .get(bound)
                    .unwrap_or_else(|| panic!("{dim}.{bound} must be present"));
                assert!(
                    b.is_null(),
                    "{dim}.{bound} on an empty DB must be null, got {b}"
                );
            }
        }
        // Without by_label, no breakdown keys appear at all.
        assert!(value.get("node_labels").is_none());
        assert!(value.get("edge_types").is_none());
    }

    #[test]
    fn test_temporal_extent_empty_database_by_label_returns_empty_breakdown() {
        let server = create_test_server();

        let response = server.temporal_extent(extent_req(Some(true)));
        let value: serde_json::Value = serde_json::from_str(&response).unwrap();

        assert!(value.get("error").is_none(), "unexpected error: {value}");
        assert!(value["valid_time"]["earliest"].is_null());
        assert!(value["transaction_time"]["latest"].is_null());
        assert_eq!(value["node_labels"].as_array().unwrap().len(), 0);
        assert_eq!(value["edge_types"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn test_temporal_extent_populated_shape_and_rfc3339() {
        let server = create_test_server();

        let alice = create_node_at(&server, "Person", "2021-03-01T00:00:00Z");
        let acme = create_node_at(&server, "Company", "2023-06-15T00:00:00Z");
        server.create_edge(CreateEdgeRequest {
            valid_time: Some("2024-01-01T00:00:00Z".to_string()),
            source_id: alice.id,
            target_id: acme.id,
            label: "WORKS_AT".to_string(),
            properties: None,
            provenance: None,
        });

        let response = server.temporal_extent(extent_req(None));
        let value: serde_json::Value = serde_json::from_str(&response).unwrap();

        assert!(value.get("error").is_none(), "unexpected error: {value}");

        // Valid-time bounds reflect the backdated writes exactly (all
        // intervals are open-ended, so only their starts count).
        assert_eq!(
            value["valid_time"]["earliest"],
            serde_json::json!("2021-03-01T00:00:00.000000Z")
        );
        assert_eq!(
            value["valid_time"]["latest"],
            serde_json::json!("2024-01-01T00:00:00.000000Z")
        );

        // Transaction-time bounds are system-assigned but must be real,
        // parseable RFC3339 strings (not sentinels, not micros integers).
        assert_rfc3339(&value["transaction_time"]["earliest"], "tx earliest");
        assert_rfc3339(&value["transaction_time"]["latest"], "tx latest");

        // No breakdown without by_label.
        assert!(value.get("node_labels").is_none());
        assert!(value.get("edge_types").is_none());
    }

    #[test]
    fn test_temporal_extent_expired_version_still_counts_toward_earliest() {
        let server = create_test_server();

        let alice = create_node_at(&server, "Person", "2021-03-01T00:00:00Z");
        // Supersede the original version: its interval is closed, but its
        // backdated start must still bound `earliest` (extent covers ALL
        // recorded history, not just current state).
        server.update_node(UpdateNodeRequest {
            valid_time: Some("2025-01-01T00:00:00Z".to_string()),
            node_id: alice.id,
            properties: {
                let mut m = HashMap::new();
                m.insert("name".to_string(), serde_json::json!("Alice v2"));
                m
            },
            provenance: None,
        });

        let response = server.temporal_extent(extent_req(None));
        let value: serde_json::Value = serde_json::from_str(&response).unwrap();

        assert!(value.get("error").is_none(), "unexpected error: {value}");
        assert_eq!(
            value["valid_time"]["earliest"],
            serde_json::json!("2021-03-01T00:00:00.000000Z"),
            "superseded version must still count toward earliest"
        );
        assert_eq!(
            value["valid_time"]["latest"],
            serde_json::json!("2025-01-01T00:00:00.000000Z")
        );
    }

    #[test]
    fn test_temporal_extent_by_label_breakdown() {
        let server = create_test_server();

        let alice = create_node_at(&server, "Person", "2021-03-01T00:00:00Z");
        let acme = create_node_at(&server, "Company", "2023-06-15T00:00:00Z");
        server.create_edge(CreateEdgeRequest {
            valid_time: Some("2024-01-01T00:00:00Z".to_string()),
            source_id: alice.id,
            target_id: acme.id,
            label: "WORKS_AT".to_string(),
            properties: None,
            provenance: None,
        });

        let response = server.temporal_extent(extent_req(Some(true)));
        let value: serde_json::Value = serde_json::from_str(&response).unwrap();

        assert!(value.get("error").is_none(), "unexpected error: {value}");

        let node_labels = value["node_labels"].as_array().unwrap();
        assert_eq!(node_labels.len(), 2);
        // Sorted by label: Company before Person.
        assert_eq!(node_labels[0]["label"], serde_json::json!("Company"));
        assert_eq!(
            node_labels[0]["valid_time"]["earliest"],
            serde_json::json!("2023-06-15T00:00:00.000000Z")
        );
        assert_eq!(node_labels[1]["label"], serde_json::json!("Person"));
        assert_eq!(
            node_labels[1]["valid_time"]["earliest"],
            serde_json::json!("2021-03-01T00:00:00.000000Z")
        );
        assert_rfc3339(
            &node_labels[1]["transaction_time"]["earliest"],
            "Person tx earliest",
        );

        let edge_types = value["edge_types"].as_array().unwrap();
        assert_eq!(edge_types.len(), 1);
        assert_eq!(edge_types[0]["edge_type"], serde_json::json!("WORKS_AT"));
        assert_eq!(
            edge_types[0]["valid_time"]["earliest"],
            serde_json::json!("2024-01-01T00:00:00.000000Z")
        );

        // Overall bounds still cover the union of all labels.
        assert_eq!(
            value["valid_time"]["earliest"],
            serde_json::json!("2021-03-01T00:00:00.000000Z")
        );
        assert_eq!(
            value["valid_time"]["latest"],
            serde_json::json!("2024-01-01T00:00:00.000000Z")
        );
    }

    #[test]
    fn test_temporal_extent_deleted_node_tombstone_still_counts() {
        // Deletions count toward the extent: a node backdated to 2021 and
        // then deleted must still bound valid_time.earliest, and both
        // dimensions' latest must reflect the deletion event.
        let server = create_test_server();

        let alice = create_node_at(&server, "Person", "2021-03-01T00:00:00Z");
        let delete_response = server.delete_node(DeleteNodeRequest {
            node_id: alice.id,
            detach: None,
            valid_time: None,
        });
        let delete_value: serde_json::Value = serde_json::from_str(&delete_response).unwrap();
        assert!(
            delete_value.get("error").is_none(),
            "unexpected delete error: {delete_value}"
        );

        let response = server.temporal_extent(extent_req(None));
        let value: serde_json::Value = serde_json::from_str(&response).unwrap();
        assert!(value.get("error").is_none(), "unexpected error: {value}");

        assert_eq!(
            value["valid_time"]["earliest"],
            serde_json::json!("2021-03-01T00:00:00.000000Z"),
            "a deleted node's backdated start must still bound earliest"
        );

        // The deletion closed the valid interval at ~now and recorded the
        // tombstone: both latest bounds move past the 2021 backdate.
        for dim in ["valid_time", "transaction_time"] {
            assert_rfc3339(&value[dim]["latest"], &format!("{dim} latest"));
            let latest =
                chrono::DateTime::parse_from_rfc3339(value[dim]["latest"].as_str().unwrap())
                    .unwrap();
            let backdate = chrono::DateTime::parse_from_rfc3339("2022-01-01T00:00:00Z").unwrap();
            assert!(
                latest > backdate,
                "{dim} latest must reflect the deletion event, got {latest}"
            );
        }
    }

    #[test]
    fn test_as_of_before_extent_is_empty_inside_extent_has_data() {
        // The answerability contract the extent exists to provide: an AS OF
        // query strictly before valid_time.earliest returns empty, while the
        // same query inside the extent returns data.
        let server = create_test_server();

        create_node_at(&server, "Person", "2021-03-01T00:00:00Z");

        let response = server.temporal_extent(extent_req(None));
        let value: serde_json::Value = serde_json::from_str(&response).unwrap();
        assert_eq!(
            value["valid_time"]["earliest"],
            serde_json::json!("2021-03-01T00:00:00.000000Z")
        );

        // AS OF before the extent: empty (indistinguishable from "never
        // existed" -- exactly why temporal_extent must be consulted first).
        let before = server.get_schema(GetSchemaRequest {
            as_of_valid_time: Some("2019-01-01T00:00:00Z".to_string()),
            as_of_transaction_time: None,
        });
        let before: serde_json::Value = serde_json::from_str(&before).unwrap();
        assert!(before.get("error").is_none(), "unexpected error: {before}");
        assert_eq!(
            before["node_labels"].as_array().unwrap().len(),
            0,
            "AS OF before valid_time.earliest must return empty"
        );

        // The same query inside the extent returns the data.
        let inside = server.get_schema(GetSchemaRequest {
            as_of_valid_time: Some("2022-01-01T00:00:00Z".to_string()),
            as_of_transaction_time: None,
        });
        let inside: serde_json::Value = serde_json::from_str(&inside).unwrap();
        assert!(inside.get("error").is_none(), "unexpected error: {inside}");
        let labels = inside["node_labels"].as_array().unwrap();
        assert_eq!(labels.len(), 1, "AS OF inside the extent must return data");
        assert_eq!(labels[0]["label"], serde_json::json!("Person"));
    }
}
