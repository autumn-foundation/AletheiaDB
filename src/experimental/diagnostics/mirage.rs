//! Mirage: Semantic Illusion Detector.
//!
//! "Are we all pointing at a phantom?"
//!
//! The Mirage Detector identifies situations where a group of nodes collectively point to a target node
//! forming a strong semantic consensus among themselves, but the target node itself is semantically
//! disconnected from that consensus.
//!
//! # Concepts
//! - **Crowd Consensus**: The average pairwise cosine similarity between all source nodes that point to a target.
//! - **Target Similarity**: The average cosine similarity between each source node and the target node.
//! - **Mirage Index**: `Crowd Consensus - Target Similarity`. A high index (> 0.5) suggests the target is a "Mirage"—
//!   the sources agree on what they're talking about, but the target they cite doesn't match their expectations.
//!
//! # Example
//! ```rust,no_run
//! use aletheiadb::AletheiaDB;
//! use aletheiadb::experimental::diagnostics::mirage::MirageDetector;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let db = AletheiaDB::new()?;
//! # let target_node_id = aletheiadb::core::id::NodeId::new(1).unwrap();
//! let detector = MirageDetector::new(&db);
//!
//! let score = detector.detect(target_node_id, "embedding")?;
//!
//! if score.mirage_index > 0.5 {
//!     println!("Target {} is a Mirage! Consensus: {}, Target Similarity: {}",
//!         target_node_id, score.consensus, score.similarity_to_target);
//! }
//! # Ok(())
//! # }
//! ```

use crate::AletheiaDB;
use crate::core::error::{Error, Result};
use crate::core::id::NodeId;
use crate::core::vector::cosine_similarity;

/// The score for a Mirage detection analysis.
#[derive(Debug, Clone, PartialEq)]
pub struct MirageScore {
    /// Average pairwise cosine similarity among all incoming source nodes.
    pub consensus: f32,
    /// Average cosine similarity between source nodes and the target node.
    pub similarity_to_target: f32,
    /// The Mirage index (`consensus - similarity_to_target`).
    pub mirage_index: f32,
}

/// Detector for finding semantic illusions (Mirages).
pub struct MirageDetector<'a> {
    db: &'a AletheiaDB,
}

impl<'a> MirageDetector<'a> {
    /// Create a new MirageDetector instance.
    pub fn new(db: &'a AletheiaDB) -> Self {
        Self { db }
    }

