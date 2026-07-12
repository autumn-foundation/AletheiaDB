#![allow(clippy::collapsible_if)]
//! Wayfinder: Semantic A* Pathfinding.
//!
//! "Navigate the graph using semantic stars."
//!
//! Wayfinder is an experimental pathfinding algorithm that combines structural
//! graph traversal with semantic vector similarity. It uses A* where the
//! heuristic cost is derived from the vector distance (cosine similarity)
//! to the target node.
//!
//! # Concepts
//! - **Structural Cost ($g(n)$)**: The number of hops taken so far.
//! - **Semantic Heuristic ($h(n)$)**: `1.0 - cosine_similarity(node, target)`.
//!   Nodes closer in vector space are "closer" to the goal.
//!
//! # Use Cases
//! - **Concept Interpolation**: Finding a path of concepts that bridge A and B.
//! - **Narrative Generation**: Generating a story that connects two events logically.
//!
//! # Example
//! ```rust,no_run
//! use aletheiadb::AletheiaDB;
//! use aletheiadb::experimental::reasoning::wayfinder::Wayfinder;
//! use aletheiadb::core::id::NodeId;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let db = AletheiaDB::new()?;
//! # let start_node = NodeId::new(1).unwrap();
//! # let target_node = NodeId::new(2).unwrap();
//! let wayfinder = Wayfinder::new(&db);
//!
//! // Find a path from start to target using the "embedding" property
//! let path = wayfinder.find_path(start_node, target_node, "embedding", 5)?;
//!
//! for step in path {
//!     println!("Node: {}", step);
//! }
//! # Ok(())
//! # }
//! ```

use crate::AletheiaDB;
use crate::api::transaction::ReadOps;
use crate::core::error::{Error, Result};
use crate::core::id::NodeId;
use crate::core::vector::ops;
use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashMap, HashSet};

/// The Wayfinder Pathfinding Engine.
pub struct Wayfinder<'a> {
    db: &'a AletheiaDB,
}

#[derive(Debug, Clone)]
struct Waypoint {
    node_id: NodeId,
    g_score: f32, // Structural cost (hops)
    f_score: f32, // f(n) = g(n) + h(n)
}

impl PartialEq for Waypoint {
    fn eq(&self, other: &Self) -> bool {
        self.node_id == other.node_id
    }
}

impl Eq for Waypoint {}

impl PartialOrd for Waypoint {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Waypoint {
    fn cmp(&self, other: &Self) -> Ordering {
        // BinaryHeap is a max-heap, we want a min-heap based on f_score.
        other.f_score.partial_cmp(&self.f_score).unwrap_or(Ordering::Equal)
    }
}

impl<'a> Wayfinder<'a> {
    /// Create a new Wayfinder.
    pub fn new(db: &'a AletheiaDB) -> Self {
        Self { db }
    }

    /// Find a path using Semantic A*.
    ///
    /// # Arguments
    /// * `start` - The starting node ID.
    /// * `target` - The destination node ID.
    /// * `vector_property` - The name of the property storing the vector.
    /// * `max_depth` - The maximum number of hops to consider.
    pub fn find_path(
        &self,
        start: NodeId,
        target: NodeId,
        vector_property: &str,
        max_depth: usize,
    ) -> Result<Vec<NodeId>> {
        if start == target {
            return Ok(vec![start]);
        }

        // Get target vector for heuristic
        let target_vec_opt =
            self.db
                .read(|tx| -> std::result::Result<Option<Vec<f32>>, Error> {
                    if let Ok(node) = tx.get_node(target) {
                        if let Some(prop) = node.get_property(vector_property) {
                            if let Some(vec) = prop.as_vector() {
                                return Ok(Some(vec.to_vec()));
                            }
                        }
                    }
                    Ok(None)
                })?;

        let target_vec = match target_vec_opt {
            Some(v) => v,
            None => {
                return Err(Error::other(format!(
                    "Target node {} missing vector property '{}'",
                    target, vector_property
                )));
            }
        };

        let mut open_set = BinaryHeap::new();
        let mut came_from: HashMap<NodeId, NodeId> = HashMap::new();

        // g_score tracks the shortest known structural distance to a node
        let mut g_score: HashMap<NodeId, f32> = HashMap::new();
        g_score.insert(start, 0.0);

        // Initial heuristic
        let start_h = self
            .heuristic(start, &target_vec, vector_property)
            .unwrap_or(1.0); // Assume max distance if missing

        open_set.push(Waypoint {
            node_id: start,
            g_score: 0.0,
            f_score: start_h,
        });

        // Set to avoid re-expanding nodes too much, though A* with consistent heuristic handles this
        let mut closed_set = HashSet::new();

        while let Some(current) = open_set.pop() {
            if current.node_id == target {
                return Ok(self.reconstruct_path(&came_from, current.node_id));
            }

            // Limit depth
            if current.g_score >= max_depth as f32 {
                continue;
            }

            closed_set.insert(current.node_id);

            // Get neighbors
            let neighbors = self.db.read(|tx| {
                let mut out = Vec::new();
                if let Ok(edges) = tx.get_outgoing_edges(current.node_id) {
                    for edge_id in edges {
                        if let Ok(edge) = tx.get_edge(edge_id) {
                            out.push(edge.target);
                        }
                    }
                }
                Ok::<Vec<NodeId>, Error>(out)
            })?;

            for neighbor in neighbors {
                if closed_set.contains(&neighbor) {
                    continue;
                }

                // Distance between connected nodes is 1 hop
                let tentative_g_score = current.g_score + 1.0;

                let neighbor_g = *g_score.get(&neighbor).unwrap_or(&f32::INFINITY);

                if tentative_g_score < neighbor_g {
                    // This path is better than any previous one
                    came_from.insert(neighbor, current.node_id);
                    g_score.insert(neighbor, tentative_g_score);

                    let h_score = self
                        .heuristic(neighbor, &target_vec, vector_property)
                        .unwrap_or(1.0);
                    let f_score = tentative_g_score + h_score;

                    open_set.push(Waypoint {
                        node_id: neighbor,
                        g_score: tentative_g_score,
                        f_score,
                    });
                }
            }
        }

        // Target not reachable within max_depth or disconnected
        Err(Error::other("Path not found"))
    }

