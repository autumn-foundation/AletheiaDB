#![allow(clippy::collapsible_if)]

//! Synergy: Identifying Emergent Properties 🌠
//!
//! "Does the whole exceed the sum of its parts?"
//!
//! Synergy explores clusters of nodes to identify "emergent vectors".
//! It analyzes whether a subgraph's collective embedding differs significantly
//! from the simple average of its individual node embeddings.
//!
//! # Concepts
//! - **Emergent Vector**: A vector representing the collective meaning of a subgraph
//!   that accounts for both node semantics and the structural interactions between them.
//! - **Synergy Score**: A measure of how much the emergent vector diverges from the
//!   baseline average vector. High synergy means the structural connections are adding
//!   significant new meaning.
//!
//! # Use Cases
//! - **Team Dynamics**: Finding groups of individuals who form a highly synergistic team.
//! - **Concept Synthesis**: Identifying combinations of concepts that create profound new ideas.
//!
//! # Example
//! ```rust
//! // Requires features = ["nova"]
//! use aletheiadb::AletheiaDB;
//! use aletheiadb::experimental::synergy::Synergy;
//! use aletheiadb::core::id::NodeId;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let db = AletheiaDB::new()?;
//! // ... setup graph ...
//! # let nodes = vec![];
//!
//! let synergy = Synergy::new(&db);
//! let score = synergy.calculate_synergy(&nodes, "embedding")?;
//!
//! println!("Synergy Score: {}", score);
//! # Ok(())
//! # }
//! ```

use crate::AletheiaDB;
use crate::api::transaction::ReadOps;
use crate::core::error::{Error, Result};
use crate::core::id::NodeId;
use crate::core::vector::ops;
use std::collections::HashSet;

/// Result of a synergy analysis.
#[derive(Debug, Clone)]
pub struct SynergyResult {
    /// The baseline average vector of the nodes without considering structure.
    pub baseline_vector: Vec<f32>,
    /// The emergent vector considering the graph structure between the nodes.
    pub emergent_vector: Vec<f32>,
    /// The synergy score (0.0 to 1.0). High score means high emergence.
    /// Calculated as 1.0 - cosine_similarity(baseline, emergent).
    pub synergy_score: f32,
}

/// The Synergy Engine.
pub struct Synergy<'a> {
    db: &'a AletheiaDB,
}

impl<'a> Synergy<'a> {
    /// Create a new Synergy Engine.
    pub fn new(db: &'a AletheiaDB) -> Self {
        Self { db }
    }

    /// Calculate the synergy of a given set of nodes.
    ///
    /// # Arguments
    /// * `nodes` - The list of node IDs forming the subgraph.
    /// * `property_name` - The vector property to use for semantic analysis.
    pub fn calculate_synergy(&self, nodes: &[NodeId], property_name: &str) -> Result<f32> {
        let result = self.analyze(nodes, property_name)?;
        Ok(result.synergy_score)
    }

    /// Perform full synergy analysis on a given set of nodes.
    pub fn analyze(&self, nodes: &[NodeId], property_name: &str) -> Result<SynergyResult> {
        if nodes.is_empty() {
            return Err(Error::other("Cannot analyze empty node list"));
        }

        let node_set: HashSet<NodeId> = nodes.iter().cloned().collect();
        let mut vectors = Vec::new();
        let mut valid_nodes = Vec::new();

        // Use read closure
        self.db.read(|tx| {
            // 1. Gather all vectors for the specified nodes
            for &node_id in nodes {
                if let Ok(node) = tx.get_node(node_id) {
                    if let Some(prop) = node.get_property(property_name).and_then(|p| p.as_vector())
                    {
                        vectors.push(prop.to_vec());
                        valid_nodes.push(node_id);
                    }
                }
            }
            Ok::<(), Error>(())
        })?;

        if vectors.is_empty() {
            return Err(Error::other(
                "None of the provided nodes have the specified vector property",
            ));
        }

        // 2. Calculate the Baseline Vector (simple average)
        let baseline_vector = Self::average_vectors(&vectors)?;

        // 3. Calculate the Emergent Vector
        let mut emergent_components = Vec::new();

        self.db.read(|tx| {
            for (i, &node_id) in valid_nodes.iter().enumerate() {
                let mut neighbor_vectors = Vec::new();

                // Get internal outgoing edges
                let outgoing = tx.get_outgoing_edges(node_id);
                for edge_id in outgoing {
                    if let Ok(edge) = tx.get_edge(edge_id) {
                        let target = edge.target;
                        if node_set.contains(&target) {
                            if let Some(idx) = valid_nodes.iter().position(|&id| id == target) {
                                neighbor_vectors.push(vectors[idx].clone());
                            }
                        }
                    }
                }

                // Get internal incoming edges
                let incoming = tx.get_incoming_edges(node_id);
                for edge_id in incoming {
                    if let Ok(edge) = tx.get_edge(edge_id) {
                        let source = edge.source;
                        if node_set.contains(&source) {
                            if let Some(idx) = valid_nodes.iter().position(|&id| id == source) {
                                neighbor_vectors.push(vectors[idx].clone());
                            }
                        }
                    }
                }

                // Calculate structural influence for this node
                let node_vec = &vectors[i];

                if neighbor_vectors.is_empty() {
                    // No internal connections, use original vector
                    emergent_components.push(node_vec.clone());
                } else {
                    // Average the neighbor vectors
                    let neighbor_avg = Self::average_vectors(&neighbor_vectors)?; // Need to handle error here inside closure

                    // Combine node vector with neighbor influence
                    let alpha = 0.5_f32;
                    let mut combined = vec![0.0; node_vec.len()];
                    for j in 0..node_vec.len() {
                        combined[j] = (1.0 - alpha) * node_vec[j] + alpha * neighbor_avg[j];
                    }
                    emergent_components.push(combined);
                }
            }
            Ok::<(), Error>(())
        })?;

        // The overall emergent vector is the average of these structurally-influenced vectors
        let mut emergent_vector = Self::average_vectors(&emergent_components)?;

        // Normalize both vectors for comparison
        ops::normalize_in_place(&mut emergent_vector);

        let mut baseline_normalized = baseline_vector.clone();
        ops::normalize_in_place(&mut baseline_normalized);

        // 4. Calculate Synergy Score
        let similarity = ops::cosine_similarity(&baseline_normalized, &emergent_vector)?;

        // Score is the divergence from the baseline
        let synergy_score = (1.0_f32 - similarity).max(0.0);

        Ok(SynergyResult {
            baseline_vector: baseline_normalized,
            emergent_vector,
            synergy_score,
        })
    }

