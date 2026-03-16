#![allow(clippy::collapsible_if)]

//! Tapestry: Semantic Storyline Weaver 🧶
//!
//! "How did we get from A to B?"
//!
//! Tapestry discovers "Semantic Storylines"—paths through the graph where each individual step
//! makes logical sense (high semantic similarity between adjacent nodes), but the overall
//! journey results in a significant conceptual shift (high total displacement from start to end).
//!
//! # Concepts
//! - **Step Similarity**: The minimum cosine similarity required between any two adjacent nodes
//!   in the storyline. Ensures the narrative flows logically without jarring jumps.
//! - **Total Displacement**: The minimum cosine distance (1.0 - similarity) required between
//!   the start node and the end node. Ensures the storyline actually goes somewhere conceptually.
//!
//! # Use Cases
//! - **Evolutionary Paths**: Tracing how a biological species, an academic concept, or a
//!   technological innovation evolved through small, incremental changes.
//! - **Creative Ideation**: Finding a logical path of thought connecting two disparate ideas.
//!
//! # Example
//! ```rust,no_run
//! // Requires features = ["nova"]
//! use aletheiadb::AletheiaDB;
//! use aletheiadb::experimental::tapestry::Tapestry;
//! use aletheiadb::core::id::NodeId;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let db = AletheiaDB::new()?;
//! // ... setup graph ...
//! # let start_node = NodeId::new(0).unwrap();
//!
//! let tapestry = Tapestry::new(&db);
//!
//! // Find a storyline starting at `start_node` up to 5 steps long,
//! // where each step has at least 0.8 similarity,
//! // but the final node is at least 0.5 distance away from the start.
//! let storyline = tapestry.weave_storyline(start_node, "embedding", 0.8, 5, 0.5)?;
//!
//! if let Some(path) = storyline {
//!     println!("Found a storyline of length {}!", path.path.len());
//! }
//! # Ok(())
//! # }
//! ```

use crate::AletheiaDB;
use crate::core::error::{Error, Result};
use crate::core::id::NodeId;
use crate::core::vector::ops;
use std::collections::{HashSet, VecDeque};

/// The result of a Tapestry storyline search.
#[derive(Debug, Clone)]
pub struct Storyline {
    /// The sequence of nodes forming the storyline, from start to end.
    pub path: Vec<NodeId>,
    /// The total semantic displacement (cosine distance) between start and end nodes.
    pub total_displacement: f32,
    /// The lowest step similarity (cosine similarity) encountered along the path.
    pub min_step_similarity: f32,
}

/// The Tapestry Engine.
pub struct Tapestry<'a> {
    db: &'a AletheiaDB,
}

impl<'a> Tapestry<'a> {
    /// Create a new Tapestry Engine.
    pub fn new(db: &'a AletheiaDB) -> Self {
        Self { db }
    }

