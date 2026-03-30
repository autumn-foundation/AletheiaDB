//! Hybrid Query Functions (VS-063)
//!
//! This module provides direct hybrid query functions that combine graph traversal
//! with vector similarity ranking. These are lower-level functions compared to the
//! query builder API, offering more direct access to hybrid operations.
//!
//! # Example Usage
//!
//! ```rust,ignore
//! use aletheiadb::query::hybrid::traverse_and_rank;
//! use aletheiadb::AletheiaDB;
//!
//! let db = AletheiaDB::new();
//! // ... populate database with nodes and embeddings ...
//!
//! // Find neighbors similar to a target embedding
//! let results = traverse_and_rank(
//!     &db,
//!     alice_id,
//!     "KNOWS",
//!     &target_embedding,
//!     10
//! )?;
//!
//! for (node_id, similarity) in results {
//!     println!("Node {:?}: similarity = {}", node_id, similarity);
//! }
//! ```

use crate::core::error::Result;
use crate::core::id::NodeId;
use crate::core::vector::{cosine_similarity, validate_vector};
use crate::query::traits::GraphView;
use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashSet};

/// A candidate node with its similarity score, ordered by similarity (min-heap).
///
/// This is used internally for efficient top-k tracking with a BinaryHeap.
/// The `Ord` implementation is reversed to create a min-heap (lowest similarity at the top).
#[derive(Debug, Clone, PartialEq)]
struct ScoredCandidate {
    node_id: NodeId,
    similarity: f32,
}

impl ScoredCandidate {
    fn new(node_id: NodeId, similarity: f32) -> Self {
        Self {
            node_id,
            similarity,
        }
    }
}

impl Eq for ScoredCandidate {}

impl PartialOrd for ScoredCandidate {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for ScoredCandidate {
    fn cmp(&self, other: &Self) -> Ordering {
        // Reverse ordering for min-heap (lower similarity = higher priority to pop)
        other
            .similarity
            .partial_cmp(&self.similarity)
            .unwrap_or(Ordering::Equal)
    }
}

/// Traverse graph from a starting node and rank results by vector similarity.
///
/// This function performs a hybrid graph+vector query:
/// 1. Traverse outgoing edges from the start node matching the edge label
/// 2. For each neighbor, compute similarity to the target embedding
/// 3. Return top-k neighbors ranked by similarity (highest first)
///
/// # Arguments
///
/// * `db` - Database instance
/// * `start` - Starting node ID for traversal
/// * `edge_label` - Edge label to traverse (e.g., "KNOWS", "LINKS_TO")
/// * `target_embedding` - Target embedding vector to rank by similarity
/// * `k` - Maximum number of results to return
///
/// # Returns
///
/// A vector of (NodeId, similarity_score) tuples, sorted by similarity in descending order.
/// The similarity score is cosine similarity in the range [-1, 1], where higher is more similar.
///
/// # Errors
///
/// - `Error::Storage(StorageError::NodeNotFound)` if the start node doesn't exist
/// - `Error::Vector(VectorError::*)` if the target embedding is invalid
/// - `Error::Storage(*)` if database access fails
///
/// # Behavior Notes
///
/// - Nodes without the "embedding" property are silently skipped (no error)
/// - Nodes with embeddings of mismatched dimensions are skipped with a warning
/// - Cycles in the graph are handled by visiting each node only once
/// - If fewer than k neighbors exist, returns all available neighbors
/// - Self-loops are traversed normally (start node can be in results if it has self-edge)
///
/// # Examples
///
/// ```rust,ignore
/// // Find Alice's friends most similar to Bob
/// let bob_embedding = db.get_node(bob_id)?.get_property("embedding")?.as_vector()?;
/// let similar_friends = traverse_and_rank(&db, alice_id, "KNOWS", bob_embedding, 5)?;
/// ```
pub fn traverse_and_rank<G: GraphView + ?Sized>(
    db: &G,
    start: NodeId,
    edge_label: &str,
    target_embedding: &[f32],
    k: usize,
) -> Result<Vec<(NodeId, f32)>> {
    // Validate target embedding
    validate_vector(target_embedding)?;

    // Check that start node exists
    let _start_node = db.get_node(start)?;

    // Get all outgoing edges from start node with matching label
    let edge_ids = db.get_outgoing_edges_with_label(start, edge_label);

    // Use a min-heap to track top-k candidates efficiently (O(N log k) instead of O(N log N))
    let mut top_k_heap = BinaryHeap::with_capacity(k);

    // Pre-allocate visited set for cycle detection
    let mut visited = HashSet::with_capacity(edge_ids.len().min(k * 2));

    for edge_id in edge_ids {
        // Zero-copy: only get target NodeId, not full Edge (Issue #190)
        let target_id = db.get_edge_target(edge_id)?;

        // Handle cycles: skip if already visited
        if visited.contains(&target_id) {
            continue;
        }
        visited.insert(target_id);

        // Get target node
        let target_node = match db.get_node(target_id) {
            Ok(node) => node,
            Err(_) => continue, // Skip if node doesn't exist (shouldn't happen)
        };

        // Get embedding from node (skip nodes without embeddings)
        let embedding = match target_node.get_property("embedding") {
            Some(prop) => match prop.as_vector() {
                Some(vec) => vec,
                None => continue, // Property exists but isn't a vector
            },
            None => continue, // No embedding property
        };

        // Compute cosine similarity
        match cosine_similarity(target_embedding, embedding) {
            Ok(similarity) => {
                let candidate = ScoredCandidate::new(target_id, similarity);

                if top_k_heap.len() < k {
                    // Heap not full yet, add candidate
                    top_k_heap.push(candidate);
                } else if let Some(min_candidate) = top_k_heap.peek() {
                    // Heap is full, only add if better than current minimum
                    if similarity > min_candidate.similarity {
                        top_k_heap.pop(); // Remove minimum
                        top_k_heap.push(candidate); // Add new candidate
                    }
                }
            }
            Err(_e) => {
                // Skip nodes with dimension mismatch or invalid embeddings (with warning)
                #[cfg(feature = "observability")]
                tracing::warn!(
                    target_id = %target_id,
                    error = %_e,
                    "Skipping node in traverse_and_rank due to incompatible embedding"
                );
                continue;
            }
        }
    }

    // Convert heap to sorted vector (highest similarity first)
    let mut results: Vec<(NodeId, f32)> = top_k_heap
        .into_iter()
        .map(|c| (c.node_id, c.similarity))
        .collect();

    // Sort in descending order (highest similarity first)
    results.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(Ordering::Equal));