    /// Helper to average a list of vectors.
    fn average_vectors(vectors: &[Vec<f32>]) -> Result<Vec<f32>> {
        if vectors.is_empty() {
            return Err(Error::other("Cannot average empty vector list"));
        }

        let dim = vectors[0].len();
        let mut sum = vec![0.0; dim];

        for vec in vectors {
            if vec.len() != dim {
                return Err(Error::other("Vector dimensions do not match"));
            }
            for i in 0..dim {
                sum[i] += vec[i];
            }
        }

        let count = vectors.len() as f32;
        for val in &mut sum {
            *val /= count;
        }

        Ok(sum)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::transaction::WriteOps;
    use crate::core::property::PropertyMapBuilder;

    #[test]
    fn test_synergy_disconnected_nodes() {
        let db = AletheiaDB::new().unwrap();

        let mut n1 = NodeId::new(0).unwrap();
        let mut n2 = NodeId::new(0).unwrap();

        db.write(|tx| {
            n1 = tx
                .create_node(
                    "Node",
                    PropertyMapBuilder::new()
                        .insert_vector("embedding", &[1.0, 0.0])
                        .build(),
                )
                .unwrap();

            n2 = tx
                .create_node(
                    "Node",
                    PropertyMapBuilder::new()
                        .insert_vector("embedding", &[0.0, 1.0])
                        .build(),
                )
                .unwrap();
            Ok::<(), Error>(())
        })
        .unwrap();

        let synergy = Synergy::new(&db);
        let result = synergy.analyze(&[n1, n2], "embedding").unwrap();

        // Disconnected nodes should have 0 synergy (emergent == baseline)
        assert!(result.synergy_score < 0.001);
    }

    #[test]
    fn test_synergy_connected_nodes() {
        let db = AletheiaDB::new().unwrap();

        let mut n1 = NodeId::new(0).unwrap();
        let mut n2 = NodeId::new(0).unwrap();
        let mut n3 = NodeId::new(0).unwrap();

        db.write(|tx| {
            n1 = tx
                .create_node(
                    "Node",
                    PropertyMapBuilder::new()
                        .insert_vector("embedding", &[1.0, 0.0])
                        .build(),
                )
                .unwrap();

            n2 = tx
                .create_node(
                    "Node",
                    PropertyMapBuilder::new()
                        .insert_vector("embedding", &[0.0, 1.0])
                        .build(),
                )
                .unwrap();

            n3 = tx
                .create_node(
                    "Node",
                    PropertyMapBuilder::new()
                        .insert_vector("embedding", &[0.5, 0.5])
                        .build(),
                )
                .unwrap();

            // Connect them to create structure
            tx.create_edge(n1, n2, "LINK", Default::default()).unwrap();
            tx.create_edge(n2, n3, "LINK", Default::default()).unwrap();

            Ok::<(), Error>(())
        })
        .unwrap();

        let synergy = Synergy::new(&db);
        let result = synergy.analyze(&[n1, n2, n3], "embedding").unwrap();

        // Connected nodes with different vectors should exhibit some synergy
        assert!(result.synergy_score > 0.0);
    }
}
