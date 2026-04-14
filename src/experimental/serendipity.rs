//! Serendipity: The Scenic Route Finder 🧭
//!
//! "Sometimes the best path is not the shortest, but the most surprising."
//!
//! Serendipity finds paths between two nodes that intentionally maximize
//! semantic divergence along the way, while still eventually reaching the destination.
//! It answers the question: "How can I get from A to B while encountering the most
//! unexpected (but structurally valid) concepts?"
//!
//! # Concepts
//! - **Scenic Route**: A path where adjacent nodes have *low* cosine similarity,
//!   representing a "leap" in meaning, rather than a gradual shift.
//! - **Serendipity Score**: The sum of semantic distances (1.0 - similarity) along the path.
//!   Higher score = more surprising journey.
//!
//! # Use Cases
//! - **Creative Ideation**: "Connect 'Black Hole' to 'Pancake' using the most surprising conceptual jumps."
//! - **Recommendation Diversity**: Breaking out of "filter bubbles" by suggesting a path
//!   that traverses diverse topics before arriving at a related interest.
//! - **Educational Exploration**: Teaching by finding lateral connections between subjects.
//!
//! # Example
//!
//! ```rust
//! // Requires features = ["nova"]
//! use aletheiadb::AletheiaDB;
//! use aletheiadb::experimental::serendipity::SerendipityEngine;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let db = AletheiaDB::new()?;
//! // ... setup graph ...
//! # let start = aletheiadb::core::id::NodeId::new(0)?;
//! # let end = aletheiadb::core::id::NodeId::new(0)?;
//!
//! let serendipity = SerendipityEngine::new(&db);
//!
//! // Find a path from start to end, max 5 hops, maximizing semantic distance
//! // Note: This is an expensive operation on large graphs, use carefully!
//! # /*
//! let path = serendipity.find_scenic_route(start, end, "embedding", 5)?;
//!
//! println!("Found a scenic route with {} hops!", path.len());
//! # */
//! # Ok(())
//! # }
//! ```

use crate::AletheiaDB;
use crate::core::error::{Error, Result};
use crate::core::id::NodeId;
use crate::core::vector::ops;
use std::collections::{HashSet, VecDeque};

/// A discovered scenic route.
#[derive(Debug, Clone)]
pub struct ScenicRoute {
    /// The sequence of nodes forming the path.
    pub path: Vec<NodeId>,
    /// The total serendipity score (sum of semantic distances).
    pub serendipity_score: f32,
}

/// The Serendipity Engine.
pub struct SerendipityEngine<'a> {
    db: &'a AletheiaDB,
}

impl<'a> SerendipityEngine<'a> {
    /// Create a new SerendipityEngine.
    pub fn new(db: &'a AletheiaDB) -> Self {
        Self { db }
    }

    /// Find a path from `start` to `end` that maximizes semantic divergence (serendipity).
    ///
    /// This uses a modified breadth-first search / beam search that tracks all simple paths
    /// up to `max_depth`. It evaluates the serendipity score of each valid path and returns
    /// the one with the highest score.
    ///
    /// # Arguments
    /// * `start` - The starting node.
    /// * `end` - The target node.
    /// * `vector_property` - The name of the vector property to use for semantic comparisons.
    /// * `max_depth` - The maximum number of hops allowed. High values will be very slow.
    pub fn find_scenic_route(
        &self,
        start: NodeId,
        end: NodeId,
        vector_property: &str,
        max_depth: usize,
    ) -> Result<Option<ScenicRoute>> {
        if start == end {
            return Ok(Some(ScenicRoute {
                path: vec![start],
                serendipity_score: 0.0,
            }));
        }

        if max_depth == 0 {
            return Ok(None);
        }

        // To calculate serendipity, we need to know the semantic distance between adjacent nodes.
        // We'll perform a BFS that maintains the path and the accumulated score.
        // Since we want to find the *most* serendipitous path, we explore all paths up to max_depth
        // that reach the destination. To prevent combinatorial explosion, this should technically
        // be a Beam Search or bounded DFS, but for Nova we'll do a simple exhaustive BFS with a depth limit
        // and a visited set per path (simple paths only).

        // Queue stores: (current_node, current_path, current_score, current_depth)
        let mut queue = VecDeque::new();
        queue.push_back((start, vec![start], 0.0_f32, 0));

        let mut best_route: Option<ScenicRoute> = None;

        while let Some((current, path, score, depth)) = queue.pop_front() {
            if current == end {
                // We found a route! Check if it's the best so far.
                // We prefer routes with the highest serendipity score.
                // Note: Longer paths naturally get higher scores if we just sum them.
                // We could average it, but taking the "longest weirdest path" is perfectly serendipitous.
                if let Some(best) = &best_route {
                    if score > best.serendipity_score {
                        best_route = Some(ScenicRoute {
                            path: path.clone(),
                            serendipity_score: score,
                        });
                    }
                } else {
                    best_route = Some(ScenicRoute {
                        path: path.clone(),
                        serendipity_score: score,
                    });
                }
                continue;
            }

            if depth >= max_depth {
                continue; // Reached depth limit without hitting end
            }

            // Get current node's vector for distance calculation
            let current_vec = match self.get_vector(current, vector_property) {
                Ok(v) => v,
                Err(_) => continue, // Skip nodes without the vector property
            };

            // Explore neighbors (outgoing edges only for simplicity, though undirected might make sense)
            let outgoing = self.db.get_outgoing_edges(current);
            let mut neighbors = HashSet::new();

            for edge_id in outgoing {
                if let Ok(target) = self.db.get_edge_target(edge_id) {
                    neighbors.insert(target);
                }
            }

            for neighbor in neighbors {
                // Prevent cycles (ensure simple path)
                if path.contains(&neighbor) {
                    continue;
                }

                // Calculate semantic distance
                if let Ok(neighbor_vec) = self.get_vector(neighbor, vector_property) {
                    // Distance = 1.0 - Cosine Similarity
                    let similarity =
                        ops::cosine_similarity(&current_vec, &neighbor_vec).unwrap_or(0.0);
                    let distance = (1.0 - similarity).max(0.0);

                    let mut new_path = path.clone();
                    new_path.push(neighbor);

                    queue.push_back((neighbor, new_path, score + distance, depth + 1));
                }
            }
        }

        Ok(best_route)
    }

