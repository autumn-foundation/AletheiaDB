//! Odyssey: Semantic Gradient Ascent Traversal.
//!
//! "Follow the North Star."
//!
//! While `SemanticNavigator` finds the shortest path, and `Voyager` explores chaos,
//! `Odyssey` enforces a strict semantic gradient: every step along the path must
//! bring you closer to a guiding "target vector".
//!
//! It is useful for tracing logical progressions, skill trees, or evolutionary
//! steps where each node represents a measurable progression toward a goal concept.

use crate::AletheiaDB;
use crate::core::error::{Error, Result, VectorError};
use crate::core::id::NodeId;
use crate::core::vector::ops::cosine_similarity;
use std::collections::HashSet;

/// The Odyssey Engine for semantic gradient ascent.
pub struct Odyssey<'a> {
    db: &'a AletheiaDB,
}

impl<'a> Odyssey<'a> {
    /// Create a new Odyssey instance.
    pub fn new(db: &'a AletheiaDB) -> Self {
        Self { db }
    }

    /// Traverse the graph starting from `start_node`, moving only to neighbors
    /// that are *strictly more similar* to the `guide_vector` than the current node.
    ///
    /// It stops when a local maximum is reached (no neighbors are closer to the guide),
    /// or `max_steps` is hit.
    ///
    /// # Arguments
    /// * `start_node` - The node to start exploring from.
    /// * `guide_vector` - The "North Star" vector to optimize toward.
    /// * `vector_property` - The name of the vector property to use for distance.
    /// * `max_steps` - The maximum length of the path to return.
    ///
    /// # Returns
    /// A sequential list of `NodeId`s representing the path taken.
    pub fn embark(
        &self,
        start_node: NodeId,
        guide_vector: &[f32],
        vector_property: &str,
        max_steps: usize,
    ) -> Result<Vec<NodeId>> {
        let mut path = Vec::new();
        let mut visited = HashSet::new();

        let mut current = start_node;
        path.push(current);
        visited.insert(current);

        for _ in 0..max_steps {
            let current_vec = self.get_vector(current, vector_property)?;
            let current_sim = cosine_similarity(&current_vec, guide_vector)?;

            let edges = self.db.get_outgoing_edges(current);
            if edges.is_empty() {
                break;
            }

            let mut best_next_node = None;
            let mut highest_sim = current_sim; // Must strictly improve!

            for edge_id in edges {
                if let Ok(edge) = self.db.get_edge(edge_id) {
                    let neighbor = edge.target;

                    if visited.contains(&neighbor) {
                        continue;
                    }

                    if let Ok(neighbor_vec) = self.get_vector(neighbor, vector_property) {
                        let sim = cosine_similarity(&neighbor_vec, guide_vector)?;
                        if sim > highest_sim {
                            highest_sim = sim;
                            best_next_node = Some(neighbor);
                        }
                    }
                }
            }

            match best_next_node {
                Some(next_node) => {
                    path.push(next_node);
                    visited.insert(next_node);
                    current = next_node;
                }
                None => {
                    // Reached local maximum, no neighbor is closer to the guide vector.
                    break;
                }
            }
        }

        Ok(path)
    }

    /// Helper to extract a vector from a node.
    fn get_vector(&self, node_id: NodeId, property: &str) -> Result<Vec<f32>> {
        let node = self.db.get_node(node_id)?;
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
    use crate::AletheiaDB;
    use crate::core::property::PropertyMapBuilder;
    use crate::index::vector::{DistanceMetric, HnswConfig};

    #[test]
    fn test_odyssey_gradient_ascent() {
        let db = AletheiaDB::new().unwrap();
        db.enable_vector_index("vec", HnswConfig::new(2, DistanceMetric::Cosine))
            .unwrap();

        let guide = vec![1.0, 1.0];

        // A: Angle 90 deg -> Sim 0.707
        let a = db
            .create_node(
                "Node",
                PropertyMapBuilder::new()
                    .insert_vector("vec", &[0.0, 1.0])
                    .build(),
            )
            .unwrap();

        // B: Angle ~63.4 deg -> Sim 0.94 (Closer to guide)
        let b = db
            .create_node(
                "Node",
                PropertyMapBuilder::new()
                    .insert_vector("vec", &[0.5, 1.0])
                    .build(),
            )
            .unwrap();

        // C: Angle 135 deg -> Sim 0.0 (Worse than A)
        let c = db
            .create_node(
                "Node",
                PropertyMapBuilder::new()
                    .insert_vector("vec", &[-1.0, 1.0])
                    .build(),
            )
            .unwrap();

        // D: Angle 45 deg -> Sim 1.0 (Perfect match to guide)
        let d = db
            .create_node(
                "Node",
                PropertyMapBuilder::new()
                    .insert_vector("vec", &[1.0, 1.0])
                    .build(),
            )
            .unwrap();

        db.create_edge(a, b, "LINK", PropertyMapBuilder::new().build())
            .unwrap();
        db.create_edge(a, c, "LINK", PropertyMapBuilder::new().build())
            .unwrap();
        db.create_edge(b, d, "LINK", PropertyMapBuilder::new().build())
            .unwrap();

        let odyssey = Odyssey::new(&db);
        let path = odyssey.embark(a, &guide, "vec", 10).unwrap();

        assert_eq!(path.len(), 3);
        assert_eq!(path[0], a);
        assert_eq!(path[1], b); // Chose B over C because B improves similarity
        assert_eq!(path[2], d); // Reached D
    }

    #[test]
    fn test_odyssey_stops_at_local_maximum() {
        let db = AletheiaDB::new().unwrap();
        db.enable_vector_index("vec", HnswConfig::new(2, DistanceMetric::Cosine))
            .unwrap();

        let guide = vec![1.0, 1.0];

        // A: Sim 0.707
        let a = db
            .create_node(
                "Node",
                PropertyMapBuilder::new()
                    .insert_vector("vec", &[0.0, 1.0])
                    .build(),
            )
            .unwrap();
        // B: Sim 0.94 (Closer)
        let b = db
            .create_node(
                "Node",
                PropertyMapBuilder::new()
                    .insert_vector("vec", &[0.5, 1.0])
                    .build(),
            )
            .unwrap();
        // C: Sim 0.89 (Worse than B!)
        let c = db
            .create_node(
                "Node",
                PropertyMapBuilder::new()
                    .insert_vector("vec", &[0.2, 1.0])
                    .build(),
            )
            .unwrap();

        db.create_edge(a, b, "LINK", PropertyMapBuilder::new().build())
            .unwrap();
        db.create_edge(b, c, "LINK", PropertyMapBuilder::new().build())
            .unwrap();

        let odyssey = Odyssey::new(&db);
        let path = odyssey.embark(a, &guide, "vec", 10).unwrap();

        // Path should stop at B because C does not improve similarity.
        assert_eq!(path.len(), 2);
        assert_eq!(path[0], a);
        assert_eq!(path[1], b);
    }
}
