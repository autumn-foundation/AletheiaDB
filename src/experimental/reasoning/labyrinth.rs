//! Labyrinth: Random Walk Path Generator.
//!
//! "Wander the graph."
//!
//! Labyrinth performs a random walk starting from a given node, simulating
//! exploration. This pattern is foundational for algorithms like Node2Vec
//! or DeepWalk, allowing the graph structure to be sampled into sequential
//! paths that can be fed into language models or embedding generators.
//!
//! # Example
//! ```rust,no_run
//! use aletheiadb::AletheiaDB;
//! use aletheiadb::experimental::reasoning::labyrinth::Labyrinth;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let db = AletheiaDB::new()?;
//! let labyrinth = Labyrinth::new(&db);
//!
//! # let start_node = aletheiadb::core::id::NodeId::new(1).unwrap();
//! // Perform a random walk of 5 steps
//! let path = labyrinth.walk(start_node, 5)?;
//! for step in path {
//!     println!("Visited node: {}", step.as_u64());
//! }
//! # Ok(())
//! # }
//! ```

use crate::AletheiaDB;
use crate::core::error::Result;
use crate::core::id::NodeId;
use rand::seq::SliceRandom;
use rand::thread_rng;

/// The Labyrinth Random Walk Engine.
pub struct Labyrinth<'a> {
    db: &'a AletheiaDB,
}

#[cfg(feature = "semantic-reasoning")]
impl<'a> Labyrinth<'a> {
    /// Create a new Labyrinth engine.
    pub fn new(db: &'a AletheiaDB) -> Self {
        Self { db }
    }

    /// Performs a random walk of up to `steps` length starting from `start_node`.
    ///
    /// The walk stops early if it reaches a node with no outgoing edges.
    pub fn walk(&self, start_node: NodeId, steps: usize) -> Result<Vec<NodeId>> {
        let mut path = Vec::with_capacity(steps + 1);
        path.push(start_node);

        let mut current_node = start_node;
        let mut rng = thread_rng();

        for _ in 0..steps {
            let edges = self.db.get_outgoing_edges(current_node);

            if edges.is_empty() {
                break;
            }

            // Pick a random edge
            let chosen_edge_id = edges.choose(&mut rng).unwrap();

            if let Ok(edge) = self.db.get_edge(*chosen_edge_id) {
                current_node = edge.target;
                path.push(current_node);
            } else {
                // If edge resolution fails, stop the walk
                break;
            }
        }

        Ok(path)
    }
}

#[cfg(all(test, feature = "semantic-reasoning"))]
mod tests {
    use super::*;
    use crate::WriteOps;

    #[test]
    fn test_labyrinth_random_walk() {
        let db = AletheiaDB::new().unwrap();

        let mut node_a = NodeId::new(1).unwrap();
        let mut node_b = NodeId::new(2).unwrap();
        let mut node_c = NodeId::new(3).unwrap();

        db.write(|tx| {
            node_a = tx.create_node("Node", Default::default()).unwrap();
            node_b = tx.create_node("Node", Default::default()).unwrap();
            node_c = tx.create_node("Node", Default::default()).unwrap();

            // A -> B -> C -> A (cycle)
            tx.create_edge(node_a, node_b, "NEXT", Default::default())
                .unwrap();
            tx.create_edge(node_b, node_c, "NEXT", Default::default())
                .unwrap();
            tx.create_edge(node_c, node_a, "NEXT", Default::default())
                .unwrap();
            Ok::<(), crate::core::error::Error>(())
        })
        .unwrap();

        let labyrinth = Labyrinth::new(&db);
        let path = labyrinth.walk(node_a, 5).unwrap();

        assert_eq!(path.len(), 6, "Path should include start node + 5 steps");
        assert_eq!(path[0], node_a);

        // Because it's a deterministic cycle (out-degree 1 for each node),
        // we can predict the exact path: A -> B -> C -> A -> B -> C
        assert_eq!(path[1], node_b);
        assert_eq!(path[2], node_c);
        assert_eq!(path[3], node_a);
        assert_eq!(path[4], node_b);
        assert_eq!(path[5], node_c);
    }

    #[test]
    fn test_labyrinth_dead_end() {
        let db = AletheiaDB::new().unwrap();

        let mut node_a = NodeId::new(1).unwrap();
        let mut node_b = NodeId::new(2).unwrap();

        db.write(|tx| {
            node_a = tx.create_node("Node", Default::default()).unwrap();
            node_b = tx.create_node("Node", Default::default()).unwrap();

            // A -> B, and B has no outgoing edges
            tx.create_edge(node_a, node_b, "NEXT", Default::default())
                .unwrap();
            Ok::<(), crate::core::error::Error>(())
        })
        .unwrap();

        let labyrinth = Labyrinth::new(&db);
        let path = labyrinth.walk(node_a, 5).unwrap();

        assert_eq!(path.len(), 2, "Path should stop at the dead end");
        assert_eq!(path[0], node_a);
        assert_eq!(path[1], node_b);
    }
}
