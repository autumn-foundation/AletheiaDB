//! Semantic Pathfinding Module (Experimental)
//!
//! This module implements pathfinding algorithms that consider semantic similarity
//! (vector embeddings) as part of the cost function. Instead of just finding the shortest
//! structural path between two nodes, semantic pathfinding finds the *most contextually relevant*
//! path, allowing queries to be guided by a concept.
//!
//! # Features
//! - **Semantic A***: Finds paths where intermediate nodes are semantically similar to a query concept.
//! - **Time-Travel Pathfinding**: Finds paths that were valid at a specific historical point in time.
//!
//! # Examples
//!
//! ```no_run
//! use aletheiadb::db::AletheiaDB;
//! use aletheiadb::core::property::PropertyMapBuilder;
//! use aletheiadb::query::semantic_pathfinding::SemanticPathfinder;
//! use aletheiadb::index::vector::{HnswConfig, DistanceMetric};
//! use aletheiadb::api::transaction::WriteOps;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! // 1. Setup Database with vector index
//! let db = AletheiaDB::new()?;
//! db.vector_index("embedding")
//!     .hnsw(HnswConfig::new(3, DistanceMetric::Cosine))
//!     .enable()?;
//!
//! // 2. Create nodes with vector embeddings
//! let start = db.create_node("Start", PropertyMapBuilder::new().insert_vector("embedding", &[0.5, 0.5, 0.0]).build())?;
//! let end = db.create_node("End", PropertyMapBuilder::new().insert_vector("embedding", &[0.5, 0.5, 0.0]).build())?;
//!
//! // Intermediate nodes
//! let apple = db.create_node("Apple", PropertyMapBuilder::new().insert_vector("embedding", &[1.0, 0.0, 0.0]).build())?; // Fruit
//! let laptop = db.create_node("Laptop", PropertyMapBuilder::new().insert_vector("embedding", &[0.0, 1.0, 0.0]).build())?; // Tech
//!
//! // Create edges: Start -> [Apple, Laptop] -> End
//! db.create_edge(start, apple, "NEXT", PropertyMapBuilder::new().build())?;
//! db.create_edge(apple, end, "NEXT", PropertyMapBuilder::new().build())?;
//! db.create_edge(start, laptop, "NEXT", PropertyMapBuilder::new().build())?;
//! db.create_edge(laptop, end, "NEXT", PropertyMapBuilder::new().build())?;
//!
//! // 3. Find path using a query vector (e.g., "Banana", which is closer to "Apple" than "Laptop")
//! let query_embedding = vec![0.9, 0.1, 0.0];
//! let pathfinder = SemanticPathfinder::new(&db, "embedding");
//!
//! // The pathfinder will prefer the route through 'Apple' because it's semantically closer
//! let path = pathfinder.find_path(start, end, &query_embedding, 10, false)?.unwrap();
//! assert_eq!(path, vec![start, apple, end]);
//! # Ok(())
//! # }
//! ```
//!
//! # Errors
//! Methods in this module will return a [`Result`] indicating failure if:
//! - An underlying database read operation fails.
//! - Property maps contain malformed vector properties.
//!
//! # Panics
//! This module does not intentionally panic. If any node has a vector that differs in dimension from the `query_embedding`,
//! it treats the cost as infinite (effectively blocking the path) instead of panicking.

use crate::core::error::Result;
use crate::core::hasher::IdentityHasher;
use crate::core::id::NodeId;
use crate::core::temporal::Timestamp;
use crate::core::vector::cosine_similarity;
use crate::query::traits::GraphView;
use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashMap};
use std::hash::BuildHasherDefault;

/// Per-hop structural cost to prefer shorter paths when semantic costs are equal.
const STRUCTURAL_HOP_COST: f32 = 0.1;

/// Default cost assigned to nodes without embeddings.
const NO_EMBEDDING_COST: f32 = 1.0;

/// State for priority queue (min-heap based on cost).
#[derive(Debug, Clone, Copy, PartialEq)]
struct State {
    cost: f32,
    node: NodeId,
    depth: usize,
}

impl Eq for State {}

impl Ord for State {
    fn cmp(&self, other: &Self) -> Ordering {
        // Reverse order for min-heap (we want smallest cost)
        // Handle NaN by treating it as "greater than" (lowest priority)
        if self.cost.is_nan() {
            return Ordering::Less; // self > other (in reverse logic)
        }
        if other.cost.is_nan() {
            return Ordering::Greater; // other > self
        }
        other
            .cost
            .partial_cmp(&self.cost)
            .unwrap_or(Ordering::Equal)
    }
}

