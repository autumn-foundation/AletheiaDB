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
        let mut baseline_vector = Vec::new();
        let mut emergent_components = Vec::new();

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

            if vectors.is_empty() {
                return Ok::<(), Error>(());
            }

            // 2. Calculate Baseline Vector (simple average)
            let dim = vectors[0].len();
            baseline_vector = vec![0.0; dim];
            for v in &vectors {
                for i in 0..dim {
                    baseline_vector[i] += v[i];
                }
            }
            for v in &mut baseline_vector {
                *v /= vectors.len() as f32;
            }

            // Normalize baseline
            baseline_vector = ops::normalize(&baseline_vector);

            // 3. Calculate Emergent Vector
            // Start with baseline
            emergent_components = baseline_vector.clone();

            // Modulate by structural edges within the group
            for &node_id in &valid_nodes {
                let edge_ids = tx.get_outgoing_edges(node_id);
                for edge_id in edge_ids {
                    if let Ok(edge) = tx.get_edge(edge_id) {
                        if node_set.contains(&edge.target) {
                            if let Ok(target_node) = tx.get_node(edge.target) {
                                if let Some(target_vec) = target_node.get_property(property_name).and_then(|p| p.as_vector()) {
                                    for i in 0..dim {
                                        emergent_components[i] += target_vec[i] * 0.1; // Magic weight factor
                                    }
                                }
                            }
                        }
                    }
                }
            }

            emergent_components = ops::normalize(&emergent_components);
            Ok::<(), Error>(())
        })?;

        if valid_nodes.is_empty() {
            return Err(Error::other("None of the specified nodes had the requested vector property"));
        }

        // 4. Calculate Synergy Score
        let mut similarity = ops::cosine_similarity(&baseline_vector, &emergent_components).unwrap_or(0.0);

        // Ensure bounds due to float inaccuracy
        similarity = similarity.clamp(-1.0, 1.0);

        // High similarity means low synergy (no change).
        // Score = 1.0 - similarity. Max score is 2.0 (if perfectly opposite), typically we clamp to 0..1
        let synergy_score = f32::max(1.0 - similarity, 0.0) / 2.0;

        Ok(SynergyResult {
            baseline_vector,
            emergent_vector: emergent_components,
            synergy_score,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::AletheiaDB;
    use crate::core::property::PropertyMapBuilder;
    use crate::api::transaction::WriteOps;
    use crate::core::vector::ops;

    #[test]
    fn test_synergy_disconnected_nodes() -> Result<()> {
        let db = AletheiaDB::new()?;
        let synergy = Synergy::new(&db);

        let n1 = db.write(|tx| {
            tx.create_node("Concept", PropertyMapBuilder::new().insert_vector("vec", &[1.0, 0.0]).build())
        })?;

        let n2 = db.write(|tx| {
            tx.create_node("Concept", PropertyMapBuilder::new().insert_vector("vec", &[0.0, 1.0]).build())
        })?;

        let result = synergy.analyze(&[n1, n2], "vec")?;

        // Disconnected nodes should have synergy score 0.0
        assert!(result.synergy_score < 1e-5);

        let expected_baseline = ops::normalize(&[0.5, 0.5]);
        assert!((result.baseline_vector[0] - expected_baseline[0]).abs() < 1e-5);
        assert!((result.baseline_vector[1] - expected_baseline[1]).abs() < 1e-5);

        Ok(())
    }

    #[test]
    fn test_synergy_connected_nodes() -> Result<()> {
        let db = AletheiaDB::new()?;
        let synergy = Synergy::new(&db);

        let (n1, n2, n3) = db.write(|tx| {
            let n1 = tx.create_node("Concept", PropertyMapBuilder::new().insert_vector("vec", &[1.0, 0.0, 0.0]).build())?;
            let n2 = tx.create_node("Concept", PropertyMapBuilder::new().insert_vector("vec", &[0.0, 1.0, 0.0]).build())?;
            let n3 = tx.create_node("Concept", PropertyMapBuilder::new().insert_vector("vec", &[0.0, 0.0, 1.0]).build())?;

            // Connect n1 -> n2 -> n3
            tx.create_edge(n1, n2, "RELATED", PropertyMapBuilder::new().build())?;
            tx.create_edge(n2, n3, "RELATED", PropertyMapBuilder::new().build())?;

            Ok::<_, Error>((n1, n2, n3))
        })?;

        let result = synergy.analyze(&[n1, n2, n3], "vec")?;

        // Because of the edges, emergent vector is pulled towards targets,
        // leading to > 0 synergy score
        assert!(result.synergy_score > 0.0);

        Ok(())
    }
}
