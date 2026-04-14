//! MCP (Model Context Protocol) server for AletheiaDB.
//!
//! This module exposes AletheiaDB's capabilities through the Model Context Protocol,
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
//! cargo run --bin aletheia-mcp --features mcp-server
//! ```
//!
//! Or use it programmatically:
//! ```ignore
//! use aletheiadb::mcp::AletheiaMcpServer;
//! use aletheiadb::AletheiaDB;
//! use std::sync::Arc;
//!
//! let db = Arc::new(AletheiaDB::new()?);
//! let server = AletheiaMcpServer::new(db);
//! server.serve_stdio().await?;
//! ```

mod server;
mod tools;

pub use server::AletheiaMcpServer;

// Re-export tool request/response types for testing (alphabetically sorted)
pub use tools::{
    CountEdgesRequest, CountNodesRequest, CreateEdgeRequest, CreateNodeRequest, DeleteEdgeRequest,
    DeleteNodeCascadeRequest, DeleteNodeRequest, EnableVectorIndexRequest, FindSimilarRequest,
    GetEdgeAtTimeRequest, GetEdgeRequest, GetIncomingEdgesRequest, GetNodeAtTimeRequest,
    GetNodeRequest, GetOutgoingEdgesRequest, HybridQueryRequest, ListEdgesRequest,
    ListNodesRequest, ListVectorIndexesRequest, TraverseRequest, UpdateEdgeRequest,
    UpdateNodeRequest,
};

#[cfg(test)]
mod tests;
