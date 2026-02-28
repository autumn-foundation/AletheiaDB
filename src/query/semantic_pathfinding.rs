//! Semantic Pathfinding Module (Experimental)
//!
//! This module implements pathfinding algorithms that consider semantic similarity
//! (vector embeddings) as part of the cost function.
//!
//! # The Story of Semantic Pathfinding
//!
//! Traditional graph pathfinding algorithms like Dijkstra's or A* are brilliant at finding the *shortest*
//! physical path between two nodes. But in a knowledge graph, the "shortest" path isn't always the
//! most *relevant* one.
//!
//! Imagine searching for a path between `Node(Alice)` and `Node(Machine Learning)`. A traditional algorithm
//! might route you through `Node(Bob)` simply because Alice knows Bob, and Bob read a book on Machine Learning.
//! But what if Alice has a deep connection to `Node(Data Science)`, which then links to `Node(Machine Learning)`?
//!
//! Semantic Pathfinding solves this by combining structural graph traversal with vector similarity.
//! By treating the semantic distance (e.g., cosine distance) between a node's embedding and a *target concept*
//! as an edge weight penalty, the algorithm naturally "steers" the traversal through nodes that are
//! conceptually related to what you are looking for. It's like asking the algorithm to "stay on topic"
//! while it searches the graph.
//!
//! # Features
//! - **Semantic A***: Finds paths where nodes are semantically similar to a query concept.
//! - **Time-Travel Pathfinding**: Finds paths that were valid at a specific point in time using the bi-temporal model.

use crate::core::error::Result;
use crate::core::id::NodeId;
use crate::core::temporal::Timestamp;
use crate::core::vector::cosine_similarity;
use crate::query::traits::GraphView;
use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashMap};

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

/// A pathfinder that uses semantic similarity as a traversal cost.
///
/// This engine wraps a standard `GraphView` and uses Dijkstra's algorithm augmented with vector distance
/// to bias paths toward a specific semantic concept.
///
/// ## Examples
///
/// ```rust,no_run
/// use aletheiadb::AletheiaDB;
/// use aletheiadb::query::semantic_pathfinding::SemanticPathfinder;
/// use aletheiadb::core::id::NodeId;
///
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// let db = AletheiaDB::new()?;
/// let start_node = NodeId::new(1).unwrap();
/// let target_node = NodeId::new(100).unwrap();
/// let query_concept = vec![0.1, 0.5, 0.9]; // e.g., an embedding for "AI"
///
/// let pathfinder = SemanticPathfinder::new(&db, "embedding");
///
/// // Find a path up to 5 hops deep that stays semantically close to "AI"
/// if let Some(path) = pathfinder.find_path(start_node, target_node, &query_concept, 5, false)? {
///     println!("Found semantic path: {:?}", path);
/// }
/// # Ok(())
/// # }
/// ```
pub struct SemanticPathfinder<'a, G: GraphView + ?Sized> {
    db: &'a G,
    vector_property: String,
}

impl<'a, G: GraphView + ?Sized> SemanticPathfinder<'a, G> {
    /// Creates a new `SemanticPathfinder`.
    ///
    /// The `vector_property` tells the pathfinder which property key to inspect when retrieving a node's
    /// embedding to compute its semantic cost against the query vector.
    ///
    /// # Arguments
    /// * `db` - Reference to the graph view (e.g., database instance).
    /// * `vector_property` - Name of the property containing vector embeddings.
    pub fn new(db: &'a G, vector_property: &str) -> Self {
        Self {
            db,
            vector_property: vector_property.to_string(),
        }
    }

