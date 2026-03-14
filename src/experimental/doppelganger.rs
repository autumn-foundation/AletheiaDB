#![allow(clippy::collapsible_if)]

//! Doppelganger Engine: Semantic Impostor Detection.
//!
//! "Who are you trying to be?"
//!
//! The Doppelganger Engine identifies nodes that are **structurally identical**
//! (they interact with the exact same neighborhood) but are **semantically opposed**
//! (their vectors are dissimilar or orthogonal).
//!
//! # Concepts
//! - **Structural Twin**: Nodes that share a high Jaccard similarity in their
//!   combined incoming and outgoing edge connections.
//! - **Semantic Impostor**: A structural twin whose vector representation indicates
//!   it means something completely different from its peers.
//!
//! # Use Cases
//! - **Sybil Attack Detection**: Identifying bot accounts that mirror a real user's
//!   network but push a different narrative (vector).
//! - **Echo Chamber Bridging**: Finding users who talk to the exact same people
//!   but hold fundamentally opposing viewpoints.
//! - **Anomaly Detection**: Finding nodes that "look" like they belong in a cluster
//!   based on edges, but "think" differently based on vectors.
//!
//! # Example
//!
//! ```rust,no_run
//! use aletheiadb::AletheiaDB;
//! use aletheiadb::experimental::doppelganger::DoppelgangerEngine;
//! use aletheiadb::core::id::NodeId;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let db = AletheiaDB::new()?;
//! let engine = DoppelgangerEngine::new(&db);
//!
//! # let candidates = vec![NodeId::new(1).unwrap(), NodeId::new(2).unwrap()];
//! // Find doppelgangers among candidate nodes
//! // High structural similarity threshold (>0.8), low semantic threshold (<0.2)
//! let impostors = engine.find_doppelgangers(&candidates, "embedding", 0.8, 0.2)?;
//!
//! for impostor in impostors {
//!     println!(
//!         "Nodes {} and {} have {} structural similarity but only {} semantic similarity!",
//!         impostor.node_a, impostor.node_b, impostor.structural_similarity, impostor.semantic_similarity
//!     );
//! }
//! # Ok(())
//! # }
//! ```

use crate::AletheiaDB;
use crate::api::transaction::ReadOps;
use crate::core::error::{Error, Result};
use crate::core::id::NodeId;
use std::collections::HashSet;

/// Result of a doppelganger detection analysis.
#[derive(Debug, Clone)]
pub struct DoppelgangerResult {
    /// The first node in the pair.
    pub node_a: NodeId,
    /// The second node in the pair.
    pub node_b: NodeId,
    /// The Jaccard similarity of their combined edge neighborhoods [0.0, 1.0].
    pub structural_similarity: f32,
    /// The cosine similarity of their vector embeddings [-1.0, 1.0].
    pub semantic_similarity: f32,
}

/// The Doppelganger Engine for finding structural twins with divergent semantics.
pub struct DoppelgangerEngine<'a> {
    db: &'a AletheiaDB,
}

impl<'a> DoppelgangerEngine<'a> {
    /// Create a new DoppelgangerEngine.
    pub fn new(db: &'a AletheiaDB) -> Self {
        Self { db }
    }

