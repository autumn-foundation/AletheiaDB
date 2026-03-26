//! EchoChamber: Semantic Bubble Detector.
//!
//! "Are we just talking to ourselves?"
//!
//! EchoChamber analyzes a subset of nodes (a community) to determine if they form an "epistemic bubble".
//! It compares the semantic similarity *within* the community (internal consistency) to the semantic
//! similarity *between* the community and its external neighbors (external openness).
//!
//! # Concepts
//! - **Internal Similarity**: Average cosine similarity between all pairs of nodes within the community.
//! - **External Similarity**: Average cosine similarity between nodes in the community and their neighbors outside the community.
//! - **Echo Chamber Intensity**: The difference between internal and external similarity. High intensity means the community is tight-knit but disconnected from outside perspectives.
//!
//! # Example
//!
//! ```rust,no_run
//! use aletheiadb::AletheiaDB;
//! use aletheiadb::experimental::echo_chamber::EchoChamberDetector;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let db = AletheiaDB::new()?;
//! let detector = EchoChamberDetector::new(&db);
//!
//! // Analyze a specific group of nodes
//! # let community = vec![];
//! let result = detector.analyze(&community, "embedding")?;
//!
//! println!("Echo Chamber Intensity: {}", result.intensity);
//! if result.is_echo_chamber {
//!     println!("Warning: This community is an echo chamber!");
//! }
//! # Ok(())
//! # }
//! ```

use crate::AletheiaDB;
use crate::api::transaction::ReadOps;
use crate::core::error::{Error, Result};
use crate::core::id::NodeId;
use crate::core::vector::ops;
use std::collections::HashSet;

/// The result of an Echo Chamber analysis.
#[derive(Debug, Clone)]
pub struct EchoChamberResult {
    /// Average cosine similarity among nodes within the community.
    pub internal_similarity: f32,
    /// Average cosine similarity between community nodes and external neighbors.
    pub external_similarity: f32,
    /// The difference: internal_similarity - external_similarity.
    pub intensity: f32,
    /// True if intensity > 0.3 (configurable threshold).
    pub is_echo_chamber: bool,
}

/// The Echo Chamber Detector engine.
pub struct EchoChamberDetector<'a> {
    db: &'a AletheiaDB,
}

impl<'a> EchoChamberDetector<'a> {
    /// Create a new EchoChamberDetector instance.
    pub fn new(db: &'a AletheiaDB) -> Self {
        Self { db }
    }

