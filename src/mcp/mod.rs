//! MCP (Model Context Protocol) server for GallifreyDB.
//!
//! This module exposes GallifreyDB's capabilities through the Model Context Protocol,
//! enabling LLMs like Claude to interact with the database for:
//! - Graph operations (nodes, edges, traversals)
//! - Vector similarity search
//! - Bi-temporal queries
//! - Hybrid queries combining all three
//!
//! # Quick Start
//!
//! Run the MCP server as a binary:
//! ```bash
//! cargo run --bin gallifrey-mcp --features mcp-server
//! ```
//!
//! Or use it programmatically:
//! ```ignore
//! use gallifreydb::mcp::GallifreyMcpServer;
//! use gallifreydb::GallifreyDB;
//! use std::sync::Arc;
//!
//! let db = Arc::new(GallifreyDB::new()?);
//! let server = GallifreyMcpServer::new(db);
//! server.serve_stdio().await?;
//! ```

mod server;
mod tools;

pub use server::GallifreyMcpServer;

// Re-export tool request/response types for testing (alphabetically sorted)
pub use tools::{
    CountEdgesRequest, CountNodesRequest, CreateEdgeRequest, CreateNodeRequest, DeleteEdgeRequest,
    DeleteNodeRequest, EnableVectorIndexRequest, FindSimilarRequest, GetEdgeAtTimeRequest,
    GetEdgeRequest, GetIncomingEdgesRequest, GetNodeAtTimeRequest, GetNodeRequest,
    GetOutgoingEdgesRequest, HybridQueryRequest, ListEdgesRequest, ListNodesRequest,
    ListVectorIndexesRequest, TraverseRequest, UpdateEdgeRequest, UpdateNodeRequest,
};

#[cfg(test)]
mod tests;