    /// Find pairs of nodes that are structurally similar but semantically divergent.
    ///
    /// # Arguments
    ///
    /// * `candidates` - The list of nodes to analyze. All pairs within this list will be compared.
    /// * `property_name` - The name of the property containing the vector embedding (e.g., "embedding").
    /// * `min_structural_sim` - Minimum structural (Jaccard) similarity to be considered a twin (e.g., 0.8).
    /// * `max_semantic_sim` - Maximum semantic (Cosine) similarity to be considered an impostor (e.g., 0.2).
    ///
    /// # Returns
    ///
    /// A list of `DoppelgangerResult` detailing the pairs that meet the criteria.
    pub fn find_doppelgangers(
        &self,
        candidates: &[NodeId],
        property_name: &str,
        min_structural_sim: f32,
        max_semantic_sim: f32,
    ) -> Result<Vec<DoppelgangerResult>> {
        if candidates.len() < 2 {
            return Ok(Vec::new());
        }

        let mut results = Vec::new();

        self.db.read(|tx| {
            // Pre-fetch vectors and neighborhoods for all candidates to avoid repeated tx lookups
            let mut node_data = Vec::with_capacity(candidates.len());

            for &id in candidates {
                let node = tx.get_node(id)?;

                // Get vector
                let vector = match node.get_property(property_name) {
                    Some(val) => match val.as_vector() {
                        Some(v) => v.to_vec(),
                        None => continue, // Skip if property is not a vector
                    },
                    None => continue, // Skip if property missing
                };

                // Get combined neighborhood (using target nodes, not edge IDs)
                let mut neighborhood = HashSet::new();

                // Outgoing edges: add target nodes
                for edge_id in tx.get_outgoing_edges(id) {
                    if let Ok(edge) = tx.get_edge(edge_id) {
                        neighborhood.insert(edge.target);
                    }
                }

                // Incoming edges: add source nodes
                for edge_id in tx.get_incoming_edges(id) {
                    if let Ok(edge) = tx.get_edge(edge_id) {
                        neighborhood.insert(edge.source);
                    }
                }

                node_data.push((id, vector, neighborhood));
            }

            // Compare all pairs (O(N^2) for candidate set)
            for i in 0..node_data.len() {
                for j in (i + 1)..node_data.len() {
                    let (id_a, vec_a, neighbors_a) = &node_data[i];
                    let (id_b, vec_b, neighbors_b) = &node_data[j];

                    // Calculate structural similarity (Jaccard)
                    let structural_sim = Self::calculate_jaccard(neighbors_a, neighbors_b);

                    if structural_sim < min_structural_sim {
                        continue; // Not structurally similar enough
                    }

                    // Calculate semantic similarity (Cosine)
                    let semantic_sim = Self::calculate_cosine(vec_a, vec_b)?;

                    if semantic_sim <= max_semantic_sim {
                        results.push(DoppelgangerResult {
                            node_a: *id_a,
                            node_b: *id_b,
                            structural_similarity: structural_sim,
                            semantic_similarity: semantic_sim,
                        });
                    }
                }
            }

            Ok::<_, Error>(())
        })?;

        // Sort by highest structural similarity, then lowest semantic similarity
        results.sort_by(|a, b| {
            b.structural_similarity
                .partial_cmp(&a.structural_similarity)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| {
                    a.semantic_similarity
                        .partial_cmp(&b.semantic_similarity)
                        .unwrap_or(std::cmp::Ordering::Equal)
                })
        });

