//! Spectre: Semantic Perspective Engine.
//!
//! "It depends on how you look at it."
//!
//! Spectre allows you to view the graph through a specific "Lens" (Concept).
//! It modifies graph traversals by warping the effective distance between nodes
//! based on their alignment with the Lens.
//!
//! # Concepts
//! - **Lens**: A vector defining a semantic axis (e.g., "Risk", "Innovation").
//! - **Focus**: The alignment of a node with the Lens.
//! - **Subjective Traversal**: Pathfinding where "cost" is determined by alignment.
//!   A path through "Safety" looks different from a path through "Speed".
//!
//! # Example
//!
//! ```rust,no_run
//! use aletheiadb::AletheiaDB;
//! use aletheiadb::experimental::spectre::{Spectre, Lens};
//! use aletheiadb::core::id::NodeId;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let db = AletheiaDB::new()?;
//! let spectre = Spectre::new(&db);
//!
//! // Define Lenses
//! let safety_lens = Lens::new(&[0.0, 1.0]); // Y-axis is Safety
//! let speed_lens = Lens::new(&[1.0, 0.0]);  // X-axis is Speed
//!
//! # let start = NodeId::new(1).unwrap();
//! # let end = NodeId::new(5).unwrap();
//!
//! // Find the "Safe" path
//! let safe_path = spectre.traverse(start, end, &safety_lens, "embedding")?;
//!
//! // Find the "Fast" path
//! let fast_path = spectre.traverse(start, end, &speed_lens, "embedding")?;
//! # Ok(())
//! # }
//! ```

use crate::AletheiaDB;
use crate::core::error::{Error, Result};
use crate::core::id::NodeId;
use crate::core::vector::ops::normalize;
use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashMap};

/// A semantic lens defining a perspective.
#[derive(Debug, Clone)]
pub struct Lens {
    /// The normalized direction vector of the perspective.
    pub vector: Vec<f32>,
}

impl Lens {
    /// Create a new Lens from a vector.
    pub fn new(vector: &[f32]) -> Self {
        Self {
            vector: normalize(vector),
        }
    }
}

/// The Spectre Engine for subjective graph analysis.
pub struct Spectre<'a> {
    db: &'a AletheiaDB,
}

#[derive(Clone, Copy, PartialEq)]
struct State {
    cost: f32,
    node: NodeId,
}

impl Eq for State {}