    /// Detect if a node is a semantic mirage based on incoming edges.
    ///
    /// # Arguments
    ///
    /// * `target_node` - The target node to evaluate.
    /// * `vector_property` - The name of the vector property containing the embeddings.
    ///
    /// # Returns
    ///
    /// Returns the `MirageScore` for the node, or an Error if vectors are missing or
    /// if there are less than 2 incoming source nodes to form a consensus.
    pub fn detect(&self, target_node: NodeId, vector_property: &str) -> Result<MirageScore> {
        // 1. Get Target Vector
        let target = self.db.get_node(target_node)?;
        let target_vec = target
            .get_property(vector_property)
            .and_then(|p| p.as_vector())
            .ok_or_else(|| Error::Other("Target node is missing vector property".to_string()))?
            .to_vec();

        // 2. Get Incoming Sources and their Vectors
        let incoming_edges = self.db.get_incoming_edges(target_node);
        let mut source_vectors = Vec::new();

        #[allow(clippy::collapsible_if)]
        for edge_id in incoming_edges {
            if let Ok(source_id) = self.db.get_edge_source(edge_id) {
                if let Ok(source_node) = self.db.get_node(source_id) {
                    if let Some(prop) = source_node.get_property(vector_property) {
                        if let Some(vec) = prop.as_vector() {
                            source_vectors.push(vec.to_vec());
                        }
                    }
                }
            }
        }

        if source_vectors.len() < 2 {
            return Err(Error::Other("Not enough incoming source nodes with vectors to form a consensus (requires at least 2).".to_string()));
        }

        // 3. Calculate Target Similarity
        let mut target_sim_sum = 0.0;
        let mut valid_target_sims = 0;
        for s_vec in &source_vectors {
            if let Ok(sim) = cosine_similarity(s_vec, &target_vec) {
                target_sim_sum += sim;
                valid_target_sims += 1;
            }
        }
        let similarity_to_target = if valid_target_sims > 0 {
            target_sim_sum / valid_target_sims as f32
        } else {
            0.0
        };

        // 4. Calculate Crowd Consensus
        let mut consensus_sum = 0.0;
        let mut valid_consensus_pairs = 0;
        for i in 0..source_vectors.len() {
            for j in (i + 1)..source_vectors.len() {
                if let Ok(sim) = cosine_similarity(&source_vectors[i], &source_vectors[j]) {
                    consensus_sum += sim;
                    valid_consensus_pairs += 1;
                }
            }
        }
        let consensus = if valid_consensus_pairs > 0 {
            consensus_sum / valid_consensus_pairs as f32
        } else {
            0.0
        };

        // 5. Compute Mirage Index
        let mirage_index = consensus - similarity_to_target;

        Ok(MirageScore {
            consensus,
            similarity_to_target,
            mirage_index,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::PropertyMapBuilder;
    use crate::WriteOps;

    #[test]
    fn test_mirage_detection() {
        let db = AletheiaDB::new().unwrap();

        let mut t_genuine = NodeId::new(0).unwrap();
        let mut t_mirage = NodeId::new(0).unwrap();
        let mut s1 = NodeId::new(0).unwrap();
        let mut s2 = NodeId::new(0).unwrap();
        let mut s3 = NodeId::new(0).unwrap();

        db.write(|tx| {
            // Source Nodes (Consensus cluster around [1.0, 0.0])
            s1 = tx
                .create_node(
                    "Source",
                    PropertyMapBuilder::new()
                        .try_insert_vector("vec", &[1.0, 0.0])
                        .unwrap()
                        .build(),
                )
                .unwrap();
            s2 = tx
                .create_node(
                    "Source",
                    PropertyMapBuilder::new()
                        .try_insert_vector("vec", &[0.95, 0.05])
                        .unwrap()
                        .build(),
                )
                .unwrap();
            s3 = tx
                .create_node(
                    "Source",
                    PropertyMapBuilder::new()
                        .try_insert_vector("vec", &[0.9, 0.1])
                        .unwrap()
                        .build(),
                )
                .unwrap();

            // Genuine Target (Agrees with consensus)
            t_genuine = tx
                .create_node(
                    "Target",
                    PropertyMapBuilder::new()
                        .try_insert_vector("vec", &[0.98, 0.02])
                        .unwrap()
                        .build(),
                )
                .unwrap();

            // Mirage Target (Completely orthogonal/disconnected concept)
            t_mirage = tx
                .create_node(
                    "Target",
                    PropertyMapBuilder::new()
                        .try_insert_vector("vec", &[0.0, 1.0])
                        .unwrap()
                        .build(),
                )
                .unwrap();

            // Setup edges for genuine case
            tx.create_edge(s1, t_genuine, "CITE", Default::default())
                .unwrap();
            tx.create_edge(s2, t_genuine, "CITE", Default::default())
                .unwrap();
            tx.create_edge(s3, t_genuine, "CITE", Default::default())
                .unwrap();

            // Setup edges for mirage case
            tx.create_edge(s1, t_mirage, "CITE", Default::default())
                .unwrap();
            tx.create_edge(s2, t_mirage, "CITE", Default::default())
                .unwrap();
            tx.create_edge(s3, t_mirage, "CITE", Default::default())
                .unwrap();

            Ok::<(), Error>(())
        })
        .unwrap();

        let detector = MirageDetector::new(&db);

        // Test Genuine
        let score_genuine = detector.detect(t_genuine, "vec").unwrap();
        assert!(
            score_genuine.consensus > 0.9,
            "Sources should have high consensus"
        );
        assert!(
            score_genuine.similarity_to_target > 0.9,
            "Target should be similar to sources"
        );
        assert!(
            score_genuine.mirage_index < 0.1,
            "Genuine target should have low mirage index"
        );

        // Test Mirage
        let score_mirage = detector.detect(t_mirage, "vec").unwrap();
        assert!(
            score_mirage.consensus > 0.9,
            "Sources should have high consensus"
        );
        assert!(
            score_mirage.similarity_to_target < 0.1,
            "Mirage target should be dissimilar to sources"
        );
        assert!(
            score_mirage.mirage_index > 0.8,
            "Mirage target should have high mirage index"
        );
    }
}
