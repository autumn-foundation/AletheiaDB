//! Semantic Pathfinding Module (Experimental)
//!
//! This module implements pathfinding algorithms that consider semantic similarity
//! (vector embeddings) as part of the cost function.
//!
//! # Features
//! - **Semantic A***: Finds paths where nodes are semantically similar to a query concept.
//! - **Time-Travel Pathfinding**: Finds paths that were valid at a specific point in time.

use crate::core::id::NodeId;
use crate::core::temporal::Timestamp;
use crate::core::vector::cosine_similarity;
use crate::db::GallifreyDB;
use crate::utils::error::Result;
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

/// A pathfinder that uses semantic similarity as a heuristic/cost.
pub struct SemanticPathfinder<'a> {
    db: &'a GallifreyDB,
    vector_property: String,
}

impl<'a> SemanticPathfinder<'a> {
    /// Create a new SemanticPathfinder.
    ///
    /// # Arguments
    /// * `db` - Reference to the database instance.
    /// * `vector_property` - Name of the property containing vector embeddings.
    pub fn new(db: &'a GallifreyDB, vector_property: &str) -> Self {
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
    ///
    /// # Returns
    /// A vector of NodeIds representing the path, or None if no path found.
    pub fn find_path(
        &self,
        start: NodeId,
        end: NodeId,
        query_embedding: &[f32],
        max_depth: usize,
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

            // Get outgoing edges
            let edges = self.db.get_outgoing_edges(node);

            for edge_id in edges {
                // For "current" pathfinding, we assume current edges are valid.
                let target = self.db.get_edge_target(edge_id)?;

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
    ///
    /// # Limitations
    /// Currently only considers edges that exist in the *current* storage but checks
    /// their validity at `time`. Does not find edges that were deleted.
    pub fn find_path_at_time(
        &self,
        start: NodeId,
        end: NodeId,
        query_embedding: &[f32],
        time: Timestamp,
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

            // Get POTENTIAL outgoing edges from current storage
            // Note: This misses deleted edges!
            let potential_edges = self.db.get_outgoing_edges(node);

            for edge_id in potential_edges {
                // Check if edge existed at time T
                if let Ok(edge) = self.db.get_edge_at_time(edge_id, time, time) {
                    let target = edge.target;

                    // Check if target node existed at time T (and get embedding)
                    if let Ok(target_node) = self.db.get_node_at_time(target, time, time) {
                        // Calculate semantic cost using historical embedding
                        let target_embedding = target_node
                            .properties
                            .get(&self.vector_property)
                            .and_then(|v| v.as_vector());

                        let semantic_cost = if let Some(emb) = target_embedding {
                            1.0 - cosine_similarity(emb, query_embedding)?
                        } else {
                            1.0 // High cost if no embedding
                        };

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
    #[allow(clippy::collapsible_if)]
    fn calculate_semantic_cost(&self, node_id: NodeId, query: &[f32]) -> Result<f32> {
        let node = self.db.get_node(node_id)?;

        if let Some(prop) = node.properties.get(&self.vector_property) {
            if let Some(vec) = prop.as_vector() {
                let sim = cosine_similarity(vec, query)?;
                // Clamp to [0, 2] (cosine sim is [-1, 1])
                // We want high similarity -> low cost
                // 1.0 - 1.0 = 0.0 (perfect match)
                // 1.0 - (-1.0) = 2.0 (opposite)
                return Ok(1.0 - sim);
            }
        }

        // Penalize nodes without embeddings
        Ok(1.0)
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
    use crate::core::property::PropertyMapBuilder;
    use crate::index::vector::{DistanceMetric, HnswConfig};

    fn create_test_db() -> GallifreyDB {
        let db = GallifreyDB::new().unwrap();
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
            .find_path(start, end, &query, 10)
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
        use std::thread;
        use std::time::Duration;

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
                Ok(())
            })
            .unwrap();

        let t0 = t_edges;

        let query = vec![1.0, 0.0, 0.0];
        let pathfinder = SemanticPathfinder::new(&db, "embedding");

        // Query at t0: Path should exist (BEFORE DELETION)
        let path_t0 = pathfinder
            .find_path_at_time(start, end, &query, t0)
            .unwrap();
        assert!(path_t0.is_some(), "Path should exist at t0 before deletion");

        // Wait a bit to ensure distinct timestamp
        thread::sleep(Duration::from_millis(10));

        // Delete "Middle" node at t1 (which should break the path)
        // Use delete_node_cascade to ensure edges are also deleted from current storage
        let (_, t_delete) = db
            .write_with_timestamp(|tx| tx.delete_node_cascade(middle))
            .unwrap();
        let _t1 = t_delete;

        // Query at t0 AFTER deletion: Path should NOT exist due to limitation
        // (Deleted edges are removed from current storage adjacency)
        let path_t0_after_delete = pathfinder
            .find_path_at_time(start, end, &query, t0)
            .unwrap();
        assert!(
            path_t0_after_delete.is_none(),
            "Should be None due to current storage limitation"
        );

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
                Ok(())
            })
            .unwrap();

        let t2 = t_new_edges;

        // Path exists at t2
        let path_t2 = pathfinder
            .find_path_at_time(start, end, &query, t2)
            .unwrap();
        assert!(path_t2.is_some(), "Path should exist at t2");

        // Path through NewMiddle did NOT exist at t0
        // (And we know path through Middle is effectively invisible due to deletion)
        // But specifically, NewMiddle edges shouldn't be valid at t0
        // find_path_at_time iterates current edges (which include NewMiddle edges)
        // but filters them by `is_visible_at(t0)`.
        // NewMiddle edges are valid from t2. t0 < t2. So they should NOT be visible.

        let path_t0_check = pathfinder
            .find_path_at_time(start, end, &query, t0)
            .unwrap();
        assert!(
            path_t0_check.is_none(),
            "New path should not be visible at t0"
        );
    }
}
