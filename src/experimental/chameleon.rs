//! Chameleon: Context-Adaptive Vector Search.
//!
//! "Context is King." 🦎
//!
//! Chameleon adjusts a node's vector representation at query time based on its
//! neighbors in the graph. It solves the problem of polysemy (words with multiple meanings)
//! by using the local graph topology to disambiguate meaning.
//!
//! # The Hook
//!
//! A "Bank" connected to "River" means something different than a "Bank" connected to "Gold".
//! Chameleon dynamically shifts the vector of "Bank" towards its context before searching.
//!
//! # How it works
//!
//! 1. **Fetch Target**: Get the vector of the node you want to use as a query.
//! 2. **Fetch Context**: Get the vectors of neighbors connected by specific edge types.
//! 3. **Adapt**: Compute a weighted centroid of the Target + Context vectors.
//! 4. **Search**: Use the new, contextualized vector to find similar nodes.
//!
//! # Example
//!
//! ```rust
//! // Requires features = ["nova"]
//! use aletheiadb::AletheiaDB;
//! use aletheiadb::experimental::chameleon::Chameleon;
//! use aletheiadb::core::id::NodeId;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let db = AletheiaDB::new()?;
//! // ... assume db is populated ...
//!
//! let chameleon = Chameleon::new(&db);
//! let bank_node = NodeId::new(1).unwrap();
//!
//! // Contextualize "Bank" using neighbors connected via "IS_NEAR" edges
//! let results = chameleon.search_with_context(
//!     bank_node,
//!     &["IS_NEAR"], // Context edge types
//!     "embedding",  // Vector property
//!     5             // Top K
//! )?;
//!
//! for (node_id, score) in results {
//!     println!("Found semantic match: {} (score: {})", node_id, score);
//! }
//! # Ok(())
//! # }
//! ```

use crate::AletheiaDB;
use crate::core::id::NodeId;
use crate::core::interning::GLOBAL_INTERNER;
use crate::core::vector::ops::magnitude;
use crate::utils::{Error, Result, VectorError};
use std::collections::HashSet;

/// The Chameleon engine for context-adaptive vector search.
pub struct Chameleon<'a> {
    db: &'a AletheiaDB,
}

impl<'a> Chameleon<'a> {
    /// Create a new Chameleon instance.
    pub fn new(db: &'a AletheiaDB) -> Self {
        Self { db }
    }

    /// Adapt a node's vector based on its context neighbors.
    ///
    /// This function:
    /// 1. Fetches the vector for `node_id`.
    /// 2. Finds all outgoing edges with types in `context_edge_types`.
    /// 3. Fetches the vectors of the target nodes of those edges.
    /// 4. Computes the centroid (mean) of the node vector and all context vectors.
    /// 5. Returns the normalized centroid vector.
    ///
    /// If no context neighbors are found (or they lack vectors), the original node vector is returned.
    pub fn adapt(
        &self,
        node_id: NodeId,
        context_edge_types: &[&str],
        property: &str,
    ) -> Result<Vec<f32>> {
        // 1. Fetch Target Vector
        let target_node = self.db.get_node(node_id)?;
        let target_vec_val = target_node.properties.get(property).ok_or_else(|| {
            Error::Vector(VectorError::IndexError(format!(
                "Node {} missing vector property '{}'",
                node_id, property
            )))
        })?;

        let target_vec = target_vec_val.as_vector().ok_or_else(|| {
            Error::Vector(VectorError::IndexError(format!(
                "Property '{}' on node {} is not a vector",
                property, node_id
            )))
        })?;

        let dim = target_vec.len();
        let mut sum_vec = target_vec.to_vec();
        let mut count = 1.0;

        // Pre-resolve context types to IDs for fast comparison.
        // If a context type isn't interned, it means no edge uses it yet (or at least not via global interner),
        // so we can safely ignore it.
        let context_ids: HashSet<u32> = context_edge_types
            .iter()
            .filter_map(|&s| GLOBAL_INTERNER.get_id(s).map(|id| id.as_u32()))
            .collect();

        // 2. Fetch Context Vectors
        // We iterate over edges and filter by type
        let edges = self.db.get_outgoing_edges(node_id);
        for edge_id in edges {
            let edge = self.db.get_edge(edge_id)?;

            // Check if edge type matches any of the requested context types
            if context_ids.contains(&edge.label.as_u32()) {
                let neighbor_id = edge.target;
                let neighbor = self.db.get_node(neighbor_id)?;

                if let Some(vec) = neighbor
                    .properties
                    .get(property)
                    .and_then(|val| val.as_vector())
                    .filter(|v| v.len() == dim)
                {
                    // Add to sum
                    for (s, v) in sum_vec.iter_mut().zip(vec.iter()) {
                        *s += v;
                    }
                    count += 1.0;
                }
            }
        }

        // 3. Compute Centroid & Normalize
        if count > 1.0 {
            for s in &mut sum_vec {
                *s /= count;
            }

            // 4. Normalize
            // Centroids can shrink; we want direction, so we normalize.
            let mag = magnitude(&sum_vec);
            if mag > 1e-6 {
                // Optimize: Normalize in-place to avoid re-calculating magnitude and allocation.
                let inv_mag = 1.0 / mag;
                for s in &mut sum_vec {
                    *s *= inv_mag;
                }
            }
            // If magnitude is zero (unlikely unless vectors cancel out perfectly),
            // return zero vector.
            Ok(sum_vec)
        } else {
            // No context applied. Return original vector as-is.
            Ok(target_vec.to_vec())
        }
    }

