//! Lens: Semantic Perspective Engine.
//!
//! "Wear glasses made of a concept."
//!
//! The Lens engine allows you to query and traverse the graph from the perspective
//! of a specific concept vector. It filters nodes and edges based on their semantic
//! relevance to the focus vector, effectively creating a "semantic projection" of the graph.

use crate::AletheiaDB;
use crate::core::error::{Error, Result, VectorError};
use crate::core::id::NodeId;
use crate::core::vector::ops::{cosine_similarity, normalize};
use std::collections::{HashSet, VecDeque};

/// A semantic lens through which to view the graph.
pub struct Lens<'a> {
    db: &'a AletheiaDB,
    focus_vector: Vec<f32>,
    vector_property: String,
}

impl<'a> Lens<'a> {
    /// Create a new Lens with a focus vector.
    pub fn new(db: &'a AletheiaDB, focus_vector: Vec<f32>, vector_property: &str) -> Self {
        // Normalize the focus vector for consistent cosine similarity
        let normalized = normalize(&focus_vector);
        Self {
            db,
            focus_vector: normalized,
            vector_property: vector_property.to_string(),
        }
    }

    /// Create a new Lens focused on a specific node.
    pub fn focus_on_node(
        db: &'a AletheiaDB,
        node_id: NodeId,
        vector_property: &str,
    ) -> Result<Self> {
        let node = db.get_node(node_id)?;
        let vector = node
            .properties
            .get(vector_property)
            .and_then(|v| v.as_vector())
            .ok_or_else(|| {
                Error::Vector(VectorError::IndexError(format!(
                    "Node {} is missing vector property '{}'",
                    node_id, vector_property
                )))
            })?;

        Ok(Self::new(db, vector.to_vec(), vector_property))
    }

    /// Calculate the "Refraction" score (semantic relevance) of a node.
    /// Returns a value between -1.0 and 1.0 (cosine similarity).
    pub fn refract(&self, node_id: NodeId) -> Result<f32> {
        let node = self.db.get_node(node_id)?;
        let vector = node
            .properties
            .get(&self.vector_property)
            .and_then(|v| v.as_vector());

        match vector {
            Some(v) => cosine_similarity(&self.focus_vector, v),
            None => Ok(0.0), // Neutral relevance if no vector
        }
    }

    /// Calculate the "Relativity" score of an edge.
    /// How relevant is the connection between A and B *given* the focus?
    /// This is the average refraction of the two nodes.
    pub fn relativity(&self, start: NodeId, end: NodeId) -> Result<f32> {
        let start_score = self.refract(start)?;
        let end_score = self.refract(end)?;
        Ok((start_score + end_score) / 2.0)
    }

