#![allow(clippy::collapsible_if)]
//! Odyssey: Semantic Gradient Descent Pathfinding.
//!
//! "Follow the scent of meaning."
//!
//! Odyssey performs a greedy traversal of the graph (a random walk guided by semantics),
//! moving from node to neighbor by always picking the neighbor that minimizes the distance
//! to a `target_vector`. It stops when it reaches a "local optimum" (a node whose neighbors
//! are all further away from the target than the node itself).
//!
//! # Use Cases
//! - **Semantic Routing**: "Find the node that best matches this concept by navigating the graph structure."
//! - **Concept Convergence**: Identifying conceptual local maxima in a knowledge graph.
//!
//! # Example
//! ```rust,no_run
//! use aletheiadb::AletheiaDB;
//! use aletheiadb::experimental::reasoning::odyssey::Odyssey;
//! use aletheiadb::core::id::NodeId;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let db = AletheiaDB::new()?;
//! let odyssey = Odyssey::new(&db);
//!
//! # let start_node = NodeId::new(1).unwrap();
//! # let target_vector = vec![1.0, 0.0, 0.0];
//! let path = odyssey.navigate(start_node, &target_vector, "embedding", 10)?;
//!
//! for step in path {
//!     println!("Visited node: {}, Distance to target: {}", step.node_id, step.distance);
//! }
//! # Ok(())
//! # }
//! ```

use crate::AletheiaDB;
use crate::core::error::Result;
use crate::core::id::NodeId;
use crate::core::vector::ops;
use std::collections::HashSet;

/// A step in the semantic gradient descent.
#[derive(Debug, Clone, PartialEq)]
pub struct TraversalStep {
    /// The node visited.
    pub node_id: NodeId,
    /// The distance from this node's vector to the target vector.
    pub distance: f32,
}

/// The Odyssey Engine.
pub struct Odyssey<'a> {
    db: &'a AletheiaDB,
}

impl<'a> Odyssey<'a> {
    /// Create a new Odyssey engine.
    pub fn new(db: &'a AletheiaDB) -> Self {
        Self { db }
    }

    /// Perform a greedy traversal of the graph towards a target vector.
    ///
    /// # Arguments
    /// * `start_node` - The node to begin traversal from.
    /// * `target_vector` - The semantic concept we are navigating towards.
    /// * `vector_property` - The name of the vector property to use for distance calculations.
    /// * `max_steps` - The maximum number of hops to take before giving up.
    ///
    /// # Returns
    /// The path taken as a series of `TraversalStep`s, including the start node.
    pub fn navigate(
        &self,
        start_node: NodeId,
        target_vector: &[f32],
        vector_property: &str,
        max_steps: usize,
    ) -> Result<Vec<TraversalStep>> {
        let mut path = Vec::new();
        let mut current_node = start_node;
        let mut visited = HashSet::new();

        // Calculate initial distance
        let current_distance =
            self.calculate_distance(current_node, target_vector, vector_property)?;
        let mut current_best_dist = current_distance;

        path.push(TraversalStep {
            node_id: current_node,
            distance: current_best_dist,
        });
        visited.insert(current_node);

        for _ in 0..max_steps {
            // Get all neighbors (both outgoing and incoming for a fuller structural context)
            let mut neighbors = HashSet::new();

            for edge_id in self.db.get_outgoing_edges(current_node) {
                if let Ok(target) = self.db.get_edge_target(edge_id) {
                    neighbors.insert(target);
                }
            }

            for edge_id in self.db.get_incoming_edges(current_node) {
                if let Ok(source) = self.db.get_edge_source(edge_id) {
                    neighbors.insert(source);
                }
            }

            // Find the neighbor closest to the target vector
            let mut best_neighbor = None;
            let mut best_neighbor_dist = f32::INFINITY;

            for neighbor in neighbors {
                if visited.contains(&neighbor) {
                    continue;
                }

                if let Ok(dist) = self.calculate_distance(neighbor, target_vector, vector_property)
                {
                    if dist < best_neighbor_dist {
                        best_neighbor_dist = dist;
                        best_neighbor = Some(neighbor);
                    }
                }
            }

            // If we found a neighbor that gets us closer to the target, move to it.
            // Local Optimum: If no unvisited neighbor is strictly closer than our current position, stop.
            if let Some(next_node) = best_neighbor {
                if best_neighbor_dist < current_best_dist {
                    current_node = next_node;
                    current_best_dist = best_neighbor_dist;
                    visited.insert(current_node);
                    path.push(TraversalStep {
                        node_id: current_node,
                        distance: current_best_dist,
                    });
                    continue;
                }
            }

            // Reached local optimum (or no unvisited neighbors)
            break;
        }

        Ok(path)
    }