impl PartialOrd for State {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// A pathfinder that uses semantic similarity as a heuristic/cost.
pub struct SemanticPathfinder<'a, G: GraphView + ?Sized> {
    db: &'a G,
    vector_property: String,
}

impl<'a, G: GraphView + ?Sized> SemanticPathfinder<'a, G> {
    /// Creates a new [`SemanticPathfinder`].
    ///
    /// # Arguments
    /// * `db` - Reference to the graph view (e.g., database instance).
    /// * `vector_property` - Name of the property containing vector embeddings.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use aletheiadb::db::AletheiaDB;
    /// use aletheiadb::query::semantic_pathfinding::SemanticPathfinder;
    ///
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// let db = AletheiaDB::new()?;
    /// let pathfinder = SemanticPathfinder::new(&db, "embedding");
    /// # Ok(())
    /// # }
    /// ```
    pub fn new(db: &'a G, vector_property: &str) -> Self {
        Self {
            db,
            vector_property: vector_property.to_string(),
        }
    }

    /// Finds a path from `start` to `end` that minimizes semantic distance to the `query_embedding`.
    ///
    /// Uses Dijkstra's algorithm where the edge weight is calculated as:
    /// `1.0 - cosine_similarity(target_node, query_embedding)`.
    /// This heavily favors paths through nodes that are semantically relevant to the query concept.
    /// A small structural cost (0.1) is added per hop to prefer shorter paths when semantics are equal.
    ///
    /// # Arguments
    /// * `start` - Starting [`NodeId`].
    /// * `end` - Target [`NodeId`].
    /// * `query_embedding` - Vector representing the semantic concept to follow.
    /// * `max_depth` - Maximum path length (to prevent infinite loops in large graphs).
    /// * `bidirectional` - If `true`, traverses both outgoing and incoming edges.
    ///
    /// # Returns
    /// An `Ok(Some(Vec<NodeId>))` representing the path from start to end, or `Ok(None)` if no valid path exists within the `max_depth`.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use aletheiadb::db::AletheiaDB;
    /// use aletheiadb::core::id::NodeId;
    /// use aletheiadb::query::semantic_pathfinding::SemanticPathfinder;
    ///
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// let db = AletheiaDB::new()?;
    /// let pathfinder = SemanticPathfinder::new(&db, "embedding");
    ///
    /// let start = NodeId::new(1).unwrap();
    /// let end = NodeId::new(2).unwrap();
    /// let concept = vec![0.5, 0.5, 0.0];
    ///
    /// if let Some(path) = pathfinder.find_path(start, end, &concept, 5, true)? {
    ///     println!("Found semantic path: {:?}", path);
    /// }
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// # Errors
    /// Returns an error if an underlying database operation fails.
    pub fn find_path(
        &self,
        start: NodeId,
        end: NodeId,
        query_embedding: &[f32],
        max_depth: usize,
        bidirectional: bool,
    ) -> Result<Option<Vec<NodeId>>> {
        let mut pq = BinaryHeap::new();
        // ⚡ Bolt: Use `IdentityHasher` instead of the default SipHash.
        // `NodeId` is already a unique wrapper over `u64`. In pathfinding (like A*),
        // we perform thousands of map insertions and lookups. Bypassing SipHash eliminates
        // unnecessary hashing overhead on already-unique integer keys.
        let mut dist: HashMap<NodeId, f32, BuildHasherDefault<IdentityHasher>> = HashMap::default();
        let mut came_from: HashMap<NodeId, NodeId, BuildHasherDefault<IdentityHasher>> =
            HashMap::default();

        // Initialize start node
        dist.insert(start, 0.0);
        pq.push(State {
            cost: 0.0,
            node: start,
            depth: 0,
        });

        // Reuse allocation across iterations
        // ⚡ Bolt: Pre-allocate neighbor vector with typical branching factor (32) to avoid standard 0->4->8->16->32 heap reallocation chain.
        let mut neighbors = Vec::with_capacity(32);

        while let Some(State { cost, node, depth }) = pq.pop() {
            if node == end {
                return Ok(Some(self.reconstruct_path(came_from, end)));
            }

            // Optimization: Skip if we found a better path already
            #[allow(clippy::collapsible_if)]
            if let Some(&d) = dist.get(&node) {
                if cost > d {
                    continue;
                }
            }

            // Check depth limit
            if depth >= max_depth {
                continue;
            }

            // Collect neighbors (outgoing, or both directions if bidirectional)
            neighbors.clear();

            // Get outgoing edges
            for edge_id in self.db.get_outgoing_edges(node) {
                if let Ok(target) = self.db.get_edge_target(edge_id) {
                    neighbors.push(target);
                }
            }

            // Also get incoming edges if bidirectional
            if bidirectional {
                for edge_id in self.db.get_incoming_edges(node) {
                    if let Ok(edge) = self.db.get_edge(edge_id) {
                        neighbors.push(edge.source);
                    }
                }
            }

            // Process all neighbors
            for &target in &neighbors {
                // Calculate semantic cost of moving to target
                let semantic_cost = self.calculate_semantic_cost(target, query_embedding)?;

                let new_cost = cost + semantic_cost + STRUCTURAL_HOP_COST;

                if new_cost < *dist.get(&target).unwrap_or(&f32::INFINITY) {
                    dist.insert(target, new_cost);
                    came_from.insert(target, node);
                    pq.push(State {
                        cost: new_cost,
                        node: target,
                        depth: depth + 1,
                    });
                }
            }
        }

        Ok(None)
    }