    /// "Magnify": Perform a BFS traversal starting from `start_node`,
    /// but only include nodes that have a refraction score > `threshold`.
    /// Returns a set of relevant node IDs within `max_depth`.
    pub fn magnify(
        &self,
        start_node: NodeId,
        max_depth: usize,
        threshold: f32,
    ) -> Result<HashSet<NodeId>> {
        let mut visited = HashSet::new();
        let mut queue = VecDeque::new();
        let mut relevant_nodes = HashSet::new();

        // Check start node
        if self.refract(start_node)? > threshold {
            queue.push_back((start_node, 0));
            visited.insert(start_node);
            relevant_nodes.insert(start_node);
        }

        while let Some((current, depth)) = queue.pop_front() {
            if depth >= max_depth {
                continue;
            }

            for edge_id in self.db.get_outgoing_edges(current) {
                let neighbor = self.db.get_edge_target(edge_id)?;

                if !visited.contains(&neighbor) {
                    visited.insert(neighbor);
                    let score = self.refract(neighbor)?;

                    if score > threshold {
                        relevant_nodes.insert(neighbor);
                        queue.push_back((neighbor, depth + 1));
                    }
                }
            }
        }

        Ok(relevant_nodes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::property::PropertyMapBuilder;
    use crate::index::vector::{DistanceMetric, HnswConfig};

    #[test]
    fn test_lens_refraction() {
        let db = AletheiaDB::new().unwrap();
        // Enable vector index to ensure vector logic works
        let config = HnswConfig::new(2, DistanceMetric::Cosine);
        db.enable_vector_index("vec", config).unwrap();

        // Create a node aligned with X-axis [1.0, 0.0]
        let props = PropertyMapBuilder::new()
            .insert_vector("vec", &[1.0, 0.0])
            .build();
        let node_x = db.create_node("NodeX", props).unwrap();

        // Create a Lens focused on X-axis [1.0, 0.0]
        let lens = Lens::new(&db, vec![1.0, 0.0], "vec");

        // Refraction should be 1.0
        let score = lens.refract(node_x).unwrap();
        assert!((score - 1.0).abs() < 1e-5);

        // Create a node aligned with Y-axis [0.0, 1.0]
        let props_y = PropertyMapBuilder::new()
            .insert_vector("vec", &[0.0, 1.0])
            .build();
        let node_y = db.create_node("NodeY", props_y).unwrap();

        // Refraction should be 0.0 (orthogonal)
        let score_y = lens.refract(node_y).unwrap();
        assert!(score_y.abs() < 1e-5);
    }

    #[test]
    fn test_lens_focus_on_node() {
        let db = AletheiaDB::new().unwrap();
        let config = HnswConfig::new(2, DistanceMetric::Cosine);
        db.enable_vector_index("vec", config).unwrap();

        let props = PropertyMapBuilder::new()
            .insert_vector("vec", &[0.707, 0.707])
            .build();
        let focus_node = db.create_node("Focus", props).unwrap();

        let lens = Lens::focus_on_node(&db, focus_node, "vec").unwrap();

        // Should have high refraction with itself
        let score = lens.refract(focus_node).unwrap();
        assert!((score - 1.0).abs() < 1e-5);
    }

    #[test]
    fn test_lens_magnify_bfs() {
        let db = AletheiaDB::new().unwrap();
        let config = HnswConfig::new(2, DistanceMetric::Cosine);
        db.enable_vector_index("vec", config).unwrap();

        // Graph Structure:
        // A -> B -> C
        // A -> D
        //
        // A: [1.0, 0.0] (Focus)
        // B: [0.9, 0.1] (Close)
        // C: [0.8, 0.2] (Close)
        // D: [0.0, 1.0] (Far / Orthogonal)

        let props_a = PropertyMapBuilder::new()
            .insert_vector("vec", &[1.0, 0.0])
            .build();
        let a = db.create_node("A", props_a).unwrap();

        let props_b = PropertyMapBuilder::new()
            .insert_vector("vec", &[0.9, 0.1]) // Normalized approx [0.99, 0.11]
            .build();
        let b = db.create_node("B", props_b).unwrap();

        let props_c = PropertyMapBuilder::new()
            .insert_vector("vec", &[0.8, 0.2])
            .build();
        let c = db.create_node("C", props_c).unwrap();

        let props_d = PropertyMapBuilder::new()
            .insert_vector("vec", &[0.0, 1.0])
            .build();
        let d = db.create_node("D", props_d).unwrap();

        // Edges
        db.create_edge(a, b, "NEXT", PropertyMapBuilder::new().build())
            .unwrap();
        db.create_edge(b, c, "NEXT", PropertyMapBuilder::new().build())
            .unwrap();
        db.create_edge(a, d, "NEXT", PropertyMapBuilder::new().build())
            .unwrap();

        // Lens focused on A
        let lens = Lens::new(&db, vec![1.0, 0.0], "vec");

        // Magnify with threshold 0.5
        // D has score 0.0 -> Should be excluded.
        // B, C have high scores -> Should be included.
        let result = lens.magnify(a, 2, 0.5).unwrap();

        assert!(result.contains(&a));
        assert!(result.contains(&b));
        assert!(result.contains(&c));
        assert!(!result.contains(&d));
    }
}