impl Ord for State {
    fn cmp(&self, other: &Self) -> Ordering {
        // Min-heap priority queue
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

impl<'a> Spectre<'a> {
    /// Create a new Spectre instance.
    pub fn new(db: &'a AletheiaDB) -> Self {
        Self { db }
    }

    /// Calculate the alignment of a node with the lens (Focus Score).
    ///
    /// # Returns
    /// A score between -1.0 (Opposed) and 1.0 (Aligned).
    pub fn focus(&self, node_id: NodeId, lens: &Lens, property: &str) -> Result<f32> {
        let node = self.db.get_node(node_id)?;
        let vec = node
            .properties
            .get(property)
            .and_then(|v| v.as_vector())
            .ok_or_else(|| {
                Error::other(format!(
                    "Node {} missing vector property '{}'",
                    node_id, property
                ))
            })?;

        // Cosine similarity
        crate::core::vector::cosine_similarity(vec, &lens.vector)
    }

    /// Find a path from `start` to `end` that maximizes alignment with the Lens.
    ///
    /// This performs Dijkstra's algorithm where the cost of visiting a node
    /// is inversely proportional to its alignment with the Lens.
    ///
    /// Cost function: `1.0 - focus(node)`
    /// - Perfect Alignment (1.0) -> Cost 0.0
    /// - Orthogonal (0.0) -> Cost 1.0
    /// - Opposite (-1.0) -> Cost 2.0
    ///
    /// Nodes missing the vector property are penalized with Cost 2.0.
    pub fn traverse(
        &self,
        start: NodeId,
        end: NodeId,
        lens: &Lens,
        vector_prop: &str,
    ) -> Result<Vec<NodeId>> {
        if start == end {
            return Ok(vec![start]);
        }

        let mut pq = BinaryHeap::new();
        pq.push(State {
            cost: 0.0,
            node: start,
        });

        let mut dist = HashMap::new();
        dist.insert(start, 0.0);

        let mut came_from = HashMap::new();

        while let Some(State { cost, node }) = pq.pop() {
            if node == end {
                return Ok(self.reconstruct_path(came_from, end));
            }

            // Stale entry check
            if cost > *dist.get(&node).unwrap_or(&f32::INFINITY) {
                continue;
            }

            for edge_id in self.db.get_outgoing_edges(node) {
                // Ignore edge fetch errors
                if let Ok(neighbor) = self.db.get_edge_target(edge_id) {
                    // Calculate cost to enter neighbor based on semantic alignment
                    let focus_score = self.focus(neighbor, lens, vector_prop).unwrap_or(-1.0); // High penalty for missing info

                    // Map [-1, 1] to [2, 0] cost
                    // We add a tiny epsilon to ensure non-zero cost for cycles unless perfect alignment
                    let step_cost = 1.0 - focus_score + 0.001;
                    let new_cost = cost + step_cost;

                    if new_cost < *dist.get(&neighbor).unwrap_or(&f32::INFINITY) {
                        dist.insert(neighbor, new_cost);
                        came_from.insert(neighbor, node);
                        pq.push(State {
                            cost: new_cost,
                            node: neighbor,
                        });
                    }
                }
            }
        }

        Ok(Vec::new()) // No path found
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
    use crate::core::property::PropertyMapBuilder;
    use crate::index::vector::{DistanceMetric, HnswConfig};

    fn create_db() -> AletheiaDB {
        let db = AletheiaDB::new().unwrap();
        db.enable_vector_index("vec", HnswConfig::new(2, DistanceMetric::Cosine))
            .unwrap();
        db
    }

    #[test]
    fn test_spectre_focus() {
        let db = create_db();
        // Node: [1, 0]
        let props = PropertyMapBuilder::new()
            .insert_vector("vec", &[1.0, 0.0])
            .build();
        let node = db.create_node("Node", props).unwrap();

        let spectre = Spectre::new(&db);

        // Lens X: [1, 0] -> Focus 1.0
        let lens_x = Lens::new(&[1.0, 0.0]);
        let focus_x = spectre.focus(node, &lens_x, "vec").unwrap();
        assert!((focus_x - 1.0).abs() < 1e-5);

        // Lens Y: [0, 1] -> Focus 0.0
        let lens_y = Lens::new(&[0.0, 1.0]);
        let focus_y = spectre.focus(node, &lens_y, "vec").unwrap();
        assert!((focus_y - 0.0).abs() < 1e-5);
    }

    #[test]
    fn test_spectre_subjective_traversal() {
        let db = create_db();

        // Topology:
        // Start -> HighRoad -> End
        // Start -> LowRoad -> End

        // HighRoad: [0, 1] (Aligned with Y)
        // LowRoad:  [1, 0] (Aligned with X)

        let p_start = PropertyMapBuilder::new()
            .insert_vector("vec", &[0.0, 0.0])
            .build();
        let start = db.create_node("Start", p_start).unwrap();

        let p_end = PropertyMapBuilder::new()
            .insert_vector("vec", &[0.0, 0.0])
            .build();
        let end = db.create_node("End", p_end).unwrap();

        let p_high = PropertyMapBuilder::new()
            .insert_vector("vec", &[0.0, 1.0])
            .build();
        let high = db.create_node("HighRoad", p_high).unwrap();

        let p_low = PropertyMapBuilder::new()
            .insert_vector("vec", &[1.0, 0.0])
            .build();
        let low = db.create_node("LowRoad", p_low).unwrap();

        // Edges
        db.create_edge(start, high, "path", Default::default())
            .unwrap();
        db.create_edge(high, end, "path", Default::default())
            .unwrap();

        db.create_edge(start, low, "path", Default::default())
            .unwrap();
        db.create_edge(low, end, "path", Default::default())
            .unwrap();

        let spectre = Spectre::new(&db);

        // 1. Perspective Y (Favor HighRoad)
        let lens_y = Lens::new(&[0.0, 1.0]);
        let path_y = spectre.traverse(start, end, &lens_y, "vec").unwrap();
        assert_eq!(
            path_y,
            vec![start, high, end],
            "Should take HighRoad with Y lens"
        );

        // 2. Perspective X (Favor LowRoad)
        let lens_x = Lens::new(&[1.0, 0.0]);
        let path_x = spectre.traverse(start, end, &lens_x, "vec").unwrap();
        assert_eq!(
            path_x,
            vec![start, low, end],
            "Should take LowRoad with X lens"
        );
    }
}