    /// Finds a path at a specific point in time.
    ///
    /// Similar to [`find_path`](Self::find_path), but only considers nodes and edges valid at the specified `time`.
    /// Uses the temporal adjacency index to find edges that existed at the specified
    /// time, even if they have since been deleted from current storage.
    ///
    /// # Arguments
    /// * `start` - Starting [`NodeId`].
    /// * `end` - Target [`NodeId`].
    /// * `query_embedding` - Vector representing the semantic concept to follow.
    /// * `time` - The [`Timestamp`] at which to evaluate the graph state.
    /// * `max_depth` - Maximum path length (to prevent infinite loops in large graphs).
    /// * `bidirectional` - If `true`, traverses both outgoing and incoming edges.
    ///
    /// # Returns
    /// An `Ok(Some(Vec<NodeId>))` representing the path from start to end as it existed at `time`, or `Ok(None)` if no valid path existed within the `max_depth`.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use aletheiadb::db::AletheiaDB;
    /// use aletheiadb::core::id::NodeId;
    /// use aletheiadb::core::temporal::Timestamp;
    /// use aletheiadb::query::semantic_pathfinding::SemanticPathfinder;
    ///
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// let db = AletheiaDB::new()?;
    /// let pathfinder = SemanticPathfinder::new(&db, "embedding");
    ///
    /// let start = NodeId::new(1).unwrap();
    /// let end = NodeId::new(2).unwrap();
    /// let concept = vec![0.5, 0.5, 0.0];
    /// let historical_time: Timestamp = 1622505600.into();
    ///
    /// if let Some(path) = pathfinder.find_path_at_time(start, end, &concept, historical_time, 5, false)? {
    ///     println!("Found historical semantic path: {:?}", path);
    /// }
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// # Errors
    /// Returns an error if an underlying database operation fails.
    pub fn find_path_at_time(
        &self,
        start: NodeId,
        end: NodeId,
        query_embedding: &[f32],
        time: Timestamp,
        max_depth: usize,
        bidirectional: bool,
    ) -> Result<Option<Vec<NodeId>>> {
        let mut pq = BinaryHeap::new();
        // ⚡ Bolt: Use `IdentityHasher` instead of the default SipHash.
        // `NodeId` is already a unique wrapper over `u64`. In pathfinding (like A*),
        // we perform thousands of map insertions and lookups. Bypassing SipHash eliminates
        // unnecessary hashing overhead on already-unique integer keys.
        let mut dist: HashMap<NodeId, f32, BuildHasherDefault<IdentityHasher>> = HashMap::default();
        let mut came_from: HashMap<NodeId, NodeId, BuildHasherDefault<IdentityHasher>> =
            HashMap::default();

        // Verify start node existed
        if self.db.get_node_at_time(start, time, time).is_err() {
            return Ok(None);
        }

        dist.insert(start, 0.0);
        pq.push(State {
            cost: 0.0,
            node: start,
            depth: 0,
        });

        // Reuse allocation across iterations
        // ⚡ Bolt: Pre-allocate neighbor vector with typical branching factor (32) to avoid standard 0->4->8->16->32 heap reallocation chain.
        let mut neighbor_edges = Vec::with_capacity(32);

        while let Some(State { cost, node, depth }) = pq.pop() {
            if node == end {
                return Ok(Some(self.reconstruct_path(came_from, end)));
            }

            #[allow(clippy::collapsible_if)]
            if let Some(&d) = dist.get(&node) {
                if cost > d {
                    continue;
                }
            }

            // Check depth limit to prevent infinite loops
            if depth >= max_depth {
                continue;
            }

            // Collect neighbors (outgoing, or both directions if bidirectional)
            neighbor_edges.clear();

            // Get outgoing edges at the specified time
            for edge_id in self.db.get_outgoing_edges_at_time(node, time, time) {
                neighbor_edges.push((edge_id, true)); // true = outgoing
            }

            // Also get incoming edges if bidirectional
            if bidirectional {
                for edge_id in self.db.get_incoming_edges_at_time(node, time, time) {
                    neighbor_edges.push((edge_id, false)); // false = incoming
                }
            }

            for &(edge_id, is_outgoing) in &neighbor_edges {
                // Get edge details at the specified time
                if let Ok(edge) = self.db.get_edge_at_time(edge_id, time, time) {
                    // For outgoing edges, target is the neighbor; for incoming, source is the neighbor
                    let target = if is_outgoing {
                        edge.target
                    } else {
                        edge.source
                    };

                    // Check if target node existed at time T (and get embedding)
                    if let Ok(target_node) = self.db.get_node_at_time(target, time, time) {
                        // Calculate semantic cost using historical embedding
                        let target_embedding = target_node
                            .properties
                            .get(&self.vector_property)
                            .and_then(|v| v.as_vector());

                        let semantic_cost =
                            self.compute_semantic_cost(target_embedding, query_embedding)?;

                        let new_cost = cost + semantic_cost + STRUCTURAL_HOP_COST;

                        if new_cost < *dist.get(&target).unwrap_or(&f32::INFINITY) {
                            dist.insert(target, new_cost);
                            came_from.insert(target, node);
                            pq.push(State {
                                cost: new_cost,
                                node: target,
                                depth: depth + 1,
                            });
                        }
                    }
                }
            }
        }

        Ok(None)
    }

