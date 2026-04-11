#![allow(clippy::collapsible_if)]

//! Horizon: Semantic Event Horizon.
//!
//! "Where does a concept end?"
//!
//! The Horizon Engine maps the "Semantic Event Horizon" of a seed node.
//! It explores the graph outward using Breadth-First Search (BFS) and stops traversing
//! along a path as soon as the semantic similarity between the seed node and the current
//! node drops below a specified threshold.
//!
//! The nodes just outside this similarity bubble form the "Event Horizon",
//! clearly defining the boundary where a concept fundamentally shifts into something else.
//!
//! # Use Cases
//! - **Concept Boundaries**: Finding where discussions about "Machine Learning" turn into "General AI".
//! - **Topic Segmentation**: Understanding the hard edges of a cluster or community.
//! - **Semantic Reach**: Measuring how far an idea can travel before losing its meaning.
//!
//! # Example
//! ```rust,no_run
//! // Requires features = ["nova"]
//! use aletheiadb::AletheiaDB;
//! use aletheiadb::experimental::horizon::HorizonEngine;
//! use aletheiadb::core::id::NodeId;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let db = AletheiaDB::new()?;
//! # let node_id = db.create_node("Concept", Default::default())?;
//! let horizon = HorizonEngine::new(&db);
//!
//! // Find where the similarity drops below 0.6. Max 5 hops.
//! let result = horizon.map_horizon(node_id, "embedding", 0.6, 5)?;
//!
//! println!("Interior nodes: {}", result.interior.len());
//! println!("Horizon nodes (boundary): {}", result.horizon.len());
//! # Ok(())
//! # }
//! ```

use crate::AletheiaDB;
use crate::api::transaction::ReadOps;
use crate::core::error::{Error, Result};
use crate::core::id::NodeId;
use crate::core::vector::ops::cosine_similarity;
use std::collections::{HashSet, VecDeque};

/// The result of mapping the Semantic Event Horizon.
#[derive(Debug, Clone, PartialEq)]
pub struct HorizonResult {
    /// Nodes that are inside the semantic bubble (similarity >= threshold).
    pub interior: HashSet<NodeId>,
    /// Nodes that represent the boundary (similarity < threshold), where exploration stopped.
    pub horizon: HashSet<NodeId>,
}

/// The Horizon Engine.
pub struct HorizonEngine<'a> {
    db: &'a AletheiaDB,
}

impl<'a> HorizonEngine<'a> {
    /// Create a new Horizon Engine.
    pub fn new(db: &'a AletheiaDB) -> Self {
        Self { db }
    }

