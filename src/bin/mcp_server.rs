//! GallifreyDB MCP Server binary.
//!
//! This binary exposes GallifreyDB's bi-temporal graph database capabilities
//! through the Model Context Protocol (MCP), enabling LLMs like Claude to
//! interact with the database.
//!
//! # Usage
//!
//! ```bash
//! cargo run --bin gallifrey-mcp --features mcp-server
//! ```
//!
//! The server communicates over stdio using the MCP protocol.
//!
//! # Available Tools
//!
//! ## Node Operations
//! - `get_node`: Get a node by ID
//! - `create_node`: Create a new node with label and properties
//! - `update_node`: Update node properties
//! - `delete_node`: Delete a node
//! - `list_nodes`: List nodes with optional filtering
//! - `count_nodes`: Count nodes
//!
//! ## Edge Operations
//! - `get_edge`: Get an edge by ID
//! - `create_edge`: Create an edge between nodes
//! - `update_edge`: Update edge properties
//! - `delete_edge`: Delete an edge
//! - `list_edges`: List edges with optional filtering
//! - `count_edges`: Count edges
//! - `get_outgoing_edges`: Get outgoing edges from a node
//! - `get_incoming_edges`: Get incoming edges to a node
//!
//! ## Graph Traversal
//! - `traverse`: Traverse the graph from a starting node
//!
//! ## Vector Search
//! - `find_similar`: Find similar nodes by embedding
//! - `enable_vector_index`: Enable vector indexing on a property
//! - `list_vector_indexes`: List enabled vector indexes
//!
//! ## Temporal Queries
//! - `get_node_at_time`: Get node state at a specific time
//! - `get_edge_at_time`: Get edge state at a specific time
//!
//! ## Hybrid Queries
//! - `hybrid_query`: Combined graph + vector + temporal query

use std::sync::Arc;

use rmcp::{ServiceExt, transport::stdio};

use gallifreydb::GallifreyDB;
use gallifreydb::mcp::GallifreyMcpServer;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // Initialize the database
    let db = GallifreyDB::new()?;

    // Create the MCP server
    let server = GallifreyMcpServer::new(Arc::new(db));

    // Serve over stdio using the MCP protocol
    let service = server.serve(stdio()).await?;

    // Wait for the server to shut down
    service.waiting().await?;

    Ok(())
}
