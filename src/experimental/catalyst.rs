//! Catalyst: Semantic Chain Reaction Engine.
//!
//! "How fast does an idea spread?"
//!
//! Catalyst models the propagation of a semantic shift through a graph.
//! When a node is "ignited" with a new concept (a vector), it can infect its
//! structural neighbors if they are semantically susceptible (i.e., their
//! current vector is similar enough to the incoming concept).
//!
//! # Use Cases
//! - **Memetic Propagation**: Tracing how a new piece of information alters a network.
//! - **Context Realignment**: Automatically shifting neighboring nodes when a core assumption changes.
//! - **Cascade Effect Modeling**: Simulating "what-if" scenarios of localized conceptual shifts.
//!
//! # Example
//! ```rust,no_run
//! use aletheiadb::AletheiaDB;
//! use aletheiadb::experimental::catalyst::CatalystEngine;
//! use aletheiadb::core::id::NodeId;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let db = AletheiaDB::new()?;
//! let engine = CatalystEngine::new(&db);
//!
//! # let start_node = NodeId::new(1).unwrap();
//! let catalyst_vector = vec![1.0, 0.0, 0.5]; // The new idea
//! let spread_threshold = 0.8; // High similarity required to catch fire
//! let max_depth = 3;
//!
//! // Ignite the reaction
//! let report = engine.ignite_reaction(
//!     start_node,
//!     &catalyst_vector,
//!     "embedding",
//!     spread_threshold,
//!     max_depth,
//! )?;
//!
//! println!("Reaction reached depth {} and affected {} nodes.", report.max_depth_reached, report.affected_nodes.len());
//! # Ok(())
//! # }
//! ```

use crate::AletheiaDB;
use crate::api::transaction::{ReadOps, WriteOps};
use crate::core::error::{Error, Result, VectorError};
use crate::core::id::NodeId;
use crate::core::vector::ops::cosine_similarity;
use std::collections::{HashSet, VecDeque};

/// A report of the chain reaction.
#[derive(Debug, Clone)]
pub struct ReactionReport {
    /// Nodes that were infected/altered by the catalyst.
    pub affected_nodes: Vec<NodeId>,
    /// The maximum structural depth reached by the reaction.
    pub max_depth_reached: usize,
}

/// The Catalyst Engine for semantic chain reactions.
pub struct CatalystEngine<'a> {
    db: &'a AletheiaDB,
}

impl<'a> CatalystEngine<'a> {
    /// Create a new CatalystEngine.
    pub fn new(db: &'a AletheiaDB) -> Self {
        Self { db }
    }

