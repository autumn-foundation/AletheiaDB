//! Voyager: Semantic Path Navigation Engine.
//!
//! "A journey of a thousand miles begins with a single vector."
//!
//! Voyager performs a guided traversal (a semantic walk) through the graph.
//! Instead of picking edges randomly or using standard BFS/DFS, Voyager
//! evaluates the semantic vectors of neighbors and chooses the next step
//! based on a user-defined "Explorer Mode".
//!
//! # Modes
//! - **Exploitation**: Prefers neighbors that are highly similar to the current node (deep dives).
//! - **Exploration**: Prefers neighbors that are orthogonal or different (brainstorming/lateral thinking).
//!
//! # Example
//! ```rust,no_run
//! use aletheiadb::AletheiaDB;
//! use aletheiadb::experimental::reasoning::voyager::{Voyager, ExplorerMode};
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let db = AletheiaDB::new()?;
//! let voyager = Voyager::new(&db);
//!
//! # let start_node = aletheiadb::core::id::NodeId::new(1).unwrap();
//! // Take a 5-hop walk, always seeking new/different concepts
//! let path = voyager.walk(start_node, 5, "embedding", ExplorerMode::Exploration)?;
//! # Ok(())
//! # }
//! ```

use crate::AletheiaDB;
use crate::core::error::Result;
use crate::core::id::NodeId;
use crate::core::vector::ops::cosine_similarity;

/// The strategy Voyager uses to pick the next step.
#[cfg(feature = "semantic-reasoning")]
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ExplorerMode {
    /// Prefers the neighbor most semantically similar to the current node.
    Exploitation,
    /// Prefers the neighbor least semantically similar to the current node.
    Exploration,
}

/// The Voyager Engine.
#[cfg(feature = "semantic-reasoning")]
pub struct Voyager<'a> {
    db: &'a AletheiaDB,
}

#[cfg(feature = "semantic-reasoning")]
impl<'a> Voyager<'a> {
    /// Create a new Voyager instance.
    pub fn new(db: &'a AletheiaDB) -> Self {
        Self { db }
    }

    /// Perform a semantic walk starting from `start_node`.
    pub fn walk(
        &self,
        start_node: NodeId,
        steps: usize,
        vector_property: &str,
        mode: ExplorerMode,
    ) -> Result<Vec<NodeId>> {
        let mut path = vec![start_node];
        let mut current_node = start_node;

        for _ in 0..steps {
            let edges = self.db.get_outgoing_edges(current_node);
            if edges.is_empty() {
                break; // Dead end
            }

            let current_node_data = self.db.get_node(current_node)?;
            let current_vec_opt = current_node_data
                .get_property(vector_property)
                .and_then(|p| p.as_vector())
                .map(|v| v.to_vec());

            let mut candidates: Vec<(NodeId, f32)> = Vec::new();

            for edge_id in edges {
                if let Ok(edge) = self.db.get_edge(edge_id) {
                    let target = edge.target;

                    // Don't immediately loop back to the exact node we just came from
                    // (Though longer cycles are allowed to let it wander)
                    if path.len() > 1 && path[path.len() - 2] == target {
                        continue;
                    }

                    if let Ok(target_node) = self.db.get_node(target) {
                        let target_vec_opt = target_node
                            .get_property(vector_property)
                            .and_then(|p| p.as_vector());

                        match (&current_vec_opt, target_vec_opt) {
                            (Some(curr_v), Some(tgt_v)) => {
                                let sim = cosine_similarity(curr_v, tgt_v).unwrap_or(0.0);
                                candidates.push((target, sim));
                            }
                            _ => {
                                // If vectors are missing, assign a neutral similarity
                                candidates.push((target, 0.0));
                            }
                        }
                    }
                }
            }

            if candidates.is_empty() {
                break;
            }

            // Sort candidates based on the mode
            candidates.sort_by(|a, b| match mode {
                ExplorerMode::Exploitation => {
                    b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal)
                }
                ExplorerMode::Exploration => {
                    a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal)
                }
            });

            // Pick the best candidate
            let next_node = candidates.first().unwrap().0;
            path.push(next_node);
            current_node = next_node;
        }

        Ok(path)
    }
}

#[cfg(all(test, feature = "semantic-reasoning"))]
mod tests {
    use super::*;
    use crate::core::property::PropertyMapBuilder;

    #[test]
    fn test_voyager_exploration_vs_exploitation() {
        let db = AletheiaDB::new().unwrap();

        // Node A: Base concept [1.0, 0.0, 0.0]
        let a = db
            .create_node(
                "Concept",
                PropertyMapBuilder::new()
                    .insert_vector("vec", &[1.0, 0.0, 0.0])
                    .build(),
            )
            .unwrap();

        // Node B: Similar to A [0.9, 0.1, 0.0]
        let b = db
            .create_node(
                "Concept",
                PropertyMapBuilder::new()
                    .insert_vector("vec", &[0.9, 0.1, 0.0])
                    .build(),
            )
            .unwrap();

        // Node C: Very different from A [0.0, 1.0, 0.0]
        let c = db
            .create_node(
                "Concept",
                PropertyMapBuilder::new()
                    .insert_vector("vec", &[0.0, 1.0, 0.0])
                    .build(),
            )
            .unwrap();

        db.create_edge(a, b, "RELATES_TO", Default::default())
            .unwrap();
        db.create_edge(a, c, "RELATES_TO", Default::default())
            .unwrap();

        let voyager = Voyager::new(&db);

        // Exploitation should prefer B
        let path_exploit = voyager
            .walk(a, 1, "vec", ExplorerMode::Exploitation)
            .unwrap();
        assert_eq!(path_exploit, vec![a, b]);

        // Exploration should prefer C
        let path_explore = voyager
            .walk(a, 1, "vec", ExplorerMode::Exploration)
            .unwrap();
        assert_eq!(path_explore, vec![a, c]);
    }
}