    /// Find a path from start to end that minimizes semantic distance to the query vector.
    ///
    /// Uses Dijkstra's algorithm where edge weight = `1.0 - cosine_similarity(target_node, query)`.
    /// This favors paths through nodes that are semantically relevant to the query concept.
    ///
    /// # Arguments
    /// * `start` - Starting node ID.
    /// * `end` - Target node ID.
    /// * `query_embedding` - Vector representing the semantic concept to follow.
    /// * `max_depth` - Maximum path length (to prevent infinite loops in large graphs).
    /// * `bidirectional` - If true, traverses both outgoing and incoming edges.
    ///
    /// # Returns
    /// A vector of NodeIds representing the path, or None if no path found.
    pub fn find_path(
        &self,
        start: NodeId,
        end: NodeId,
        query_embedding: &[f32],
        max_depth: usize,
        bidirectional: bool,
    ) -> Result<Option<Vec<NodeId>>> {
        let mut pq = BinaryHeap::new();
        let mut dist = HashMap::new();
        let mut came_from = HashMap::new();

        // Initialize start node
        dist.insert(start, 0.0);
        pq.push(State {
            cost: 0.0,
            node: start,
            depth: 0,
        });

        while let Some(State { cost, node, depth }) = pq.pop() {
            if node == end {
                return Ok(Some(self.reconstruct_path(came_from, end)));
            }

            // Optimization: Skip if we found a better path already
            if dist.get(&node).is_some_and(|&d| cost > d) {
                continue;
            }

            // Check depth limit
            if depth >= max_depth {
                continue;
            }

            // Collect neighbors (outgoing, or both directions if bidirectional)
            let mut neighbors = Vec::new();

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
            for target in neighbors {
                // Calculate semantic cost of moving to target
                let semantic_cost = self.calculate_semantic_cost(target, query_embedding)?;

                // Total cost = current cost + semantic cost + structural cost (1.0 for hop)
                // We add 1.0 structural cost to prefer shorter paths if semantics are equal.
                // Adjustable weight could be added later.
                let new_cost = cost + semantic_cost + 0.1; // Small structural cost

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

    /// Find a path at a specific point in time.
    ///
    /// Similar to `find_path`, but only considers nodes and edges valid at `time`.
    /// Uses the temporal adjacency index to find edges that existed at the specified
    /// time, even if they have been deleted from current storage.
    ///
    /// # Arguments
    /// * `start` - Starting node ID.
    /// * `end` - Target node ID.
    /// * `query_embedding` - Vector representing the semantic concept to follow.
    /// * `time` - The timestamp at which to evaluate the graph.
    /// * `max_depth` - Maximum path length (to prevent infinite loops in large graphs).
    /// * `bidirectional` - If true, traverses both outgoing and incoming edges.
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
        let mut dist = HashMap::new();
        let mut came_from = HashMap::new();

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

        while let Some(State { cost, node, depth }) = pq.pop() {
            if node == end {
                return Ok(Some(self.reconstruct_path(came_from, end)));
            }

            if dist.get(&node).is_some_and(|&d| cost > d) {
                continue;
            }

            // Check depth limit to prevent infinite loops
            if depth >= max_depth {
                continue;
            }

            // Collect neighbors (outgoing, or both directions if bidirectional)
            let mut neighbor_edges = Vec::new();

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

            for (edge_id, is_outgoing) in neighbor_edges {
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

                        let new_cost = cost + semantic_cost + 0.1;

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
            Ok(1.0)
        }
    }

    fn reconstruct_path(&self, came_from: HashMap<NodeId, NodeId>, current: NodeId) -> Vec<NodeId> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::transaction::WriteOps;
    use crate::core::error::Error;
    use crate::core::property::PropertyMapBuilder;
    use crate::db::AletheiaDB;
    use crate::index::vector::{DistanceMetric, HnswConfig};

    fn create_test_db() -> AletheiaDB {
        let db = AletheiaDB::new().unwrap();
        // Enable vector index to ensure vector properties are handled correctly
        // (though SemanticPathfinder works with raw properties too)
        db.vector_index("embedding")
            .hnsw(HnswConfig::new(3, DistanceMetric::Cosine))
            .enable()
            .unwrap();
        db
    }

    #[test]
    fn test_semantic_pathfinding_prefers_similar_nodes() {
        let db = create_test_db();

        // Topic: "Fruits" (Query will be close to this)
        let fruit_vec = vec![1.0, 0.0, 0.0];

        // Topic: "Tech" (Dissimilar)
        let tech_vec = vec![0.0, 1.0, 0.0];

        // Start
        let start = db
            .create_node(
                "Start",
                PropertyMapBuilder::new()
                    .insert_vector("embedding", &[0.5, 0.5, 0.0])
                    .build(),
            )
            .unwrap();

        // End
        let end = db
            .create_node(
                "End",
                PropertyMapBuilder::new()
                    .insert_vector("embedding", &[0.5, 0.5, 0.0])
                    .build(),
            )
            .unwrap();

        // Path 1: "Apple" (Fruit) -> End
        let apple = db
            .create_node(
                "Apple",
                PropertyMapBuilder::new()
                    .insert_vector("embedding", &fruit_vec)
                    .build(),
            )
            .unwrap();

        // Path 2: "Laptop" (Tech) -> End
        let laptop = db
            .create_node(
                "Laptop",
                PropertyMapBuilder::new()
                    .insert_vector("embedding", &tech_vec)
                    .build(),
            )
            .unwrap();

        // Edges
        db.create_edge(start, apple, "NEXT", PropertyMapBuilder::new().build())
            .unwrap();
        db.create_edge(apple, end, "NEXT", PropertyMapBuilder::new().build())
            .unwrap();

        db.create_edge(start, laptop, "NEXT", PropertyMapBuilder::new().build())
            .unwrap();
        db.create_edge(laptop, end, "NEXT", PropertyMapBuilder::new().build())
            .unwrap();

        // Find path with query "Banana" (Fruit-like)
        let query = vec![0.9, 0.1, 0.0];

        let pathfinder = SemanticPathfinder::new(&db, "embedding");
        let path = pathfinder
            .find_path(start, end, &query, 10, false)
            .unwrap()
            .unwrap();

        // Should prefer Apple over Laptop
        assert_eq!(path.len(), 3);
        assert_eq!(path[0], start);
        assert_eq!(path[1], apple);
        assert_eq!(path[2], end);
    }

    #[test]
    fn test_semantic_pathfinding_time_travel() {
        use crate::core::temporal::time;

        let db = create_test_db();
        let _now = time::now();

        // Create nodes
        let start = db
            .create_node("Start", PropertyMapBuilder::new().build())
            .unwrap();
        let middle = db
            .create_node(
                "Middle",
                PropertyMapBuilder::new()
                    .insert_vector("embedding", &[1.0, 0.0, 0.0])
                    .build(),
            )
            .unwrap();
        let end = db
            .create_node("End", PropertyMapBuilder::new().build())
            .unwrap();

        // Create edges at t0
        // Use write_with_timestamp to ensure t0 covers the creation
        let (_, t_edges) = db
            .write_with_timestamp(|tx| {
                tx.create_edge(start, middle, "NEXT", PropertyMapBuilder::new().build())?;
                tx.create_edge(middle, end, "NEXT", PropertyMapBuilder::new().build())?;
                Ok::<_, Error>(())
            })
            .unwrap();

        let t0 = t_edges;

        let query = vec![1.0, 0.0, 0.0];
        let pathfinder = SemanticPathfinder::new(&db, "embedding");

        // Query at t0: Path should exist (BEFORE DELETION)
        let path_t0 = pathfinder
            .find_path_at_time(start, end, &query, t0, 10, false)
            .unwrap();
        assert!(path_t0.is_some(), "Path should exist at t0 before deletion");

        // Delete "Middle" node at t1 (which should break the path)
        // Use delete_node_cascade to ensure edges are also deleted from current storage
        let (_, t_delete) = db
            .write_with_timestamp(|tx| tx.delete_node_cascade(middle))
            .unwrap();
        let _t1 = t_delete;

        // Verify time monotonicity (HLC guarantees distinct timestamps)
        assert!(
            t_delete > t0,
            "Time must advance monotonically for subsequent transactions"
        );

        // Query at t0 AFTER deletion: With temporal adjacency index (enabled by default),
        // the path SHOULD be found even though edges are deleted from current storage
        let path_t0_after_delete = pathfinder
            .find_path_at_time(start, end, &query, t0, 10, false)
            .unwrap();
        assert!(
            path_t0_after_delete.is_some(),
            "Temporal adjacency index (enabled by default) should find path through deleted edges"
        );
        let path = path_t0_after_delete.unwrap();
        assert_eq!(path.len(), 3);
        assert_eq!(path[0], start);
        assert_eq!(path[1], middle);
        assert_eq!(path[2], end);

        // Test "Future Path" scenario: Path exists now but didn't in past

        let new_middle = db
            .create_node(
                "NewMiddle",
                PropertyMapBuilder::new()
                    .insert_vector("embedding", &[1.0, 0.0, 0.0])
                    .build(),
            )
            .unwrap();

        let (_, t_new_edges) = db
            .write_with_timestamp(|tx| {
                tx.create_edge(start, new_middle, "NEXT", PropertyMapBuilder::new().build())?;
                tx.create_edge(new_middle, end, "NEXT", PropertyMapBuilder::new().build())?;
                Ok::<_, Error>(())
            })
            .unwrap();

        let t2 = t_new_edges;

        // Path exists at t2
        let path_t2 = pathfinder
            .find_path_at_time(start, end, &query, t2, 10, false)
            .unwrap();
        assert!(path_t2.is_some(), "Path should exist at t2");

        // Query at t0 again: Should find the ORIGINAL path through middle
        // (not the new path through new_middle which was created at t2)
        // With temporal adjacency index, deleted edges are still accessible
        // when querying at times before they were deleted.
        let path_t0_check = pathfinder
            .find_path_at_time(start, end, &query, t0, 10, false)
            .unwrap();
        assert!(
            path_t0_check.is_some(),
            "Should find original path at t0 (through middle, not new_middle)"
        );
        // Verify it's the original middle node, not new_middle
        assert_eq!(path_t0_check.as_ref().unwrap()[1], middle);
    }

    mod sentry_tests {
        use super::*;

        #[test]
        fn test_pathfinding_zero_max_depth() {
            let db = create_test_db();
            // Create a minimal graph A -> B
            let a = db
                .create_node("A", PropertyMapBuilder::new().build())
                .unwrap();
            let b = db
                .create_node("B", PropertyMapBuilder::new().build())
                .unwrap();
            db.create_edge(a, b, "NEXT", PropertyMapBuilder::new().build())
                .unwrap();

            let query = vec![0.0; 3];
            let pathfinder = SemanticPathfinder::new(&db, "embedding");

            // Max depth 0 should fail to find path if A != B
            let path = pathfinder.find_path(a, b, &query, 0, false).unwrap();
            assert!(path.is_none(), "Depth 0 should not allow traversal");
        }

        #[test]
        fn test_pathfinding_start_equals_end() {
            let db = create_test_db();
            let a = db
                .create_node("A", PropertyMapBuilder::new().build())
                .unwrap();

            let query = vec![0.0; 3];
            let pathfinder = SemanticPathfinder::new(&db, "embedding");

            // Should find path [A] immediately
            let path = pathfinder.find_path(a, a, &query, 10, false).unwrap();
            assert!(path.is_some());
            assert_eq!(path.unwrap(), vec![a]);
        }

        #[test]
        fn test_pathfinding_disconnected() {
            let db = create_test_db();
            let a = db
                .create_node("A", PropertyMapBuilder::new().build())
                .unwrap();
            let b = db
                .create_node("B", PropertyMapBuilder::new().build())
                .unwrap();

            // No edges

            let query = vec![0.0; 3];
            let pathfinder = SemanticPathfinder::new(&db, "embedding");

            let path = pathfinder.find_path(a, b, &query, 10, false).unwrap();
            assert!(path.is_none());
        }

        #[test]
        fn test_pathfinding_cycle() {
            let db = create_test_db();
            let a = db
                .create_node("A", PropertyMapBuilder::new().build())
                .unwrap();
            let b = db
                .create_node("B", PropertyMapBuilder::new().build())
                .unwrap();

            // Cycle: A -> B -> A
            db.create_edge(a, b, "NEXT", PropertyMapBuilder::new().build())
                .unwrap();
            db.create_edge(b, a, "BACK", PropertyMapBuilder::new().build())
                .unwrap();

            let query = vec![0.0; 3];
            let pathfinder = SemanticPathfinder::new(&db, "embedding");

            // Search for unreachable C
            let c = db
                .create_node("C", PropertyMapBuilder::new().build())
                .unwrap();

            // Should terminate and return None, not hang
            let path = pathfinder.find_path(a, c, &query, 10, false).unwrap();
            assert!(path.is_none());
        }

        #[test]
        fn test_calculate_semantic_cost_dimension_mismatch() {
            let db = create_test_db();
            // Node with 3D vector
            let a = db
                .create_node(
                    "A",
                    PropertyMapBuilder::new()
                        .insert_vector("embedding", &[1.0, 0.0, 0.0])
                        .build(),
                )
                .unwrap();
            let b = db
                .create_node(
                    "B",
                    PropertyMapBuilder::new()
                        .insert_vector("embedding", &[0.0, 1.0, 0.0])
                        .build(),
                )
                .unwrap();

            db.create_edge(a, b, "NEXT", PropertyMapBuilder::new().build())
                .unwrap();

            // Query with 4D vector -> Mismatch!
            let query = vec![0.0; 4];
            let pathfinder = SemanticPathfinder::new(&db, "embedding");

            // Sentry 🛡️: Should handle dimension mismatch gracefully by treating the node as incompatible
            // (infinite cost), effectively blocking the path.
            // Since A->B is the only path, and B is incompatible, it should return Ok(None).
            let result = pathfinder.find_path(a, b, &query, 10, false);
            assert!(result.is_ok());
            assert!(
                result.unwrap().is_none(),
                "Path should be blocked due to dimension mismatch"
            );
        }
    }

    mod sentry_robustness_tests {
        use super::*;
        use crate::api::transaction::WriteOps;
        use crate::core::property::PropertyMapBuilder;
        use crate::db::AletheiaDB;
        use crate::index::vector::{DistanceMetric, HnswConfig}; // Import WriteOps to get create_node/update_node

        fn create_test_db() -> AletheiaDB {
            let db = AletheiaDB::new().unwrap();
            // Enable vector index
            db.vector_index("embedding")
                .hnsw(HnswConfig::new(3, DistanceMetric::Cosine))
                .enable()
                .unwrap();
            db
        }

        #[test]
        fn test_pathfinding_skips_incompatible_dimensions() {
            // 🛡️ Sentry Test: Mixed dimensions should not crash pathfinding.
            // Setup:
            // Start (3D) -> Broken (4D) -> End (3D)
            //            -> Valid (3D)  -> End (3D)
            //
            // Pathfinding should navigate around Broken and use Valid.

            let db = create_test_db();

            // Nodes
            let start = db
                .create_node(
                    "Start",
                    PropertyMapBuilder::new()
                        .insert_vector("embedding", &[1.0, 0.0, 0.0])
                        .build(),
                )
                .unwrap();

            let end = db
                .create_node(
                    "End",
                    PropertyMapBuilder::new()
                        .insert_vector("embedding", &[0.0, 0.0, 1.0])
                        .build(),
                )
                .unwrap();

            // Create nodes with different property name for "broken" 4D vector
            // to bypass potential index validation during creation.
            // We will tell pathfinder to use "embedding_mixed".

            let broken = db
                .create_node(
                    "Broken",
                    PropertyMapBuilder::new()
                        .insert_vector("embedding_mixed", &[0.5, 0.5, 0.5, 0.5])
                        .build(),
                )
                .unwrap();

            let valid = db
                .create_node(
                    "Valid",
                    PropertyMapBuilder::new()
                        .insert_vector("embedding_mixed", &[0.5, 0.5, 0.0])
                        .build(),
                )
                .unwrap();

            // Update Start and End to also use "embedding_mixed" using explicit transaction
            db.write(|tx| {
                tx.update_node(
                    start,
                    PropertyMapBuilder::new()
                        .insert_vector("embedding_mixed", &[1.0, 0.0, 0.0])
                        .build(),
                )
            })
            .unwrap();
            db.write(|tx| {
                tx.update_node(
                    end,
                    PropertyMapBuilder::new()
                        .insert_vector("embedding_mixed", &[0.0, 0.0, 1.0])
                        .build(),
                )
            })
            .unwrap();

            // Connect
            db.create_edge(start, broken, "NEXT", PropertyMapBuilder::new().build())
                .unwrap();
            db.create_edge(broken, end, "NEXT", PropertyMapBuilder::new().build())
                .unwrap();

            db.create_edge(start, valid, "NEXT", PropertyMapBuilder::new().build())
                .unwrap();
            db.create_edge(valid, end, "NEXT", PropertyMapBuilder::new().build())
                .unwrap();

            // Pathfinding
            let query = vec![1.0, 0.0, 0.0];
            let pathfinder = SemanticPathfinder::new(&db, "embedding_mixed");

            let result = pathfinder.find_path(start, end, &query, 10, false);

            match result {
                Ok(Some(p)) => {
                    // If it succeeds, verify it took the valid path
                    assert_eq!(p, vec![start, valid, end], "Should take valid path");
                }
                Ok(None) => panic!("Should find a path (returned None)"),
                Err(e) => {
                    panic!(
                        "Regression: Dimension mismatch error was not suppressed. Expected successful pathfinding skipping invalid node. Error: {}",
                        e
                    );
                }
            }
        }
    }
}