    /// Calculate the semantic heuristic.
    ///
    /// Cost = 1.0 - cosine_similarity.
    /// Range: [0.0, 2.0]. Lower is better.
    fn heuristic(&self, node_id: NodeId, target_vec: &[f32], vector_property: &str) -> Result<f32> {
        let vec_opt = self
            .db
            .read(|tx| -> std::result::Result<Option<Vec<f32>>, Error> {
                if let Ok(node) = tx.get_node(node_id) {
                    if let Some(prop) = node.get_property(vector_property) {
                        if let Some(vec) = prop.as_vector() {
                            return Ok(Some(vec.to_vec()));
                        }
                    }
                }
                Ok(None)
            })?;

        if let Some(mut v) = vec_opt {
            let mut t = target_vec.to_vec();
            ops::normalize_in_place(&mut v);
            ops::normalize_in_place(&mut t);

            // Handle precision issues where cos_sim > 1.0 occasionally
            let sim = ops::cosine_similarity(&v, &t)?.clamp(-1.0, 1.0);

            // Distance is 1 - similarity.
            // Sim 1.0 -> dist 0.0
            // Sim -1.0 -> dist 2.0
            Ok(1.0 - sim)
        } else {
            // Node has no vector, give it a neutral heuristic or max penalty?
            // Neutral penalty prevents it from being prioritized over semantic paths
            Ok(1.0)
        }
    }

    fn reconstruct_path(
        &self,
        came_from: &HashMap<NodeId, NodeId>,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::transaction::WriteOps;
    use crate::core::property::PropertyMapBuilder;

    #[test]
    fn test_wayfinder_semantic_path() {
        let db = AletheiaDB::new().unwrap();

        // 1. Setup Nodes
        // S (Start): [1.0, 0.0]
        // A (Bad path): [-1.0, 0.0] -> goes away from target semantically
        // B (Good path): [0.7, 0.7] -> gets closer to target semantically
        // T (Target): [0.0, 1.0]

        let create_node = |vec: &[f32]| -> NodeId {
            db.write(|tx| {
                let props = PropertyMapBuilder::new().insert_vector("vec", vec).build();
                Ok::<NodeId, Error>(tx.create_node("Node", props).unwrap())
            })
            .unwrap()
        };

        let start = create_node(&[1.0, 0.0]);
        let a = create_node(&[-1.0, 0.0]);
        let b = create_node(&[0.7, 0.7]);
        let target = create_node(&[0.0, 1.0]);

        // 2. Setup Edges
        db.write(|tx| {
            // S -> A and S -> B
            tx.create_edge(start, a, "NEXT", Default::default())
                .unwrap();
            tx.create_edge(start, b, "NEXT", Default::default())
                .unwrap();

            // Both A and B connect to Target
            tx.create_edge(a, target, "NEXT", Default::default())
                .unwrap();
            tx.create_edge(b, target, "NEXT", Default::default())
                .unwrap();

            Ok::<(), Error>(())
        })
        .unwrap();

        // 3. Find Path
        let wayfinder = Wayfinder::new(&db);
        let path = wayfinder.find_path(start, target, "vec", 5).unwrap();

        // Path should be S -> B -> T because B's heuristic is much better than A's.
        // H(A) to T = 1.0 - (-0.0) = 1.0? Wait. cos_sim([-1,0], [0,1]) = 0.
        // H(A) to T = 1.0 - 0.0 = 1.0
        // H(B) to T = 1.0 - 0.707 = 0.293
        // So B is preferred.
        assert_eq!(path, vec![start, b, target]);
    }
}