    /// Helper to fetch a node's vector safely.
    fn get_vector(&self, node_id: NodeId, property: &str) -> Result<Vec<f32>> {
        let node = self.db.get_node(node_id)?;
        let val = node.properties.get(property).ok_or_else(|| {
            Error::other(format!(
                "Property '{}' not found on node {}",
                property, node_id
            ))
        })?;
        let vec = val
            .as_vector()
            .ok_or_else(|| Error::other(format!("Property '{}' is not a vector", property)))?;
        Ok(vec.to_vec())
    }
}

#[cfg(not(feature = "nova"))]
impl<'a> SerendipityEngine<'a> {
    /// Create a new SerendipityEngine instance.
    pub fn new(_db: &'a AletheiaDB) -> Self {
        panic!(
            "Experimental features like Serendipity require the 'nova' feature. Please enable it in your Cargo.toml."
        );
    }

    /// Find a path from `start` to `end` that maximizes semantic divergence (serendipity).
    pub fn find_scenic_route(
        &self,
        _start: NodeId,
        _end: NodeId,
        _vector_property: &str,
        _max_depth: usize,
    ) -> Result<Option<ScenicRoute>> {
        panic!("Experimental features like Serendipity require the 'nova' feature.");
    }
}

#[cfg(all(test, feature = "nova"))]
mod tests {
    use super::*;
    use crate::core::property::PropertyMapBuilder;

    #[test]
    fn test_serendipity_route() {
        let db = AletheiaDB::new().unwrap();

        // Let's build a graph where A -> B -> C is the shortest path (and semantically smooth)
        // But A -> X -> Y -> C is a longer, highly divergent path.

        // Vectors:
        // A: [1.0, 0.0]
        // B: [0.9, 0.1] (Similar to A)
        // C: [0.8, 0.2] (Similar to B)
        // X: [0.0, 1.0] (Orthogonal to A -> High serendipity!)
        // Y: [-1.0, 0.0] (Opposite to X -> High serendipity!)

        let a = db
            .create_node(
                "Node",
                PropertyMapBuilder::new()
                    .insert_vector("vec", &[1.0, 0.0])
                    .build(),
            )
            .unwrap();
        let b = db
            .create_node(
                "Node",
                PropertyMapBuilder::new()
                    .insert_vector("vec", &[0.9, 0.1])
                    .build(),
            )
            .unwrap();
        let c = db
            .create_node(
                "Node",
                PropertyMapBuilder::new()
                    .insert_vector("vec", &[0.8, 0.2])
                    .build(),
            )
            .unwrap();

        let x = db
            .create_node(
                "Node",
                PropertyMapBuilder::new()
                    .insert_vector("vec", &[0.0, 1.0])
                    .build(),
            )
            .unwrap();
        let y = db
            .create_node(
                "Node",
                PropertyMapBuilder::new()
                    .insert_vector("vec", &[-1.0, 0.0])
                    .build(),
            )
            .unwrap();

        // Smooth Path: A -> B -> C
        db.create_edge(a, b, "LINK", Default::default()).unwrap();
        db.create_edge(b, c, "LINK", Default::default()).unwrap();

        // Scenic Path: A -> X -> Y -> C
        db.create_edge(a, x, "LINK", Default::default()).unwrap();
        db.create_edge(x, y, "LINK", Default::default()).unwrap();
        db.create_edge(y, c, "LINK", Default::default()).unwrap();

        let engine = SerendipityEngine::new(&db);

        // Let's find the route!
        let result = engine.find_scenic_route(a, c, "vec", 4).unwrap().unwrap();

        // The scenic path A->X->Y->C should have a much higher score than A->B->C
        assert_eq!(result.path, vec![a, x, y, c]);

        // A to X: Cosine([1,0], [0,1]) = 0.0 -> Distance = 1.0
        // X to Y: Cosine([0,1], [-1,0]) = 0.0 -> Distance = 1.0
        // Y to C: Cosine([-1,0], [0.8, 0.2]) = -0.8 -> Distance = 1.8
        // Total expected score: ~3.8
        assert!(result.serendipity_score > 3.0);
    }

    #[test]
    fn test_serendipity_no_path() {
        let db = AletheiaDB::new().unwrap();
        let a = db
            .create_node(
                "Node",
                PropertyMapBuilder::new()
                    .insert_vector("vec", &[1.0])
                    .build(),
            )
            .unwrap();
        let b = db
            .create_node(
                "Node",
                PropertyMapBuilder::new()
                    .insert_vector("vec", &[1.0])
                    .build(),
            )
            .unwrap();

        let engine = SerendipityEngine::new(&db);
        let result = engine.find_scenic_route(a, b, "vec", 3).unwrap();

        assert!(result.is_none());
    }
}

#[cfg(all(test, not(feature = "nova")))]
mod stub_tests {
    use super::*;

    #[test]
    #[should_panic(expected = "require the 'nova' feature")]
    fn test_stub_new() {
        let db = AletheiaDB::new().unwrap();
        let _ = SerendipityEngine::new(&db);
    }
}