    Ok(results)
}

/// Find k most similar nodes at a specific point in time.
///
/// This function performs a temporal vector search, finding nodes with embeddings
/// most similar to the query embedding as they existed at the specified timestamp.
///
/// # Arguments
///
/// * `db` - Database instance
/// * `embedding` - Query embedding vector to search for
/// * `k` - Maximum number of results to return
/// * `timestamp` - Point in time to query (in microseconds since epoch)
///
/// # Returns
///
/// A vector of (NodeId, similarity_score) tuples, sorted by similarity in descending order.
/// The similarity score is cosine similarity in the range [-1, 1], where higher is more similar.
///
/// # Errors
///
/// - `Error::Vector(VectorError::IndexError)` if temporal vector index is not enabled
/// - `Error::Vector(VectorError::*)` if the query embedding is invalid
/// - `Error::Temporal(*)` if the timestamp is invalid or no snapshot exists
///
/// # Behavior Notes
///
/// - Requires temporal vector indexing to be enabled via `enable_temporal_vector_index()`
/// - Returns results from the nearest snapshot at or before the given timestamp
/// - If no snapshots exist at the given timestamp, returns an error
/// - Empty results indicate no vectors existed at that timestamp (not an error)
///
/// # Examples
///
/// ```rust,ignore
/// use aletheiadb::query::hybrid::find_similar_as_of;
/// use aletheiadb::core::temporal::time;
///
/// // Find documents similar to a query embedding at a specific timestamp
/// let query_embedding = vec![0.1f32; 384];
/// let timestamp_2023 = 1672531200000000; // 2023-01-01 in microseconds
/// let similar_docs = find_similar_as_of(&db, &query_embedding, 10, timestamp_2023)?;
///
/// for (node_id, similarity) in similar_docs {
///     println!("Document {:?} was similar at that time: {:.3}", node_id, similarity);
/// }
/// ```
pub fn find_similar_as_of<G: GraphView + ?Sized>(
    db: &G,
    embedding: &[f32],
    k: usize,
    timestamp: crate::core::temporal::Timestamp,
) -> Result<Vec<(NodeId, f32)>> {
    // Validate embedding
    validate_vector(embedding)?;

    // Delegate to database method
    db.find_similar_as_of(embedding, k, timestamp)
}