    /// Calculate cost based on semantic similarity (1.0 - similarity).
    /// Returns 1.0 (max cost) if node has no embedding.
    fn calculate_semantic_cost(&self, node_id: NodeId, query: &[f32]) -> Result<f32> {
        let node = self.db.get_node(node_id)?;
        let embedding = node
            .properties
            .get(&self.vector_property)
            .and_then(|v| v.as_vector());

        self.compute_semantic_cost(embedding, query)
    }

    /// Helper to compute semantic cost from an embedding slice.
    ///
    /// Handles dimension mismatches gracefully by returning infinite cost.
    fn compute_semantic_cost(&self, embedding: Option<&[f32]>, query: &[f32]) -> Result<f32> {
        if let Some(emb) = embedding {
            match cosine_similarity(emb, query) {
                Ok(sim) => {
                    // Clamp to [0, 2] (cosine sim is [-1, 1])
                    // We want high similarity -> low cost
                    // 1.0 - 1.0 = 0.0 (perfect match)
                    // 1.0 - (-1.0) = 2.0 (opposite)
                    Ok(1.0 - sim)
                }
                Err(crate::core::error::Error::Vector(
                    crate::core::error::VectorError::DimensionMismatch { .. },
                )) => {
                    // Sentry 🛡️: Dimension mismatch implies incompatibility.
                    // Return infinite cost to strictly avoid this node unless no other path exists.
                    // This prevents the entire search from failing due to one malformed node.
                    Ok(f32::INFINITY)
                }
                Err(e) => Err(e),
            }
        } else {
            // Penalize nodes without embeddings
            Ok(NO_EMBEDDING_COST)
        }
    }

    fn reconstruct_path(
        &self,
        came_from: HashMap<NodeId, NodeId, BuildHasherDefault<IdentityHasher>>,
        current: NodeId,
    ) -> Vec<NodeId> {
        let mut path = vec![current];
        let mut curr = current;
        while let Some(&prev) = came_from.get(&curr) {
            path.push(prev);
            curr = prev;
        }
        path.reverse();
        path
    }
}