    /// Weave a semantic storyline starting from a given node.
    ///
    /// # Arguments
    /// * `start_node` - The beginning of the storyline.
    /// * `property_name` - The vector property to use for semantic analysis.
    /// * `min_step_similarity` - The minimum cosine similarity required between adjacent steps.
    /// * `max_depth` - The maximum number of edges the storyline can traverse.
    /// * `min_total_displacement` - The minimum cosine distance (1.0 - similarity) required
    ///   between the start node and the final node.
    ///
    /// # Returns
    /// An `Option<Storyline>` containing the first path found that satisfies all constraints.
    pub fn weave_storyline(
        &self,
        start_node: NodeId,
        property_name: &str,
        min_step_similarity: f32,
        max_depth: usize,
        min_total_displacement: f32,
    ) -> Result<Option<Storyline>> {
        if max_depth == 0 {
            return Ok(None);
        }

        // Helper to get vector for a node
        let get_vector = |node_id: NodeId| -> Result<Option<Vec<f32>>> {
            let node = self.db.get_node(node_id)?;
            if let Some(prop) = node.get_property(property_name) {
                if let Some(vec) = prop.as_vector() {
                    return Ok(Some(vec.to_vec()));
                }
            }
            Ok(None)
        };

        // Get the start node's vector
        let start_vector = match get_vector(start_node)? {
            Some(vec) => vec,
            None => {
                return Err(Error::other(format!(
                    "Start node {} does not have vector property '{}'",
                    start_node, property_name
                )));
            }
        };

        // BFS Queue: (current_node, current_path, current_vector, min_sim_so_far)
        let mut queue = VecDeque::new();
        queue.push_back((
            start_node,
            vec![start_node],
            start_vector.clone(),
            f32::MAX, // min_sim_so_far
        ));

        // Keep track of visited paths or visited nodes to prevent infinite loops.
        // We'll track visited nodes globally for simplicity, although ideally we'd track visited
        // states (node + path depth) if we wanted to find all paths. For a "find first" approach,
        // visited nodes is fine and prevents cycles.
        let mut visited = HashSet::new();
        visited.insert(start_node);

        while let Some((curr_node, path, curr_vector, min_sim)) = queue.pop_front() {
            if path.len() > max_depth + 1 {
                // max_depth is max edges. max_depth + 1 is max nodes.
                continue;
            }

            // Check if current path satisfies the total displacement condition
            // (We require at least 1 step to have a meaningful storyline)
            if path.len() > 1 {
                let displacement = 1.0_f32 - ops::cosine_similarity(&start_vector, &curr_vector)?;
                if displacement >= min_total_displacement {
                    // We found a valid storyline!
                    return Ok(Some(Storyline {
                        path,
                        total_displacement: displacement,
                        min_step_similarity: min_sim,
                    }));
                }
            }

            // Get neighbors
            let outgoing_edges = self.db.get_outgoing_edges(curr_node);
            for edge_id in outgoing_edges {
                let neighbor_node = self.db.get_edge_target(edge_id)?;

                if visited.contains(&neighbor_node) {
                    continue; // Avoid cycles
                }

                if let Some(neighbor_vector) = get_vector(neighbor_node)? {
                    let step_similarity = ops::cosine_similarity(&curr_vector, &neighbor_vector)?;

                    if step_similarity >= min_step_similarity {
                        visited.insert(neighbor_node);
                        let mut new_path = path.clone();
                        new_path.push(neighbor_node);
                        let new_min_sim = min_sim.min(step_similarity);

                        queue.push_back((neighbor_node, new_path, neighbor_vector, new_min_sim));
                    }
                }
            }
        }

        // No storyline found matching criteria
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::transaction::WriteOps;
    use crate::core::property::PropertyMapBuilder;

    #[test]
    fn test_tapestry_finds_valid_storyline() {
        let db = AletheiaDB::new().unwrap();

        // A (1, 0)
        // B (0.707, 0.707) -> ~0.707 sim to A
        // C (0, 1) -> ~0.707 sim to B, 0 sim to A (1.0 dist to A)

        let mut a = NodeId::new(0).unwrap();
        let mut b = NodeId::new(0).unwrap();
        let mut c = NodeId::new(0).unwrap();

        db.write(|tx| {
            a = tx
                .create_node(
                    "Concept",
                    PropertyMapBuilder::new()
                        .insert_vector("vec", &[1.0, 0.0])
                        .build(),
                )
                .unwrap();

            b = tx
                .create_node(
                    "Concept",
                    PropertyMapBuilder::new()
                        .insert_vector("vec", &[0.707, 0.707])
                        .build(),
                )
                .unwrap();

            c = tx
                .create_node(
                    "Concept",
                    PropertyMapBuilder::new()
                        .insert_vector("vec", &[0.0, 1.0])
                        .build(),
                )
                .unwrap();

            tx.create_edge(a, b, "EVOLVES_TO", Default::default())
                .unwrap();
            tx.create_edge(b, c, "EVOLVES_TO", Default::default())
                .unwrap();
            Ok::<(), Error>(())
        })
        .unwrap();

        let tapestry = Tapestry::new(&db);

        // We want a path where each step has >= 0.7 sim, but total displacement >= 0.9
        let storyline = tapestry.weave_storyline(a, "vec", 0.7, 5, 0.9).unwrap();

        assert!(storyline.is_some());
        let storyline = storyline.unwrap();

        assert_eq!(storyline.path, vec![a, b, c]);
        assert!(storyline.total_displacement >= 0.9);
        assert!(storyline.min_step_similarity >= 0.7);
    }

    #[test]
    fn test_tapestry_rejects_low_step_similarity() {
        let db = AletheiaDB::new().unwrap();

        // A (1, 0)
        // B (0, 1) -> 0 sim to A (huge jump)
        // C (-1, 0)

        let mut a = NodeId::new(0).unwrap();
        let mut b = NodeId::new(0).unwrap();
        let mut c = NodeId::new(0).unwrap();

        db.write(|tx| {
            a = tx
                .create_node(
                    "Concept",
                    PropertyMapBuilder::new()
                        .insert_vector("vec", &[1.0, 0.0])
                        .build(),
                )
                .unwrap();

            b = tx
                .create_node(
                    "Concept",
                    PropertyMapBuilder::new()
                        .insert_vector("vec", &[0.0, 1.0])
                        .build(),
                )
                .unwrap();

            c = tx
                .create_node(
                    "Concept",
                    PropertyMapBuilder::new()
                        .insert_vector("vec", &[-1.0, 0.0])
                        .build(),
                )
                .unwrap();

            tx.create_edge(a, b, "EVOLVES_TO", Default::default())
                .unwrap();
            tx.create_edge(b, c, "EVOLVES_TO", Default::default())
                .unwrap();
            Ok::<(), Error>(())
        })
        .unwrap();

        let tapestry = Tapestry::new(&db);

        // Require step sim >= 0.7
        let storyline = tapestry.weave_storyline(a, "vec", 0.7, 5, 0.9).unwrap();

        // Should find nothing because A->B is a huge jump
        assert!(storyline.is_none());
    }
}
