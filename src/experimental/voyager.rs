//! Voyager: Maximal Novelty Traversal Engine.
//!
//! "To boldly go where the semantics haven't."
//!
//! Most pathfinding and recommendation systems in AletheiaDB (and graph DBs in general)
//! rely on *similarity* — finding nodes that are close in vector space. This creates
//! filter bubbles and predictable trajectories.
//!
//! Voyager takes the opposite approach. It explores the graph by intentionally
//! choosing the connected node that is **most semantically different** from the current one.
//! It's a "semantic chaos" explorer designed to break out of local minimums and
//! find the most surprising connections.
//!
//! # Use Cases
//! - **Creative Brainstorming**: Jump between radically different concepts.
//! - **Story Generation**: Introduce unexpected plot twists by forcing characters into unfamiliar domains.
//! - **Anomaly Discovery**: Find paths that abruptly shift context.

use crate::core::error::{Error, Result, StorageError, VectorError};
use crate::core::id::NodeId;
use crate::core::vector::ops::cosine_similarity;
use crate::AletheiaDB;
use std::collections::HashSet;

/// The Voyager Engine for finding the path of maximal novelty.
pub struct Voyager<'a> {
    db: &'a AletheiaDB,
}

impl<'a> Voyager<'a> {
    /// Create a new Voyager instance.
    pub fn new(db: &'a AletheiaDB) -> Self {
        Self { db }
    }

    /// Traverse the graph starting from a seed node, always choosing the neighbor
    /// with the *lowest* cosine similarity to the current node.
    ///
    /// The traversal stops if:
    /// - `max_steps` is reached.
    /// - The current node has no outgoing edges.
    /// - All neighbors have already been visited (prevents infinite loops).
    /// - A node lacks the specified vector property.
    ///
    /// # Arguments
    /// * `start_node` - The node to start exploring from.
    /// * `vector_property` - The name of the vector property to use for distance.
    /// * `max_steps` - The maximum length of the path to return.
    ///
    /// # Returns
    /// A sequential list of `NodeId`s representing the path taken, starting with `start_node`.
    pub fn traverse(
        &self,
        start_node: NodeId,
        vector_property: &str,
        max_steps: usize,
    ) -> Result<Vec<NodeId>> {
        let mut path = Vec::new();
        let mut visited = HashSet::new();

        let mut current = start_node;
        path.push(current);
        visited.insert(current);

        for _ in 0..max_steps {
            // Get current node's vector
            let current_vec = self.get_vector(current, vector_property)?;

            let edges = self.db.get_outgoing_edges(current);
            if edges.is_empty() {
                break; // Dead end
            }

            let mut least_similar_neighbor = None;
            let mut lowest_similarity = f32::MAX;

            for edge_id in edges {
                if let Ok(edge) = self.db.get_edge(edge_id) {
                    let neighbor = edge.target;

                    // Skip visited to prevent cycles
                    if visited.contains(&neighbor) {
                        continue;
                    }

                    // Try to get neighbor's vector. If it doesn't have one, we can either
                    // ignore it or treat it as highly novel. Let's ignore it to keep it semantic.
                    if let Ok(neighbor_vec) = self.get_vector(neighbor, vector_property) {
                        let sim = cosine_similarity(&current_vec, &neighbor_vec)?;

                        if sim < lowest_similarity {
                            lowest_similarity = sim;
                            least_similar_neighbor = Some(neighbor);
                        }
                    }
                }
            }

            match least_similar_neighbor {
                Some(next_node) => {
                    path.push(next_node);
                    visited.insert(next_node);
                    current = next_node;
                }
                None => {
                    // All neighbors visited or lack vectors
                    break;
                }
            }
        }

        Ok(path)
    }

