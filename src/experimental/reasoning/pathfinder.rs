#![allow(clippy::collapsible_if)]
//! Pathfinder: Semantic Pathfinding Engine.
//!
//! "Follow the train of thought."
//!
//! Pathfinder implements `SemanticPathfinding`. Instead of finding the shortest
//! structural path (like BFS/Dijkstra), it navigates the graph by following edges
//! that progressively move closer to a target semantic concept (vector embedding).
//!
//! # Concepts
//! - **Train of Thought**: A path through the graph where each step makes semantic sense
//!   towards a goal, even if it's not the structurally shortest route.
//! - **Semantic Gradient**: The measure of vector similarity used to choose the next hop.
//!
//! # Use Cases
//! - **Concept Bridging**: Finding how two disparate ideas connect through intermediate thoughts.
//! - **Agent Navigation**: Allowing an LLM agent to explore a knowledge graph by "sniffing out"
//!   the most relevant next steps.
//!
//! # Example
//! ```rust,no_run
//! use aletheiadb::AletheiaDB;
//! use aletheiadb::experimental::reasoning::pathfinder::SemanticPathfinder;
//! use aletheiadb::core::id::NodeId;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let db = AletheiaDB::new()?;
//! let pathfinder = SemanticPathfinder::new(&db);
//!
//! # let start_node = NodeId::new(1).unwrap();
//! let target_concept = vec![0.5, 0.2, 0.8];
//!
//! // Navigate up to 10 hops towards the target concept
//! let path = pathfinder.navigate(start_node, "embedding", &target_concept, 10)?;
//!
//! println!("Found semantic path of length {}", path.len());
//! # Ok(())
//! # }
//! ```

use crate::AletheiaDB;
use crate::core::error::Result;
use crate::core::id::NodeId;
use crate::core::vector::{cosine_similarity, normalize};
use std::collections::HashSet;

/// The Semantic Pathfinder Engine.
pub struct SemanticPathfinder<'a> {
    db: &'a AletheiaDB,
}

#[cfg(feature = "semantic-reasoning")]
impl<'a> SemanticPathfinder<'a> {
    /// Create a new Pathfinder instance.
    pub fn new(db: &'a AletheiaDB) -> Self {
        Self { db }
    }

    /// Navigate the graph from a start node towards a target semantic concept.
    /// Uses greedy best-first search based on cosine similarity.
    ///
    /// # Arguments
    /// * `start_node` - The node to start the journey from.
    /// * `property` - The name of the vector property to use for similarity.
    /// * `target_embedding` - The vector we are trying to approach.
    /// * `max_hops` - Maximum length of the path.
    pub fn navigate(
        &self,
        start_node: NodeId,
        property: &str,
        target_embedding: &[f32],
        max_hops: usize,
    ) -> Result<Vec<NodeId>> {
        let mut path = Vec::new();
        let mut visited = HashSet::new();

        let mut current_node = start_node;
        path.push(current_node);
        visited.insert(current_node);

        // Normalize target for cosine similarity
        let target_norm = normalize(target_embedding);

        for _ in 0..max_hops {
            let edges = self.db.get_outgoing_edges(current_node);

            let mut best_neighbor = None;
            let mut best_similarity = f32::NEG_INFINITY;

            for edge_id in edges {
                if let Ok(edge) = self.db.get_edge(edge_id) {
                    let neighbor = edge.target;

                    if visited.contains(&neighbor) {
                        continue;
                    }

                    if let Ok(node) = self.db.get_node(neighbor) {
                        if let Some(prop) = node.get_property(property) {
                            if let Some(vec) = prop.as_vector() {
                                // Calculate similarity
                                let neighbor_norm = normalize(vec);
                                if let Ok(sim) = cosine_similarity(&neighbor_norm, &target_norm) {
                                    if sim > best_similarity {
                                        best_similarity = sim;
                                        best_neighbor = Some(neighbor);
                                    }
                                }
                            }
                        }
                    }
                }
            }

            match best_neighbor {
                Some(next_node) => {
                    path.push(next_node);
                    visited.insert(next_node);
                    current_node = next_node;
                }
                None => {
                    // Dead end or no unvisited neighbors with the property
                    break;
                }
            }
        }

        Ok(path)
    }
}

#[cfg(all(test, feature = "semantic-reasoning"))]
mod tests {
    use super::*;
    use crate::PropertyMapBuilder;
    use crate::api::transaction::WriteOps;

    #[test]
    fn test_semantic_pathfinder() {
        let db = AletheiaDB::new().unwrap();

        let mut n1 = NodeId::new(1).unwrap();
        let mut n2 = NodeId::new(2).unwrap();
        let mut n3 = NodeId::new(3).unwrap();
        let mut n4 = NodeId::new(4).unwrap();

        db.write(|tx| {
            // A path that makes semantic sense towards [1.0, 1.0]:
            // n1 [0.0, 0.0] -> n2 [0.5, 0.0] -> n3 [0.8, 0.8]
            // We also add a distractor n4 [-1.0, 1.0] connected to n1

            n1 = tx.create_node("Concept", PropertyMapBuilder::new().insert_vector("vec", &[0.0, 0.0]).build()).unwrap();
            n2 = tx.create_node("Concept", PropertyMapBuilder::new().insert_vector("vec", &[0.5, 0.0]).build()).unwrap();
            n3 = tx.create_node("Concept", PropertyMapBuilder::new().insert_vector("vec", &[0.8, 0.8]).build()).unwrap();
            n4 = tx.create_node("Concept", PropertyMapBuilder::new().insert_vector("vec", &[-1.0, 1.0]).build()).unwrap();

            tx.create_edge(n1, n2, "NEXT", Default::default()).unwrap();
            tx.create_edge(n1, n4, "NEXT", Default::default()).unwrap();
            tx.create_edge(n2, n3, "NEXT", Default::default()).unwrap();

            Ok::<(), crate::core::error::Error>(())
        }).unwrap();

        let pathfinder = SemanticPathfinder::new(&db);
        let target = vec![1.0, 1.0];

        let path = pathfinder.navigate(n1, "vec", &target, 5).unwrap();

        // It should prefer n2 over n4 because n2 has a better cosine similarity to [1.0, 1.0].
        // Then from n2 it goes to n3.
        assert_eq!(path, vec![n1, n2, n3]);
    }
}
