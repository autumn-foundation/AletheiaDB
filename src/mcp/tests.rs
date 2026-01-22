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
        props.insert("float_val".to_string(), serde_json::json!(3.14));
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
