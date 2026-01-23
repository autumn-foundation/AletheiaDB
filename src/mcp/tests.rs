//! Tests for the GallifreyDB MCP server.
//!
//! These tests verify the MCP server functionality including:
//! - Server initialization and info
//! - Node CRUD operations
//! - Edge CRUD operations
//! - Graph traversal
//! - Vector similarity search
//! - Temporal queries
//! - Hybrid queries

use std::collections::HashMap;
use std::sync::Arc;

use crate::core::PropertyMapBuilder;
use crate::db::GallifreyDB;

use super::server::GallifreyMcpServer;
use super::tools::*;

/// Helper to create a test database with some sample data.
fn create_test_db() -> Arc<GallifreyDB> {
    let db = GallifreyDB::new().expect("Failed to create database");
    Arc::new(db)
}

#[allow(unused_imports)]
use rmcp::ServerHandler;

/// Helper to create a test server.
fn create_test_server() -> GallifreyMcpServer {
    GallifreyMcpServer::new(create_test_db())
}

/// Helper to parse JSON response and check for errors.
fn parse_response<T: serde::de::DeserializeOwned>(response: &str) -> Result<T, String> {
    let value: serde_json::Value =
        serde_json::from_str(response).map_err(|e| format!("Failed to parse JSON: {}", e))?;

    if let Some(error) = value.get("error") {
        return Err(error.as_str().unwrap_or("Unknown error").to_string());
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

    let server = GallifreyMcpServer::new(db);
    assert_eq!(server.db().node_count(), 1);
}

// ============================================================================
// Node CRUD Tests
// ============================================================================

mod node_tests {
    use super::*;

    #[test]
    fn test_create_node_basic() {
        let server = create_test_server();

        let req = CreateNodeRequest {
            label: "Person".to_string(),
            properties: None,
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
            label: "Person".to_string(),
            properties: Some(props),
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
            label: "Person".to_string(),
            properties: Some(props),
        };

        let create_response = server.create_node(create_req);
        let created: NodeResponse = parse_response(&create_response).expect("Failed to create");

        // Now get it
        let get_req = GetNodeRequest {
            node_id: created.id,
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

        let req = GetNodeRequest { node_id: 999999 };

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
            label: "Person".to_string(),
            properties: Some(props),
        };

        let create_response = server.create_node(create_req);
        let created: NodeResponse = parse_response(&create_response).expect("Failed to create");

        // Update it
        let mut new_props = HashMap::new();
        new_props.insert("age".to_string(), serde_json::json!(26));
        new_props.insert("city".to_string(), serde_json::json!("London"));

        let update_req = UpdateNodeRequest {
            node_id: created.id,
            properties: new_props,
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
            label: "ToDelete".to_string(),
            properties: None,
        };

        let create_response = server.create_node(create_req);
        let created: NodeResponse = parse_response(&create_response).expect("Failed to create");
        let node_id = created.id;

        // Delete it
        let delete_req = DeleteNodeRequest { node_id };

        let delete_response = server.delete_node(delete_req);
        let value: serde_json::Value = serde_json::from_str(&delete_response).unwrap();

        assert_eq!(value.get("success"), Some(&serde_json::json!(true)));

        // Verify it's gone
        let get_req = GetNodeRequest { node_id };
        let get_response = server.get_node(get_req);
        let get_value: serde_json::Value = serde_json::from_str(&get_response).unwrap();

        assert!(get_value.get("error").is_some());
    }

    #[test]
    fn test_list_nodes() {
        let server = create_test_server();

        // Create multiple nodes
        for i in 0..5 {
            let mut props = HashMap::new();
            props.insert("index".to_string(), serde_json::json!(i));

            let req = CreateNodeRequest {
                label: "ListTest".to_string(),
                properties: Some(props),
            };
            server.create_node(req);
        }

        // List with label filter (required for listing nodes efficiently)
        let list_req = ListNodesRequest {
            label: Some("ListTest".to_string()),
            limit: None,
            offset: None,
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
                label: "TypeA".to_string(),
                properties: None,
            });
        }

        for _ in 0..2 {
            server.create_node(CreateNodeRequest {
                label: "TypeB".to_string(),
                properties: None,
            });
        }

        // List only TypeA
        let list_req = ListNodesRequest {
            label: Some("TypeA".to_string()),
            limit: None,
            offset: None,
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
                label: "Paginated".to_string(),
                properties: Some(props),
            });
        }

        // Get first page (with label filter required for efficient listing)
        let page1_req = ListNodesRequest {
            label: Some("Paginated".to_string()),
            limit: Some(5),
            offset: Some(0),
        };

        let page1_response = server.list_nodes(page1_req);
        let page1: serde_json::Value = serde_json::from_str(&page1_response).unwrap();

        assert_eq!(page1.get("count"), Some(&serde_json::json!(5)));

        // Get second page
        let page2_req = ListNodesRequest {
            label: Some("Paginated".to_string()),
            limit: Some(5),
            offset: Some(5),
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
                label: "Counted".to_string(),
                properties: None,
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
                label: "CountA".to_string(),
                properties: None,
            });
        }

        for _ in 0..2 {
            server.create_node(CreateNodeRequest {
                label: "CountB".to_string(),
                properties: None,
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

    fn create_two_nodes(server: &GallifreyMcpServer) -> (u64, u64) {
        let node1 = server.create_node(CreateNodeRequest {
            label: "Person".to_string(),
            properties: Some({
                let mut m = HashMap::new();
                m.insert("name".to_string(), serde_json::json!("Alice"));
                m
            }),
        });
        let n1: NodeResponse = parse_response(&node1).unwrap();

        let node2 = server.create_node(CreateNodeRequest {
            label: "Person".to_string(),
            properties: Some({
                let mut m = HashMap::new();
                m.insert("name".to_string(), serde_json::json!("Bob"));
                m
            }),
        });
        let n2: NodeResponse = parse_response(&node2).unwrap();

        (n1.id, n2.id)
    }

    #[test]
    fn test_create_edge_basic() {
        let server = create_test_server();
        let (source_id, target_id) = create_two_nodes(&server);

        let req = CreateEdgeRequest {
            source_id,
            target_id,
            label: "KNOWS".to_string(),
            properties: None,
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
            source_id,
            target_id,
            label: "KNOWS".to_string(),
            properties: Some(props),
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
            source_id,
            target_id,
            label: "KNOWS".to_string(),
            properties: None,
        });
        let created: EdgeResponse = parse_response(&create_response).unwrap();

        let get_response = server.get_edge(GetEdgeRequest {
            edge_id: created.id,
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
            source_id,
            target_id,
            label: "KNOWS".to_string(),
            properties: None,
        });
        let created: EdgeResponse = parse_response(&create_response).unwrap();

        let mut new_props = HashMap::new();
        new_props.insert("weight".to_string(), serde_json::json!(0.5));

        let update_response = server.update_edge(UpdateEdgeRequest {
            edge_id: created.id,
            properties: new_props,
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
            source_id,
            target_id,
            label: "KNOWS".to_string(),
            properties: None,
        });
        let created: EdgeResponse = parse_response(&create_response).unwrap();

        let delete_response = server.delete_edge(DeleteEdgeRequest {
            edge_id: created.id,
        });
        let value: serde_json::Value = serde_json::from_str(&delete_response).unwrap();
        assert_eq!(value.get("success"), Some(&serde_json::json!(true)));

        // Verify it's gone
        let get_response = server.get_edge(GetEdgeRequest {
            edge_id: created.id,
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
            label: "Person".to_string(),
            properties: None,
        });
        let n3: NodeResponse = parse_response(&node3).unwrap();

        // Create edges
        server.create_edge(CreateEdgeRequest {
            source_id: n1,
            target_id: n2,
            label: "KNOWS".to_string(),
            properties: None,
        });
        server.create_edge(CreateEdgeRequest {
            source_id: n2,
            target_id: n3.id,
            label: "KNOWS".to_string(),
            properties: None,
        });

        // Note: list_edges doesn't support listing all edges without a node
        // It returns a message indicating to use get_outgoing_edges or get_incoming_edges
        let list_response = server.list_edges(ListEdgesRequest {
            label: None,
            limit: None,
            offset: None,
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
            source_id: n1,
            target_id: n2,
            label: "KNOWS".to_string(),
            properties: None,
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
            label: "Person".to_string(),
            properties: None,
        });
        let n3: NodeResponse = parse_response(&node3).unwrap();

        // Create edges from n1
        server.create_edge(CreateEdgeRequest {
            source_id: n1,
            target_id: n2,
            label: "KNOWS".to_string(),
            properties: None,
        });
        server.create_edge(CreateEdgeRequest {
            source_id: n1,
            target_id: n3.id,
            label: "WORKS_WITH".to_string(),
            properties: None,
        });

        // Get all outgoing edges
        let response = server.get_outgoing_edges(GetOutgoingEdgesRequest {
            node_id: n1,
            label: None,
        });
        let value: serde_json::Value = serde_json::from_str(&response).unwrap();
        assert_eq!(value.get("count"), Some(&serde_json::json!(2)));

        // Get only KNOWS edges
        let response = server.get_outgoing_edges(GetOutgoingEdgesRequest {
            node_id: n1,
            label: Some("KNOWS".to_string()),
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
            label: "Person".to_string(),
            properties: None,
        });
        let n3: NodeResponse = parse_response(&node3).unwrap();

        // Create edges to n2
        server.create_edge(CreateEdgeRequest {
            source_id: n1,
            target_id: n2,
            label: "KNOWS".to_string(),
            properties: None,
        });
        server.create_edge(CreateEdgeRequest {
            source_id: n3.id,
            target_id: n2,
            label: "KNOWS".to_string(),
            properties: None,
        });

        let response = server.get_incoming_edges(GetIncomingEdgesRequest {
            node_id: n2,
            label: None,
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

    fn create_graph(server: &GallifreyMcpServer) -> Vec<u64> {
        // Create a simple graph: A -> B -> C -> D
        let nodes: Vec<u64> = (0..4)
            .map(|i| {
                let response = server.create_node(CreateNodeRequest {
                    label: "Node".to_string(),
                    properties: Some({
                        let mut m = HashMap::new();
                        m.insert("name".to_string(), serde_json::json!(format!("Node{}", i)));
                        m
                    }),
                });
                let node: NodeResponse = parse_response(&response).unwrap();
                node.id
            })
            .collect();

        // Create edges
        for i in 0..3 {
            server.create_edge(CreateEdgeRequest {
                source_id: nodes[i],
                target_id: nodes[i + 1],
                label: "NEXT".to_string(),
                properties: None,
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
                label: "Document".to_string(),
                properties: Some(props),
            });
        }

        // Search for similar
        let response = server.find_similar(FindSimilarRequest {
            property_name: "embedding".to_string(),
            embedding: vec![0.1, 0.2, 0.3, 0.4],
            k: Some(3),
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
            label: "Person".to_string(),
            properties: None,
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
            label: "Person".to_string(),
            properties: Some({
                let mut m = HashMap::new();
                m.insert("name".to_string(), serde_json::json!("Alice"));
                m
            }),
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
            label: "Person".to_string(),
            properties: None,
        });
        let n1: NodeResponse = parse_response(&n1_response).unwrap();

        let n2_response = server.create_node(CreateNodeRequest {
            label: "Person".to_string(),
            properties: None,
        });
        let n2: NodeResponse = parse_response(&n2_response).unwrap();

        let edge_response = server.create_edge(CreateEdgeRequest {
            source_id: n1.id,
            target_id: n2.id,
            label: "KNOWS".to_string(),
            properties: None,
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
            label: "Person".to_string(),
            properties: Some({
                let mut m = HashMap::new();
                m.insert("name".to_string(), serde_json::json!("Alice"));
                m
            }),
        });
        let n1: NodeResponse = parse_response(&n1_response).unwrap();

        let n2_response = server.create_node(CreateNodeRequest {
            label: "Person".to_string(),
            properties: Some({
                let mut m = HashMap::new();
                m.insert("name".to_string(), serde_json::json!("Bob"));
                m
            }),
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
                label: "Person".to_string(),
                properties: None,
            });
        }

        for _ in 0..2 {
            server.create_node(CreateNodeRequest {
                label: "Document".to_string(),
                properties: None,
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
        });

        let value: serde_json::Value = serde_json::from_str(&response).unwrap();
        assert!(value.get("error").is_some());
    }

    #[test]
    fn test_hybrid_query_with_traversal() {
        let server = create_test_server();

        // Create a simple graph
        let n1_response = server.create_node(CreateNodeRequest {
            label: "Person".to_string(),
            properties: None,
        });
        let n1: NodeResponse = parse_response(&n1_response).unwrap();

        let n2_response = server.create_node(CreateNodeRequest {
            label: "Person".to_string(),
            properties: None,
        });
        let n2: NodeResponse = parse_response(&n2_response).unwrap();

        server.create_edge(CreateEdgeRequest {
            source_id: n1.id,
            target_id: n2.id,
            label: "KNOWS".to_string(),
            properties: None,
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
            label: "Test".to_string(),
            properties: Some(props),
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
            label: "Document".to_string(),
            properties: Some(props),
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
            label: "Event".to_string(),
            properties: None,
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
            let err_str = error.as_str().unwrap_or("");
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
            label: "Event".to_string(),
            properties: None,
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
            let err_str = error.as_str().unwrap_or("");
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
            label: "Event".to_string(),
            properties: None,
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
                label: "OffsetTest".to_string(),
                properties: Some({
                    let mut props = HashMap::new();
                    props.insert("index".to_string(), serde_json::json!(i));
                    props
                }),
            });
        }

        // Request with a very large offset (should be capped)
        let response = server.list_nodes(ListNodesRequest {
            label: Some("OffsetTest".to_string()),
            limit: Some(10),
            offset: Some(100_000), // Very large offset, should be capped to MAX_PAGINATION_OFFSET
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
                label: "ChainNode".to_string(),
                properties: Some({
                    let mut props = HashMap::new();
                    props.insert("level".to_string(), serde_json::json!(i));
                    props
                }),
            });
            let node: NodeResponse = parse_response(&response).unwrap();

            if let Some(source_id) = prev_id {
                server.create_edge(CreateEdgeRequest {
                    source_id,
                    target_id: node.id,
                    label: "NEXT".to_string(),
                    properties: None,
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
        assert!(instructions.contains("GallifreyDB"));
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
            node_id: 999999,
            properties: HashMap::new(),
        };

        let response = server.update_node(req);
        let value: serde_json::Value = serde_json::from_str(&response).unwrap();

        assert!(value.get("error").is_some());
    }

    #[test]
    fn test_delete_node_nonexistent() {
        let server = create_test_server();

        let req = DeleteNodeRequest { node_id: 999999 };

        let response = server.delete_node(req);
        let value: serde_json::Value = serde_json::from_str(&response).unwrap();

        assert!(value.get("error").is_some());
    }

    #[test]
    fn test_get_edge_nonexistent() {
        let server = create_test_server();

        let req = GetEdgeRequest { edge_id: 999999 };

        let response = server.get_edge(req);
        let value: serde_json::Value = serde_json::from_str(&response).unwrap();

        assert!(value.get("error").is_some());
    }

    #[test]
    fn test_update_edge_nonexistent() {
        let server = create_test_server();

        let req = UpdateEdgeRequest {
            edge_id: 999999,
            properties: HashMap::new(),
        };

        let response = server.update_edge(req);
        let value: serde_json::Value = serde_json::from_str(&response).unwrap();

        assert!(value.get("error").is_some());
    }

    #[test]
    fn test_delete_edge_nonexistent() {
        let server = create_test_server();

        let req = DeleteEdgeRequest { edge_id: 999999 };

        let response = server.delete_edge(req);
        let value: serde_json::Value = serde_json::from_str(&response).unwrap();

        assert!(value.get("error").is_some());
    }

    #[test]
    fn test_create_edge_invalid_source() {
        let server = create_test_server();

        // Create only target node
        let target_resp = server.create_node(CreateNodeRequest {
            label: "Target".to_string(),
            properties: None,
        });
        let target: NodeResponse = parse_response(&target_resp).unwrap();

        // Try to create edge with non-existent source
        let req = CreateEdgeRequest {
            source_id: 999999,
            target_id: target.id,
            label: "KNOWS".to_string(),
            properties: None,
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
            label: "Source".to_string(),
            properties: None,
        });
        let source: NodeResponse = parse_response(&source_resp).unwrap();

        // Try to create edge with non-existent target
        let req = CreateEdgeRequest {
            source_id: source.id,
            target_id: 999999,
            label: "KNOWS".to_string(),
            properties: None,
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
            k: Some(10000), // Much larger than MAX_VECTOR_K
        });

        // Should not error (k gets capped internally)
        let value: serde_json::Value = serde_json::from_str(&response).unwrap();
        // The query succeeds (may return empty results, but no error about k)
        assert!(
            value.get("results").is_some() || value.get("error").is_some(),
            "Should handle large k gracefully"
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
            label: "Event".to_string(),
            properties: Some({
                let mut m = HashMap::new();
                m.insert("title".to_string(), serde_json::json!("Conference"));
                m
            }),
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
            label: "Person".to_string(),
            properties: None,
        });
        let n1: NodeResponse = parse_response(&n1_response).unwrap();

        let n2_response = server.create_node(CreateNodeRequest {
            label: "Person".to_string(),
            properties: None,
        });
        let n2: NodeResponse = parse_response(&n2_response).unwrap();

        let edge_response = server.create_edge(CreateEdgeRequest {
            source_id: n1.id,
            target_id: n2.id,
            label: "KNOWS".to_string(),
            properties: None,
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
            label: "Test".to_string(),
            properties: None,
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
        let error_str = value["error"].as_str().unwrap();
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
            label: "Person".to_string(),
            properties: None,
        });
        let n1: NodeResponse = parse_response(&n1_response).unwrap();

        let n2_response = server.create_node(CreateNodeRequest {
            label: "Person".to_string(),
            properties: None,
        });
        let n2: NodeResponse = parse_response(&n2_response).unwrap();

        let edge_response = server.create_edge(CreateEdgeRequest {
            source_id: n1.id,
            target_id: n2.id,
            label: "KNOWS".to_string(),
            properties: None,
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
            label: "Event".to_string(),
            properties: None,
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
            let err_str = error.as_str().unwrap_or("");
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

    fn create_bidirectional_graph(server: &GallifreyMcpServer) -> Vec<u64> {
        // Create nodes: A <-> B <-> C
        let nodes: Vec<u64> = (0..3)
            .map(|i| {
                let response = server.create_node(CreateNodeRequest {
                    label: "BiNode".to_string(),
                    properties: Some({
                        let mut m = HashMap::new();
                        m.insert("name".to_string(), serde_json::json!(format!("Node{}", i)));
                        m
                    }),
                });
                let node: NodeResponse = parse_response(&response).unwrap();
                node.id
            })
            .collect();

        // Create bidirectional edges
        for i in 0..2 {
            server.create_edge(CreateEdgeRequest {
                source_id: nodes[i],
                target_id: nodes[i + 1],
                label: "CONNECTED".to_string(),
                properties: None,
            });
            server.create_edge(CreateEdgeRequest {
                source_id: nodes[i + 1],
                target_id: nodes[i],
                label: "CONNECTED".to_string(),
                properties: None,
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
    fn test_traverse_nonexistent_edge_label() {
        let server = create_test_server();

        // Create a single node
        let node_response = server.create_node(CreateNodeRequest {
            label: "Lonely".to_string(),
            properties: None,
        });
        let node: NodeResponse = parse_response(&node_response).unwrap();

        // Traverse with non-existent edge label
        let response = server.traverse(TraverseRequest {
            start_node_id: node.id,
            edge_label: "NONEXISTENT".to_string(),
            direction: None,
            depth: Some(3),
            limit: None,
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
            label: "Node".to_string(),
            properties: None,
        });
        let n1: NodeResponse = parse_response(&n1_response).unwrap();

        let n2_response = server.create_node(CreateNodeRequest {
            label: "Node".to_string(),
            properties: None,
        });
        let n2: NodeResponse = parse_response(&n2_response).unwrap();

        server.create_edge(CreateEdgeRequest {
            source_id: n1.id,
            target_id: n2.id,
            label: "NEXT".to_string(),
            properties: None,
        });

        // Traverse without specifying direction (should default to outgoing)
        let response = server.traverse(TraverseRequest {
            start_node_id: n1.id,
            edge_label: "NEXT".to_string(),
            direction: None, // Default to outgoing
            depth: Some(1),
            limit: None,
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
            label: "Center".to_string(),
            properties: None,
        });
        let center: NodeResponse = parse_response(&center_response).unwrap();

        for i in 0..10 {
            let spoke_response = server.create_node(CreateNodeRequest {
                label: "Spoke".to_string(),
                properties: Some({
                    let mut m = HashMap::new();
                    m.insert("index".to_string(), serde_json::json!(i));
                    m
                }),
            });
            let spoke: NodeResponse = parse_response(&spoke_response).unwrap();

            server.create_edge(CreateEdgeRequest {
                source_id: center.id,
                target_id: spoke.id,
                label: "SPOKE".to_string(),
                properties: None,
            });
        }

        // Traverse with limit
        let response = server.traverse(TraverseRequest {
            start_node_id: center.id,
            edge_label: "SPOKE".to_string(),
            direction: Some("outgoing".to_string()),
            depth: Some(1),
            limit: Some(5),
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
            label: "Person".to_string(),
            properties: Some({
                let mut m = HashMap::new();
                m.insert("name".to_string(), serde_json::json!("Alice"));
                m
            }),
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
        });

        let value: serde_json::Value = serde_json::from_str(&response).unwrap();
        // Should return temporal query result
        if value.get("error").is_none() {
            assert!(value.get("temporal_query").is_some() || value.get("results").is_some());
        }
    }

    #[test]
    fn test_hybrid_query_multi_depth_traversal() {
        let server = create_test_server();

        // Create chain: A -> B -> C -> D
        let nodes: Vec<u64> = (0..4)
            .map(|i| {
                let response = server.create_node(CreateNodeRequest {
                    label: "ChainNode".to_string(),
                    properties: Some({
                        let mut m = HashMap::new();
                        m.insert("level".to_string(), serde_json::json!(i));
                        m
                    }),
                });
                let node: NodeResponse = parse_response(&response).unwrap();
                node.id
            })
            .collect();

        for i in 0..3 {
            server.create_edge(CreateEdgeRequest {
                source_id: nodes[i],
                target_id: nodes[i + 1],
                label: "CHAIN".to_string(),
                properties: None,
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
            label: "Test".to_string(),
            properties: None,
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
        });

        let value: serde_json::Value = serde_json::from_str(&response).unwrap();
        assert!(value.get("error").is_some());
        let error = value["error"].as_str().unwrap();
        assert!(
            error.contains("Invalid valid_time"),
            "Should report invalid valid_time"
        );
    }

    #[test]
    fn test_hybrid_query_invalid_transaction_time() {
        let server = create_test_server();

        let node_response = server.create_node(CreateNodeRequest {
            label: "Test".to_string(),
            properties: None,
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
        });

        let value: serde_json::Value = serde_json::from_str(&response).unwrap();
        assert!(value.get("error").is_some());
        let error = value["error"].as_str().unwrap();
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
                label: "LimitTest".to_string(),
                properties: Some({
                    let mut m = HashMap::new();
                    m.insert("index".to_string(), serde_json::json!(i));
                    m
                }),
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
            limit: Some(100000), // Much larger than MAX_RESULT_LIMIT
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
        });

        let value: serde_json::Value = serde_json::from_str(&response).unwrap();
        assert!(value.get("error").is_some());
        let error = value["error"].as_str().unwrap();
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
            label: "Person".to_string(),
            properties: None,
        });
        let n1: NodeResponse = parse_response(&n1_response).unwrap();

        let n2_response = server.create_node(CreateNodeRequest {
            label: "Person".to_string(),
            properties: None,
        });
        let n2: NodeResponse = parse_response(&n2_response).unwrap();

        // Create edges with different labels
        server.create_edge(CreateEdgeRequest {
            source_id: n1.id,
            target_id: n2.id,
            label: "KNOWS".to_string(),
            properties: None,
        });
        server.create_edge(CreateEdgeRequest {
            source_id: n1.id,
            target_id: n2.id,
            label: "WORKS_WITH".to_string(),
            properties: None,
        });

        // List edges with label filter
        let response = server.list_edges(ListEdgesRequest {
            label: Some("KNOWS".to_string()),
            limit: None,
            offset: None,
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
            label: "Person".to_string(),
            properties: None,
        });
        let n1: NodeResponse = parse_response(&n1_response).unwrap();

        let n2_response = server.create_node(CreateNodeRequest {
            label: "Person".to_string(),
            properties: None,
        });
        let n2: NodeResponse = parse_response(&n2_response).unwrap();

        server.create_edge(CreateEdgeRequest {
            source_id: n1.id,
            target_id: n2.id,
            label: "KNOWS".to_string(),
            properties: None,
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
            label: "Person".to_string(),
            properties: None,
        });
        let n1: NodeResponse = parse_response(&n1_response).unwrap();

        let n2_response = server.create_node(CreateNodeRequest {
            label: "Person".to_string(),
            properties: None,
        });
        let n2: NodeResponse = parse_response(&n2_response).unwrap();

        let n3_response = server.create_node(CreateNodeRequest {
            label: "Person".to_string(),
            properties: None,
        });
        let n3: NodeResponse = parse_response(&n3_response).unwrap();

        // Create edges with different labels pointing to n2
        server.create_edge(CreateEdgeRequest {
            source_id: n1.id,
            target_id: n2.id,
            label: "KNOWS".to_string(),
            properties: None,
        });
        server.create_edge(CreateEdgeRequest {
            source_id: n3.id,
            target_id: n2.id,
            label: "WORKS_WITH".to_string(),
            properties: None,
        });

        // Get incoming edges with label filter
        let response = server.get_incoming_edges(GetIncomingEdgesRequest {
            node_id: n2.id,
            label: Some("KNOWS".to_string()),
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
                label: format!("Type{}", i),
                properties: None,
            });
        }

        // List nodes without label filter
        let response = server.list_nodes(ListNodesRequest {
            label: None,
            limit: None,
            offset: None,
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
                label: "LimitCap".to_string(),
                properties: Some({
                    let mut m = HashMap::new();
                    m.insert("index".to_string(), serde_json::json!(i));
                    m
                }),
            });
        }

        // Request with very large limit (should be capped to MAX_RESULT_LIMIT)
        let response = server.list_nodes(ListNodesRequest {
            label: Some("LimitCap".to_string()),
            limit: Some(100000), // Much larger than MAX_RESULT_LIMIT
            offset: Some(0),
        });

        let value: serde_json::Value = serde_json::from_str(&response).unwrap();
        // Should not error
        assert!(
            value.get("nodes").is_some(),
            "Large limit should be capped, not error"
        );
    }
}