    /// Analyze a community of nodes to detect if it forms an echo chamber.
    ///
    /// # Arguments
    /// * `community` - The list of node IDs forming the community.
    /// * `property_name` - The vector property to use for semantic analysis.
    pub fn analyze(&self, community: &[NodeId], property_name: &str) -> Result<EchoChamberResult> {
        if community.is_empty() {
            return Err(Error::other("Cannot analyze empty community"));
        }

        let community_set: HashSet<NodeId> = community.iter().cloned().collect();

        let mut internal_sum = 0.0;
        let mut internal_count = 0;

        let mut external_sum = 0.0;
        let mut external_count = 0;

        self.db.read(|tx| {
            // 1. Gather all vectors for the community
            let mut community_vectors = Vec::new();
            for &node_id in community {
                if let Ok(node) = tx.get_node(node_id)
                    && let Some(prop) = node.get_property(property_name).and_then(|p| p.as_vector())
                {
                    community_vectors.push((node_id, prop.to_vec()));
                }
            }

            if community_vectors.is_empty() {
                return Err(Error::other(
                    "None of the provided nodes have the specified vector property",
                ));
            }

            // 2. Calculate Internal Similarity (pairwise combinations)
            for i in 0..community_vectors.len() {
                for j in (i + 1)..community_vectors.len() {
                    let mut vec_a = community_vectors[i].1.clone();
                    let mut vec_b = community_vectors[j].1.clone();

                    ops::normalize_in_place(&mut vec_a);
                    ops::normalize_in_place(&mut vec_b);

                    if let Ok(sim) = ops::cosine_similarity(&vec_a, &vec_b) {
                        internal_sum += sim;
                        internal_count += 1;
                    }
                }
            }

            // 3. Calculate External Similarity
            for (node_id, vec) in &community_vectors {
                let mut external_neighbors = HashSet::new();

                // Collect external neighbors via outgoing edges
                let outgoing = tx.get_outgoing_edges(*node_id);
                for edge_id in outgoing {
                    if let Ok(edge) = tx.get_edge(edge_id)
                        && !community_set.contains(&edge.target)
                    {
                        external_neighbors.insert(edge.target);
                    }
                }

                // Collect external neighbors via incoming edges
                let incoming = tx.get_incoming_edges(*node_id);
                for edge_id in incoming {
                    if let Ok(edge) = tx.get_edge(edge_id)
                        && !community_set.contains(&edge.source)
                    {
                        external_neighbors.insert(edge.source);
                    }
                }

                let mut normalized_node_vec = vec.clone();
                ops::normalize_in_place(&mut normalized_node_vec);

                for external_id in external_neighbors {
                    if let Ok(ext_node) = tx.get_node(external_id)
                        && let Some(prop) = ext_node
                            .get_property(property_name)
                            .and_then(|p| p.as_vector())
                    {
                        let mut ext_vec = prop.to_vec();
                        ops::normalize_in_place(&mut ext_vec);

                        if let Ok(sim) = ops::cosine_similarity(&normalized_node_vec, &ext_vec) {
                            external_sum += sim;
                            external_count += 1;
                        }
                    }
                }
            }

            Ok::<(), Error>(())
        })?;

        let internal_similarity = if internal_count > 0 {
            internal_sum / (internal_count as f32)
        } else {
            // If there's only 1 node, internal similarity is theoretically 1.0
            1.0
        };

        let external_similarity = if external_count > 0 {
            external_sum / (external_count as f32)
        } else {
            0.0
        };

        let intensity = internal_similarity - external_similarity;
        let is_echo_chamber = intensity > 0.3;

        Ok(EchoChamberResult {
            internal_similarity,
            external_similarity,
            intensity,
            is_echo_chamber,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::transaction::WriteOps;
    use crate::core::property::PropertyMapBuilder;

    #[test]
    fn test_echo_chamber_detected() {
        let db = AletheiaDB::new().unwrap();

        // Community: highly similar vectors
        let mut c1 = NodeId::new(0).unwrap();
        let mut c2 = NodeId::new(0).unwrap();
        let mut c3 = NodeId::new(0).unwrap();

        // External: very different vector
        let mut ext = NodeId::new(0).unwrap();

        db.write(|tx| {
            c1 = tx
                .create_node(
                    "User",
                    PropertyMapBuilder::new()
                        .insert_vector("belief", &[1.0, 0.1])
                        .build(),
                )
                .unwrap();
            c2 = tx
                .create_node(
                    "User",
                    PropertyMapBuilder::new()
                        .insert_vector("belief", &[0.9, 0.2])
                        .build(),
                )
                .unwrap();
            c3 = tx
                .create_node(
                    "User",
                    PropertyMapBuilder::new()
                        .insert_vector("belief", &[1.0, 0.0])
                        .build(),
                )
                .unwrap();

            ext = tx
                .create_node(
                    "User",
                    PropertyMapBuilder::new()
                        .insert_vector("belief", &[-1.0, 0.0])
                        .build(),
                )
                .unwrap();

            // Connections
            tx.create_edge(c1, c2, "KNOWS", Default::default()).unwrap();
            tx.create_edge(c2, c3, "KNOWS", Default::default()).unwrap();
            // One external connection
            tx.create_edge(c1, ext, "ARGUES_WITH", Default::default())
                .unwrap();

            Ok::<(), Error>(())
        })
        .unwrap();

        let detector = EchoChamberDetector::new(&db);
        let result = detector.analyze(&[c1, c2, c3], "belief").unwrap();

        // Internal similarity should be high (vectors are close)
        assert!(result.internal_similarity > 0.9);
        // External similarity should be negative or low (vectors are opposite)
        assert!(result.external_similarity < 0.0);

        assert!(result.intensity > 0.5);
        assert!(result.is_echo_chamber);
    }

    #[test]
    fn test_diverse_community_not_echo_chamber() {
        let db = AletheiaDB::new().unwrap();

        // Community: somewhat diverse vectors
        let mut c1 = NodeId::new(0).unwrap();
        let mut c2 = NodeId::new(0).unwrap();

        // External: similar to some community members
        let mut ext = NodeId::new(0).unwrap();

        db.write(|tx| {
            c1 = tx
                .create_node(
                    "User",
                    PropertyMapBuilder::new()
                        .insert_vector("belief", &[1.0, 0.0])
                        .build(),
                )
                .unwrap();
            c2 = tx
                .create_node(
                    "User",
                    PropertyMapBuilder::new()
                        .insert_vector("belief", &[0.0, 1.0])
                        .build(),
                )
                .unwrap();

            ext = tx
                .create_node(
                    "User",
                    PropertyMapBuilder::new()
                        .insert_vector("belief", &[0.5, 0.5])
                        .build(),
                )
                .unwrap();

            // Connections
            tx.create_edge(c1, c2, "KNOWS", Default::default()).unwrap();
            tx.create_edge(c1, ext, "KNOWS", Default::default())
                .unwrap();
            tx.create_edge(c2, ext, "KNOWS", Default::default())
                .unwrap();

            Ok::<(), Error>(())
        })
        .unwrap();

        let detector = EchoChamberDetector::new(&db);
        let result = detector.analyze(&[c1, c2], "belief").unwrap();

        // Internal similarity between [1,0] and [0,1] is 0
        assert!(result.internal_similarity < 0.1);

        // External similarity to [0.5, 0.5] is higher than 0
        assert!(result.external_similarity > 0.5);

        assert!(result.intensity < 0.0);
        assert!(!result.is_echo_chamber);
    }
}
