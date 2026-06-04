//! Doppelganger Detector: Semantic Plagiarism and Redundancy Checker.
//!
//! "Who are you, and why do you sound exactly like me?"
//!
//! The Doppelganger Detector identifies nodes that are semantically highly similar
//! (their vectors align) but have absolutely no structural overlap (no direct edges
//! or common neighbors). These are often redundant entities or disconnected
//! copies representing the same concept.
//!
//! # Example
//! ```rust,no_run
//! use aletheiadb::AletheiaDB;
//! use aletheiadb::experimental::diagnostics::doppelganger::DoppelgangerDetector;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let db = AletheiaDB::new()?;
//! let detector = DoppelgangerDetector::new(&db);
//!
//! # let node_id = aletheiadb::core::id::NodeId::new(1).unwrap();
//! // Find nodes with > 0.9 similarity that are structurally isolated from the target
//! let doppelgangers = detector.detect(node_id, 0.9, "embedding")?;
//!
//! for pair in doppelgangers {
//!     println!("Found doppelganger: {}", pair.target);
//! }
//! # Ok(())
//! # }
//! ```

use crate::AletheiaDB;
use crate::core::error::Result;
use crate::core::id::NodeId;
use crate::core::vector::ops::cosine_similarity;
use std::collections::HashSet;

/// A detected semantically similar but structurally isolated node.
#[derive(Debug, Clone, PartialEq)]
pub struct DoppelgangerPair {
    /// The original node.
    pub source: NodeId,
    /// The structurally isolated but semantically similar node.
    pub target: NodeId,
    /// The semantic similarity score.
    pub similarity: f32,
}

/// The Doppelganger Detector Engine.
pub struct DoppelgangerDetector<'a> {
    db: &'a AletheiaDB,
}

#[allow(clippy::collapsible_if)]
impl<'a> DoppelgangerDetector<'a> {
    /// Create a new DoppelgangerDetector.
    pub fn new(db: &'a AletheiaDB) -> Self {
        Self { db }
    }

    /// Find doppelgangers for a specific node.
    pub fn detect(
        &self,
        node_id: NodeId,
        similarity_threshold: f32,
        property_name: &str,
    ) -> Result<Vec<DoppelgangerPair>> {
        let mut doppelgangers = Vec::new();

        // 1. Get the target node's vector
        let target_node = self.db.get_node(node_id)?;
        let target_vec_opt = target_node
            .properties
            .get(property_name)
            .and_then(|p| p.as_vector())
            .map(|v| v.to_vec());
        let target_vec = match target_vec_opt {
            Some(v) => v,
            None => return Ok(doppelgangers),
        };

        // 2. Identify the target node's structural neighborhood (1 hop)
        let mut structural_neighborhood = HashSet::new();
        structural_neighborhood.insert(node_id);

        for edge_id in self.db.get_outgoing_edges(node_id) {
            if let Ok(target) = self.db.get_edge_target(edge_id) {
                structural_neighborhood.insert(target);
            }
        }
        for edge_id in self.db.get_incoming_edges(node_id) {
            if let Ok(source) = self.db.get_edge_source(edge_id) {
                structural_neighborhood.insert(source);
            }
        }

        // 3. Scan all nodes (or query a vector index if available)
        // For simplicity, we scan all nodes.
        let scan_results = self.db.query().scan(None).execute(self.db)?;

        for row_result in scan_results {
            let row = row_result?;
            if let Some(node) = row.entity.as_node() {
                let candidate_id = node.id;

                // Skip if it's in the structural neighborhood
                if structural_neighborhood.contains(&candidate_id) {
                    continue;
                }

                // Check semantic similarity
                if let Some(candidate_vec) = node
                    .properties
                    .get(property_name)
                    .and_then(|p| p.as_vector())
                {
                    let sim = cosine_similarity(&target_vec, candidate_vec).unwrap_or(0.0);

                    if sim >= similarity_threshold {
                        // Double check if candidate has edges back to target (just in case)
                        let mut isolated = true;
                        for edge_id in self.db.get_outgoing_edges(candidate_id) {
                            if let Ok(t) = self.db.get_edge_target(edge_id) {
                                if t == node_id {
                                    isolated = false;
                                    break;
                                }
                            }
                        }

                        if isolated {
                            doppelgangers.push(DoppelgangerPair {
                                source: node_id,
                                target: candidate_id,
                                similarity: sim,
                            });
                        }
                    }
                }
            }
        }

        Ok(doppelgangers)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::property::PropertyMapBuilder;

    #[test]
    fn test_detect_doppelgangers() {
        let db = AletheiaDB::new().unwrap();

        // Target Node
        let a = db
            .create_node(
                "Concept",
                PropertyMapBuilder::new()
                    .insert_vector("vec", &[1.0, 0.0])
                    .build(),
            )
            .unwrap();

        // Neighbor Node (Connected, similar, NOT a doppelganger)
        let b = db
            .create_node(
                "Concept",
                PropertyMapBuilder::new()
                    .insert_vector("vec", &[0.9, 0.1])
                    .build(),
            )
            .unwrap();
        db.create_edge(a, b, "LINK", Default::default()).unwrap();

        // Doppelganger Node (Disconnected, similar)
        let c = db
            .create_node(
                "Concept",
                PropertyMapBuilder::new()
                    .insert_vector("vec", &[1.0, 0.0])
                    .build(),
            )
            .unwrap();

        // Noise Node (Disconnected, dissimilar, NOT a doppelganger)
        let _d = db
            .create_node(
                "Concept",
                PropertyMapBuilder::new()
                    .insert_vector("vec", &[0.0, 1.0])
                    .build(),
            )
            .unwrap();

        let detector = DoppelgangerDetector::new(&db);
        let doppelgangers = detector.detect(a, 0.8, "vec").unwrap();

        assert_eq!(doppelgangers.len(), 1);
        assert_eq!(doppelgangers[0].target, c);
        assert!(doppelgangers[0].similarity > 0.99);
    }
}