    /// Map the semantic event horizon of a seed node.
    ///
    /// # Arguments
    /// * `seed` - The starting node.
    /// * `property_name` - The vector property to use for semantic comparison.
    /// * `threshold` - The similarity threshold (e.g., 0.6). Nodes below this form the horizon.
    /// * `max_depth` - The maximum number of hops to explore.
    pub fn map_horizon(
        &self,
        seed: NodeId,
        property_name: &str,
        threshold: f32,
        max_depth: usize,
    ) -> Result<HorizonResult> {
        let mut interior = HashSet::new();
        let mut horizon = HashSet::new();
        let mut visited = HashSet::new();
        let mut queue = VecDeque::new();

        self.db.read(|tx| {
            // Get seed vector
            let seed_node = tx.get_node(seed)?;
            let seed_vec = match seed_node
                .get_property(property_name)
                .and_then(|p| p.as_vector())
            {
                Some(v) => v,
                None => {
                    return Err(Error::other(format!(
                        "Seed node {} does not have vector property '{}'",
                        seed, property_name
                    )));
                }
            };

            // Start BFS
            queue.push_back((seed, 0));
            visited.insert(seed);
            interior.insert(seed); // Seed is always in the interior

            while let Some((current_node_id, depth)) = queue.pop_front() {
                if depth >= max_depth {
                    continue;
                }

                // Get all neighbors (both outgoing and incoming for true semantic reach)
                let outgoing = tx
                    .get_outgoing_edges(current_node_id)
                    .into_iter()
                    .filter_map(|e| tx.get_edge(e).ok().map(|edge| edge.target));

                let incoming = tx
                    .get_incoming_edges(current_node_id)
                    .into_iter()
                    .filter_map(|e| tx.get_edge(e).ok().map(|edge| edge.source));

                // Bolt Optimization: Chain incoming and outgoing iterators directly
                // to eliminate two intermediate Vec allocations per node during BFS.
                for neighbor_id in outgoing.chain(incoming) {
                    if visited.contains(&neighbor_id) {
                        continue;
                    }

                    visited.insert(neighbor_id);

                    if let Ok(neighbor_node) = tx.get_node(neighbor_id) {
                        if let Some(neighbor_vec) = neighbor_node
                            .get_property(property_name)
                            .and_then(|p| p.as_vector())
                        {
                            // Calculate similarity with the *seed* node
                            let sim = cosine_similarity(seed_vec, neighbor_vec)?;

                            if sim >= threshold {
                                // Still inside the bubble
                                interior.insert(neighbor_id);
                                queue.push_back((neighbor_id, depth + 1));
                            } else {
                                // Crossed the event horizon
                                horizon.insert(neighbor_id);
                            }
                        }
                    }
                }
            }

            Ok::<(), Error>(())
        })?;

        Ok(HorizonResult { interior, horizon })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::transaction::WriteOps;
    use crate::core::property::PropertyMapBuilder;

    #[test]
    fn test_horizon_mapping() {
        let db = AletheiaDB::new().unwrap();

        let mut a = NodeId::new(0).unwrap();
        let mut b = NodeId::new(0).unwrap();
        let mut c = NodeId::new(0).unwrap();
        let mut d = NodeId::new(0).unwrap();
        let mut e = NodeId::new(0).unwrap();

        db.write(|tx| {
            // A (seed): Core Concept
            a = tx
                .create_node(
                    "Concept",
                    PropertyMapBuilder::new()
                        .insert_vector("vec", &[1.0, 0.0, 0.0])
                        .build(),
                )
                .unwrap();

            // B: Very similar to A (cosine sim = 0.9)
            // dot product = 0.9, norm = 1.0 * 1.0
            // x * 1.0 = 0.9 => x = 0.9
            // y^2 + z^2 = 1 - 0.9^2 = 1 - 0.81 = 0.19
            // let y = sqrt(0.19) ~ 0.4358
            b = tx
                .create_node(
                    "Concept",
                    PropertyMapBuilder::new()
                        .insert_vector("vec", &[0.9, 0.435889, 0.0])
                        .build(),
                )
                .unwrap();

            // C: Somewhat similar to A (cosine sim = 0.6)
            // dot product = 0.6 => x = 0.6
            // y = sqrt(1 - 0.36) = 0.8
            c = tx
                .create_node(
                    "Concept",
                    PropertyMapBuilder::new()
                        .insert_vector("vec", &[0.6, 0.8, 0.0])
                        .build(),
                )
                .unwrap();

            // D: Different from A (cosine sim = 0.2)
            // x = 0.2
            // y = sqrt(1 - 0.04) = 0.9797
            d = tx
                .create_node(
                    "Concept",
                    PropertyMapBuilder::new()
                        .insert_vector("vec", &[0.2, 0.979795, 0.0])
                        .build(),
                )
                .unwrap();

            // E: Completely different (cosine sim = 0.0)
            e = tx
                .create_node(
                    "Concept",
                    PropertyMapBuilder::new()
                        .insert_vector("vec", &[0.0, 1.0, 0.0])
                        .build(),
                )
                .unwrap();

            // Graph structure: A -> B -> C -> D -> E
            tx.create_edge(a, b, "LINK", Default::default()).unwrap();
            tx.create_edge(b, c, "LINK", Default::default()).unwrap();
            tx.create_edge(c, d, "LINK", Default::default()).unwrap();
            tx.create_edge(d, e, "LINK", Default::default()).unwrap();

            Ok::<(), Error>(())
        })
        .unwrap();

        let engine = HorizonEngine::new(&db);

        // Threshold = 0.5.
        // A=1.0, B=0.9, C=0.6 are inside.
        // D=0.2 is the boundary (horizon). E is never reached because traversal stops at D.
        let result = engine.map_horizon(a, "vec", 0.5, 10).unwrap();

        assert!(result.interior.contains(&a));
        assert!(result.interior.contains(&b));
        assert!(result.interior.contains(&c));
        assert!(!result.interior.contains(&d));
        assert!(!result.interior.contains(&e));

        assert!(result.horizon.contains(&d));
        assert!(!result.horizon.contains(&e)); // Not reached

        // Threshold = 0.8
        // A=1.0, B=0.9 are inside.
        // C=0.6 is the boundary. D and E are not reached.
        let result2 = engine.map_horizon(a, "vec", 0.8, 10).unwrap();
        assert!(result2.interior.contains(&a));
        assert!(result2.interior.contains(&b));
        assert!(result2.horizon.contains(&c));
        assert!(!result2.horizon.contains(&d));
    }
}
