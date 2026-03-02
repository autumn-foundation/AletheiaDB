//! Luna: Semantic Subgraph Synthesis.
//!
//! "What holds these ideas together?"
//!
//! Luna explores a set of connected or related concepts (seeds), computes their
//! semantic "center of gravity" (centroid), and dynamically instantiates a new
//! node representing that core concept. It then automatically connects this new
//! core node back to the seeds, thereby structurally materializing a previously
//! implicit relationship.
//!
//! # Concepts
//! - **Synthesis**: The creation of a new "Core" node from the semantic average of seeds.
//! - **Materialization**: Automatically adding structural edges (`CORE_OF`) from the
//!   new node to the original seed nodes.

#![allow(clippy::collapsible_if)]

use crate::AletheiaDB;
use crate::api::transaction::WriteOps;
use crate::core::error::{Error, Result, VectorError};
use crate::core::id::NodeId;
use crate::core::property::PropertyMapBuilder;

/// The Luna Engine for Semantic Synthesis.
pub struct Luna<'a> {
    db: &'a AletheiaDB,
}

#[cfg(feature = "nova")]
impl<'a> Luna<'a> {
    /// Create a new Luna instance.
    pub fn new(db: &'a AletheiaDB) -> Self {
        Self { db }
    }

    /// Synthesize a core concept from a set of seed nodes.
    ///
    /// # Arguments
    /// * `seeds` - The source nodes to synthesize.
    /// * `property` - The vector property to use for calculation and assignment.
    /// * `label` - The label for the newly created core node.
    ///
    /// # Returns
    /// The `NodeId` of the newly synthesized core node.
    pub fn synthesize_core(&self, seeds: &[NodeId], property: &str, label: &str) -> Result<NodeId> {
        if seeds.is_empty() {
            return Err(Error::other(
                "Cannot synthesize from an empty set of seeds.",
            ));
        }

        // 1. Fetch Vectors from seeds
        let mut vectors = Vec::with_capacity(seeds.len());
        for &node_id in seeds {
            let node = self.db.get_node(node_id)?;
            if let Some(val) = node.properties.get(property) {
                if let Some(vec) = val.as_vector() {
                    vectors.push(vec.to_vec());
                }
            }
        }

        if vectors.is_empty() {
            return Err(Error::Vector(VectorError::IndexError(format!(
                "No vectors found in property '{}' for the given seeds.",
                property
            ))));
        }

        // 2. Calculate Centroid
        let dim = vectors[0].len();
        let mut centroid = vec![0.0; dim];
        for vec in &vectors {
            if vec.len() != dim {
                return Err(Error::Vector(VectorError::DimensionMismatch {
                    expected: dim,
                    actual: vec.len(),
                }));
            }
            for (i, val) in vec.iter().enumerate() {
                centroid[i] += val;
            }
        }

        // Normalize the centroid to unit length for directional semantics (Cosine)
        let centroid = crate::core::vector::ops::normalize(&centroid);

        // 3. Materialize: Create new node and edges
        let new_node_id = self.db.write(|tx| {
            // Build properties for the new core node
            let props = PropertyMapBuilder::new()
                .insert_vector(property, &centroid)
                .build();

            // Create the Core Node
            let core_node_id = tx.create_node(label, props)?;

            // Create edges from the Core Node to the Seeds
            let edge_props = PropertyMapBuilder::new().build();
            for &seed_id in seeds {
                // Ensure we don't try to link to a seed that failed to provide a vector?
                // Actually, linking to all requested seeds makes structural sense even if they lacked the vector.
                tx.create_edge(core_node_id, seed_id, "CORE_OF", edge_props.clone())?;
            }

            Ok::<NodeId, Error>(core_node_id)
        })?;

        Ok(new_node_id)
    }
}

#[cfg(all(test, feature = "nova"))]
mod tests {
    use super::*;
    use crate::index::vector::{DistanceMetric, HnswConfig};

    #[test]
    fn test_luna_synthesize_core() {
        let db = AletheiaDB::new().unwrap();
        let config = HnswConfig::new(2, DistanceMetric::Cosine);
        db.enable_vector_index("vec", config).unwrap();

        // Node A: [1, 0]
        let props_a = PropertyMapBuilder::new()
            .insert_vector("vec", &[1.0, 0.0])
            .build();
        let a = db.create_node("Concept", props_a).unwrap();

        // Node B: [0, 1]
        let props_b = PropertyMapBuilder::new()
            .insert_vector("vec", &[0.0, 1.0])
            .build();
        let b = db.create_node("Concept", props_b).unwrap();

        let luna = Luna::new(&db);
        let core_id = luna.synthesize_core(&[a, b], "vec", "CoreConcept").unwrap();

        // Check that the new node was created and has the correct label
        let core_node = db.get_node(core_id).unwrap();
        let label_str = crate::core::GLOBAL_INTERNER
            .resolve_with(core_node.label, |s| s.to_string())
            .unwrap();
        assert_eq!(label_str, "CoreConcept");

        // Check the synthesized vector (Normalized [0.5, 0.5] -> [0.707, 0.707])
        let vec_val = core_node.properties.get("vec").unwrap();
        let vec = vec_val.as_vector().unwrap();
        assert!((vec[0] - std::f32::consts::FRAC_1_SQRT_2).abs() < 0.01);
        assert!((vec[1] - std::f32::consts::FRAC_1_SQRT_2).abs() < 0.01);

        // Verify the structural materialization: Edges from Core -> Seeds
        let out_edges = db.get_outgoing_edges(core_id);
        assert_eq!(out_edges.len(), 2, "Core node should have 2 outgoing edges");

        let mut targets = std::collections::HashSet::new();
        for edge_id in out_edges {
            let edge = db.get_edge(edge_id).unwrap();
            let edge_label_str = crate::core::GLOBAL_INTERNER
                .resolve_with(edge.label, |s| s.to_string())
                .unwrap();
            assert_eq!(edge_label_str, "CORE_OF");
            targets.insert(edge.target);
        }

        assert!(targets.contains(&a));
        assert!(targets.contains(&b));
    }
}