    /// Ignite a semantic chain reaction starting from a specific node.
    ///
    /// # Arguments
    /// * `start_node` - The structural epicenter of the reaction.
    /// * `catalyst_vector` - The new semantic vector to propagate.
    /// * `property_name` - The vector property to analyze and update.
    /// * `spread_threshold` - The minimum cosine similarity required for a neighbor to be infected (e.g., 0.8).
    /// * `max_depth` - Maximum hops from the start node.
    pub fn ignite_reaction(
        &self,
        start_node: NodeId,
        catalyst_vector: &[f32],
        property_name: &str,
        spread_threshold: f32,
        max_depth: usize,
    ) -> Result<ReactionReport> {
        let mut affected_nodes = Vec::new();
        let mut max_depth_reached = 0;
        let mut visited = HashSet::new();
        let mut queue = VecDeque::new();

        // The start node is infected automatically
        queue.push_back((start_node, 0));
        visited.insert(start_node);

        // We use a single write transaction to ensure atomic propagation
        self.db.write(|tx| {
            while let Some((current_node, depth)) = queue.pop_front() {
                if depth > max_depth {
                    continue;
                }

                if depth > max_depth_reached {
                    max_depth_reached = depth;
                }

                // If not the start node, check susceptibility
                if current_node != start_node {
                    let node = tx.get_node(current_node)?;
                    let val = node.properties.get(property_name).ok_or_else(|| {
                        Error::Vector(VectorError::IndexError(format!(
                            "Property '{}' missing on node {}",
                            property_name, current_node
                        )))
                    })?;

                    let current_vec = val.as_vector().ok_or_else(|| {
                        Error::Vector(VectorError::IndexError(format!(
                            "Property '{}' is not a vector on node {}",
                            property_name, current_node
                        )))
                    })?;

                    if current_vec.len() != catalyst_vector.len() {
                        continue; // Dimension mismatch, skip
                    }

                    let sim = cosine_similarity(current_vec, catalyst_vector)?;

                    if sim >= spread_threshold {
                        // Node catches fire!
                        // Interpolate 50% towards the catalyst (Alpha = 0.5)
                        let new_vec: Vec<f32> = current_vec
                            .iter()
                            .zip(catalyst_vector.iter())
                            .map(|(c, cat)| *c * 0.5 + *cat * 0.5)
                            .collect();

                        // Preserve existing properties
                        let props = node.properties.builder()
                            .insert_vector(property_name, &new_vec)
                            .build();

                        tx.update_node(current_node, props)?;
                        affected_nodes.push(current_node);
                    } else {
                        // Not susceptible, reaction stops at this branch
                        continue;
                    }
                } else {
                    // Force update the start node to the exact catalyst vector
                    let node = tx.get_node(start_node)?;
                    let props = node.properties.builder()
                        .insert_vector(property_name, catalyst_vector)
                        .build();
                    tx.update_node(start_node, props)?;
                    affected_nodes.push(start_node);
                }

                // If we haven't reached max depth, propagate to neighbors
                if depth < max_depth {
                    // Outgoing edges
                    let out_edges = tx.get_outgoing_edges(current_node);
                    for edge_id in out_edges {
                        #[allow(clippy::collapsible_if)]
                        if let Ok(edge) = tx.get_edge(edge_id) {
                            if visited.insert(edge.target) {
                                queue.push_back((edge.target, depth + 1));
                            }
                        }
                    }

                    // Incoming edges (chain reaction flows both ways structurally)
                    let in_edges = tx.get_incoming_edges(current_node);
                    for edge_id in in_edges {
                        #[allow(clippy::collapsible_if)]
                        if let Ok(edge) = tx.get_edge(edge_id) {
                            if visited.insert(edge.source) {
                                queue.push_back((edge.source, depth + 1));
                            }
                        }
                    }
                }
            }
            Ok::<(), Error>(())
        })?;

        Ok(ReactionReport {
            affected_nodes,
            max_depth_reached,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::property::PropertyMapBuilder;
    use crate::index::vector::{DistanceMetric, HnswConfig};

    #[test]
    fn test_catalyst_chain_reaction() {
        let db = AletheiaDB::new().unwrap();
        db.enable_vector_index("vec", HnswConfig::new(2, DistanceMetric::Cosine))
            .unwrap();

        // Node A: [1.0, 0.0] (Start Node)
        let a = db
            .write(|tx| {
                tx.create_node(
                    "Node",
                    PropertyMapBuilder::new()
                        .insert_vector("vec", &[1.0, 0.0])
                        .build(),
                )
            })
            .unwrap();

        // Node B: [0.9, 0.1] (Highly susceptible, sim > 0.9)
        let b = db
            .write(|tx| {
                tx.create_node(
                    "Node",
                    PropertyMapBuilder::new()
                        .insert_vector("vec", &[0.9, 0.1])
                        .build(),
                )
            })
            .unwrap();

        // Node C: [0.0, 1.0] (Not susceptible, sim ~ 0.0)
        let c = db
            .write(|tx| {
                tx.create_node(
                    "Node",
                    PropertyMapBuilder::new()
                        .insert_vector("vec", &[0.0, 1.0])
                        .build(),
                )
            })
            .unwrap();

        // Node D: [0.8, 0.2] (Highly susceptible)
        let d = db
            .write(|tx| {
                tx.create_node(
                    "Node",
                    PropertyMapBuilder::new()
                        .insert_vector("vec", &[0.8, 0.2])
                        .build(),
                )
            })
            .unwrap();

        // Graph: A -> B -> D
        //        A -> C
        db.write(|tx| {
            tx.create_edge(a, b, "LINK", PropertyMapBuilder::new().build())
                .unwrap();
            tx.create_edge(a, c, "LINK", PropertyMapBuilder::new().build())
                .unwrap();
            tx.create_edge(b, d, "LINK", PropertyMapBuilder::new().build())
                .unwrap();
            Ok::<(), Error>(())
        })
        .unwrap();

        let engine = CatalystEngine::new(&db);

        // Ignite A with [1.0, 0.0] (same as A, but we test propagation)
        let catalyst = vec![1.0, 0.0];
        let threshold = 0.8;

        let report = engine
            .ignite_reaction(a, &catalyst, "vec", threshold, 3)
            .unwrap();

        // A is always affected.
        // B is structurally connected to A and sim([1,0], [0.9,0.1]) > 0.8. Caught fire!
        // D is connected to B and sim([1,0], [0.8,0.2]) > 0.8. Caught fire!
        // C is connected to A but sim([1,0], [0,1]) = 0.0 < 0.8. Immune!

        assert_eq!(report.affected_nodes.len(), 3);
        assert!(report.affected_nodes.contains(&a));
        assert!(report.affected_nodes.contains(&b));
        assert!(report.affected_nodes.contains(&d));
        assert!(!report.affected_nodes.contains(&c));
        assert_eq!(report.max_depth_reached, 2); // A(0) -> B(1) -> D(2)

        // Verify B's vector was updated (Lerp 50% between [0.9,0.1] and [1.0,0.0])
        // Expected B: [0.95, 0.05]
        let b_node = db.get_node(b).unwrap();
        let b_vec = b_node.get_property("vec").unwrap().as_vector().unwrap();
        assert!((b_vec[0] - 0.95).abs() < 0.01);
        assert!((b_vec[1] - 0.05).abs() < 0.01);
    }
}