    /// Helper to extract a vector from a node.
    fn get_vector(&self, node_id: NodeId, property: &str) -> Result<Vec<f32>> {
        let node = self.db.get_node(node_id).map_err(|_| {
            Error::Storage(StorageError::NodeNotFound(node_id))
        })?;

        let val = node.properties.get(property).ok_or_else(|| {
            Error::Vector(VectorError::IndexError(format!(
                "Property '{}' missing on node {}",
                property, node_id
            )))
        })?;

        let vec = val.as_vector().ok_or_else(|| {
            Error::Vector(VectorError::IndexError(format!(
                "Property '{}' is not a vector on node {}",
                property, node_id
            )))
        })?;

        Ok(vec.to_vec())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::property::PropertyMapBuilder;
    use crate::index::vector::{DistanceMetric, HnswConfig};
    use crate::AletheiaDB;

    #[test]
    fn test_voyager_chooses_lowest_similarity() {
        let db = AletheiaDB::new().unwrap();
        db.enable_vector_index("vec", HnswConfig::new(2, DistanceMetric::Cosine)).unwrap();

        // Node A: [1.0, 0.0] (Start)
        let props_a = PropertyMapBuilder::new().insert_vector("vec", &[1.0, 0.0]).build();
        let a = db.create_node("Node", props_a).unwrap();

        // Node B: [0.9, 0.1] (High similarity to A, ~0.99)
        let props_b = PropertyMapBuilder::new().insert_vector("vec", &[0.9, 0.1]).build();
        let b = db.create_node("Node", props_b).unwrap();

        // Node C: [0.0, 1.0] (Low similarity to A, 0.0)
        let props_c = PropertyMapBuilder::new().insert_vector("vec", &[0.0, 1.0]).build();
        let c = db.create_node("Node", props_c).unwrap();

        // Node D: [-1.0, 0.0] (Lowest similarity to C, -1.0 relative to C? No, C is [0,1], D is [-1,0]. Sim is 0.0)
        // Wait, C is [0.0, 1.0]. D is [-1.0, 0.0]. Cosine sim(C, D) = 0.0.
        // Let's make D perfectly opposite to C: [0.0, -1.0]. Sim = -1.0.
        let props_d = PropertyMapBuilder::new().insert_vector("vec", &[0.0, -1.0]).build();
        let d = db.create_node("Node", props_d).unwrap();

        // Edges from A -> B and A -> C
        db.create_edge(a, b, "LINK", PropertyMapBuilder::new().build()).unwrap();
        db.create_edge(a, c, "LINK", PropertyMapBuilder::new().build()).unwrap();

        // Edges from C -> B and C -> D
        db.create_edge(c, b, "LINK", PropertyMapBuilder::new().build()).unwrap();
        db.create_edge(c, d, "LINK", PropertyMapBuilder::new().build()).unwrap();

        let voyager = Voyager::new(&db);

        // Start at A. Max steps = 2.
        // Step 1: A has neighbors B (sim ~0.99) and C (sim 0.0). Voyager chooses C.
        // Step 2: C has neighbors B (sim ~0.1) and D (sim -1.0). Voyager chooses D.
        let path = voyager.traverse(a, "vec", 2).unwrap();

        assert_eq!(path.len(), 3); // Start + 2 steps
        assert_eq!(path[0], a);
        assert_eq!(path[1], c);
        assert_eq!(path[2], d);
    }

    #[test]
    fn test_voyager_avoids_cycles() {
        let db = AletheiaDB::new().unwrap();
        db.enable_vector_index("vec", HnswConfig::new(2, DistanceMetric::Cosine)).unwrap();

        // Node A: [1.0, 0.0]
        let props_a = PropertyMapBuilder::new().insert_vector("vec", &[1.0, 0.0]).build();
        let a = db.create_node("Node", props_a).unwrap();

        // Node B: [0.0, 1.0] (Opposite/orthogonal to A)
        let props_b = PropertyMapBuilder::new().insert_vector("vec", &[0.0, 1.0]).build();
        let b = db.create_node("Node", props_b).unwrap();

        // Connect A <-> B to form a cycle
        db.create_edge(a, b, "LINK", PropertyMapBuilder::new().build()).unwrap();
        db.create_edge(b, a, "LINK", PropertyMapBuilder::new().build()).unwrap();

        let voyager = Voyager::new(&db);

        // Even with max_steps = 10, it should stop after A -> B because A is already visited.
        let path = voyager.traverse(a, "vec", 10).unwrap();

        assert_eq!(path.len(), 2);
        assert_eq!(path[0], a);
        assert_eq!(path[1], b);
    }
}