    /// Calculate the distance (1 - Cosine Similarity) between a node's vector and the target vector.
    fn calculate_distance(
        &self,
        node_id: NodeId,
        target_vector: &[f32],
        property_name: &str,
    ) -> Result<f32> {
        let node = self.db.get_node(node_id)?;

        let node_vec = node
            .properties
            .get(property_name)
            .and_then(|p| p.as_vector())
            .ok_or_else(|| {
                crate::core::error::Error::other(format!(
                    "Vector property {} not found on node {}",
                    property_name, node_id
                ))
            })?;

        // Assuming normalized vectors, cosine distance is 1 - dot product.
        // `ops::cosine_similarity` handles this if not normalized, but let's normalize to be safe.
        let mut v1 = node_vec.to_vec();
        let mut v2 = target_vector.to_vec();

        ops::normalize_in_place(&mut v1);
        ops::normalize_in_place(&mut v2);

        let sim = ops::cosine_similarity(&v1, &v2)?;
        Ok(1.0 - sim)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::property::PropertyMapBuilder;
    use crate::index::vector::{DistanceMetric, HnswConfig};

    #[test]
    fn test_odyssey_navigation() {
        let db = AletheiaDB::new().unwrap();
        // Enable vector index just so we can set vectors
        db.enable_vector_index("vec", HnswConfig::new(2, DistanceMetric::Cosine))
            .unwrap();

        // Target concept: [1.0, 0.0]
        let target = vec![1.0, 0.0];

        // Create Nodes forming a path towards the target
        // A (far) -> B (closer) -> C (closest) -> D (farther, a trap)

        // Node A: [-1.0, 0.0] (Very far)
        let props_a = PropertyMapBuilder::new()
            .insert_vector("vec", &[-1.0, 0.0])
            .build();
        let a = db.create_node("Node", props_a).unwrap();

        // Node B: [0.0, 1.0] (Orthogonal, dist=1)
        let props_b = PropertyMapBuilder::new()
            .insert_vector("vec", &[0.0, 1.0])
            .build();
        let b = db.create_node("Node", props_b).unwrap();

        // Node C: [0.9, 0.1] (Very close)
        let props_c = PropertyMapBuilder::new()
            .insert_vector("vec", &[0.9, 0.1])
            .build();
        let c = db.create_node("Node", props_c).unwrap();

        // Node D: [0.5, 0.5] (Farther away than C)
        let props_d = PropertyMapBuilder::new()
            .insert_vector("vec", &[0.5, 0.5])
            .build();
        let d = db.create_node("Node", props_d).unwrap();

        // Create Graph Structure
        db.create_edge(a, b, "LINK", Default::default()).unwrap();
        db.create_edge(b, c, "LINK", Default::default()).unwrap();
        db.create_edge(c, d, "LINK", Default::default()).unwrap();

        let odyssey = Odyssey::new(&db);

        // Start from A, navigate to target
        let path = odyssey.navigate(a, &target, "vec", 10).unwrap();

        // Expected Path: A -> B -> C.
        // It should stop at C because D is farther away than C, meaning C is a local optimum.
        assert_eq!(path.len(), 3);
        assert_eq!(path[0].node_id, a);
        assert_eq!(path[1].node_id, b);
        assert_eq!(path[2].node_id, c);

        // Distances should be strictly decreasing
        assert!(path[0].distance > path[1].distance);
        assert!(path[1].distance > path[2].distance);
    }
}