        Ok(results)
    }

    /// Calculates Jaccard similarity between two sets of nodes.
    /// Result is between 0.0 and 1.0.
    fn calculate_jaccard(set_a: &HashSet<NodeId>, set_b: &HashSet<NodeId>) -> f32 {
        if set_a.is_empty() && set_b.is_empty() {
            return 1.0;
        }

        let intersection_count = set_a.intersection(set_b).count();
        let union_count = set_a.len() + set_b.len() - intersection_count;

        intersection_count as f32 / union_count as f32
    }

    /// Calculates Cosine similarity between two vectors.
    /// Result is between -1.0 and 1.0.
    fn calculate_cosine(a: &[f32], b: &[f32]) -> Result<f32> {
        if a.len() != b.len() {
            return Err(Error::Query(crate::core::error::QueryError::TypeMismatch {
                expected: "Same dimension".to_string(),
                actual: "Different dimensions".to_string(),
            }));
        }

        let mut dot_product = 0.0;
        let mut norm_a = 0.0;
        let mut norm_b = 0.0;

        for (val_a, val_b) in a.iter().zip(b.iter()) {
            dot_product += val_a * val_b;
            norm_a += val_a * val_a;
            norm_b += val_b * val_b;
        }

        if norm_a == 0.0 || norm_b == 0.0 {
            return Ok(0.0);
        }

        Ok(dot_product / (norm_a.sqrt() * norm_b.sqrt()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::transaction::WriteOps;
    use crate::core::property::PropertyValue;
    use crate::{AletheiaDB, properties};
    use tempfile::tempdir;

    #[test]
    fn test_doppelganger_detection() -> Result<()> {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("doppelganger_test");
        let mut config = crate::config::AletheiaDBConfig::default();
        config.wal.wal_dir = db_path;
        let db = AletheiaDB::with_unified_config(config)?;

        // Vectors
        let vector_positive =
            PropertyValue::Vector(std::sync::Arc::from(vec![1.0, 1.0, 1.0].into_boxed_slice())); // Vector A
        let vector_similar =
            PropertyValue::Vector(std::sync::Arc::from(vec![0.9, 0.9, 1.0].into_boxed_slice())); // Vector B (similar to A)
        let vector_negative = PropertyValue::Vector(std::sync::Arc::from(
            vec![-1.0, -1.0, -1.0].into_boxed_slice(),
        )); // Vector C (opposite of A)

        // Create nodes
        let (n1, n2, n3) = db.write(|tx| {
            let n1 = tx.create_node(
                "User",
                properties! {
                    "embedding" => vector_positive.clone(),
                },
            )?;
            let n2 = tx.create_node(
                "User",
                properties! {
                    "embedding" => vector_similar,
                },
            )?;
            let n3 = tx.create_node(
                "User",
                properties! {
                    "embedding" => vector_negative,
                },
            )?;

            // Peers to form the neighborhood
            let peer1 = tx.create_node("Peer", crate::core::property::PropertyMap::default())?;
            let peer2 = tx.create_node("Peer", crate::core::property::PropertyMap::default())?;
            let peer3 = tx.create_node("Peer", crate::core::property::PropertyMap::default())?;

            // Setup structurally identical networks for n1, n2, n3
            // Everyone connects to peer1, peer2, peer3
            for src in [n1, n2, n3] {
                tx.create_edge(
                    src,
                    peer1,
                    "KNOWS",
                    crate::core::property::PropertyMap::default(),
                )?;
                tx.create_edge(
                    src,
                    peer2,
                    "KNOWS",
                    crate::core::property::PropertyMap::default(),
                )?;
                tx.create_edge(
                    src,
                    peer3,
                    "KNOWS",
                    crate::core::property::PropertyMap::default(),
                )?;
                // And receive a connection from peer1
                tx.create_edge(
                    peer1,
                    src,
                    "FOLLOWS",
                    crate::core::property::PropertyMap::default(),
                )?;
            }

            Ok::<_, Error>((n1, n2, n3))
        })?;

        let engine = DoppelgangerEngine::new(&db);
        let candidates = vec![n1, n2, n3];

        // We want to find nodes that share >90% structure but have <0.0 semantic similarity
        // n1 and n2 are structurally identical (1.0) and semantically similar (~0.99) - NOT doppelgangers
        // n1 and n3 are structurally identical (1.0) but semantically opposite (-1.0) - DOPPELGANGERS
        // n2 and n3 are structurally identical (1.0) but semantically opposite (~-0.99) - DOPPELGANGERS
        let results = engine.find_doppelgangers(&candidates, "embedding", 0.9, 0.0)?;

        assert_eq!(results.len(), 2, "Should find exactly 2 doppelganger pairs");

        // First result should be n1 and n3 (or n3 and n1) since their cosine sim is exactly -1.0
        let res1 = &results[0];
        assert_eq!(res1.structural_similarity, 1.0);
        assert!((res1.semantic_similarity - (-1.0)).abs() < 0.01);
        assert!(
            (res1.node_a == n1 && res1.node_b == n3) || (res1.node_a == n3 && res1.node_b == n1)
        );

        Ok(())
    }
}