    /// Search for nodes similar to the *contextualized* version of the input node.
    pub fn search_with_context(
        &self,
        node_id: NodeId,
        context_edge_types: &[&str],
        property: &str,
        k: usize,
    ) -> Result<Vec<(NodeId, f32)>> {
        let adapted_vec = self.adapt(node_id, context_edge_types, property)?;
        self.db.search_vectors_in(property, &adapted_vec, k)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::property::PropertyMapBuilder;
    use crate::index::vector::{DistanceMetric, HnswConfig};

    // Helper to setup DB with vector index
    fn setup_db() -> AletheiaDB {
        let db = AletheiaDB::new().unwrap();
        let config = HnswConfig::new(2, DistanceMetric::Cosine);
        db.enable_vector_index("embedding", config).unwrap();
        db
    }

    #[test]
    fn test_chameleon_polysemy_resolution() {
        let db = setup_db();

        // Scenario: "Bank" Polysemy
        // Vectors (2D simplified):
        // [1, 0] = Finance/Money direction
        // [0, 1] = Nature/Water direction
        // [0.7, 0.7] = Ambiguous "Bank" (somewhere in between)

        // 1. Create Nodes
        let bank_props = PropertyMapBuilder::new()
            .insert("name", "Bank")
            .insert_vector("embedding", &[0.707, 0.707])
            .build();
        let bank = db.create_node("Word", bank_props).unwrap();

        let river_props = PropertyMapBuilder::new()
            .insert("name", "River")
            .insert_vector("embedding", &[0.0, 1.0]) // Pure Nature
            .build();
        let river = db.create_node("Word", river_props).unwrap();

        let money_props = PropertyMapBuilder::new()
            .insert("name", "Money")
            .insert_vector("embedding", &[1.0, 0.0]) // Pure Finance
            .build();
        let money = db.create_node("Word", money_props).unwrap();

        // Targets to find
        let nature_target_props = PropertyMapBuilder::new()
            .insert("name", "NatureTarget")
            .insert_vector("embedding", &[0.1, 0.9]) // Mostly Nature
            .build();
        let nature_target = db.create_node("Target", nature_target_props).unwrap();

        let finance_target_props = PropertyMapBuilder::new()
            .insert("name", "FinanceTarget")
            .insert_vector("embedding", &[0.9, 0.1]) // Mostly Finance
            .build();
        let finance_target = db.create_node("Target", finance_target_props).unwrap();

        // 2. Setup Contexts (Edges)
        // Bank -> River (Context: Nature)
        db.create_edge(bank, river, "IS_NEAR", PropertyMapBuilder::new().build())
            .unwrap();

        // Bank -> Money (Context: Finance)
        db.create_edge(bank, money, "RELATED_TO", PropertyMapBuilder::new().build())
            .unwrap();

        let chameleon = Chameleon::new(&db);

        // 3. Search with "IS_NEAR" context (River)
        // Expectation: Result should be closer to NatureTarget
        let results_nature = chameleon
            .search_with_context(bank, &["IS_NEAR"], "embedding", 1)
            .unwrap();

        assert_eq!(
            results_nature[0].0, nature_target,
            "Context 'IS_NEAR' (River) should shift Bank towards Nature"
        );

        // 4. Search with "RELATED_TO" context (Money)
        // Expectation: Result should be closer to FinanceTarget
        let results_finance = chameleon
            .search_with_context(bank, &["RELATED_TO"], "embedding", 1)
            .unwrap();

        assert_eq!(
            results_finance[0].0, finance_target,
            "Context 'RELATED_TO' (Money) should shift Bank towards Finance"
        );
    }

    #[test]
    fn test_chameleon_no_context_fallback() {
        let db = setup_db();

        let props = PropertyMapBuilder::new()
            .insert_vector("embedding", &[1.0, 0.0])
            .build();
        let node = db.create_node("Node", props).unwrap();

        let chameleon = Chameleon::new(&db);

        // Search with non-existent context edge type
        let adapted = chameleon
            .adapt(node, &["NON_EXISTENT"], "embedding")
            .unwrap();

        // Should return original vector
        assert!((adapted[0] - 1.0).abs() < 1e-5);
        assert!((adapted[1] - 0.0).abs() < 1e-5);
    }
}
