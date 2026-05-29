#![allow(clippy::collapsible_if)]
//! Doppelgänger Detector: Convergent Semantic Evolution.
//!
//! "We are not the same... but we became the same."
//!
//! Identifies "Convergent Evolution" in the graph: pairs of nodes that started
//! semantically divergent but evolved to become semantically identical over time,
//! despite having no structural edges between them.
//!
//! # Use Cases
//! - **Duplicate Detection**: Identifying distinct entities that merged into the
//!   same concept.
//! - **Echo Chamber Formation**: Detecting users who arrived at the same beliefs
//!   without directly interacting.
//!
//! # Example
//! ```rust,no_run
//! # #[cfg(feature = "semantic-diagnostics")]
//! use aletheiadb::AletheiaDB;
//! # #[cfg(feature = "semantic-diagnostics")]
//! use aletheiadb::experimental::diagnostics::doppelganger::DoppelgangerDetector;
//! # #[cfg(feature = "semantic-diagnostics")]
//! use aletheiadb::core::temporal::{TimeRange, time};
//!
//! # #[cfg(feature = "semantic-diagnostics")]
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let db = AletheiaDB::new()?;
//! let detector = DoppelgangerDetector::new(&db);
//!
//! let end = time::now();
//! let start = (end.wallclock() - 3600 * 1_000_000 * 24 * 7).into();
//! let window = TimeRange::new(start, end).unwrap();
//!
//! let doppelgangers = detector.detect("embedding", window, 0.9, 0.5)?;
//!
//! for pair in doppelgangers {
//!     println!("Nodes {} and {} became doppelgängers!", pair.node_a, pair.node_b);
//! }
//! # Ok(())
//! # }
//! # #[cfg(not(feature = "semantic-diagnostics"))]
//! # fn main() {}
//! ```

use crate::AletheiaDB;
use crate::core::error::Result;
use crate::core::id::NodeId;
use crate::core::temporal::TimeRange;
use crate::core::vector::ops::cosine_similarity;

/// A detected pair of semantic doppelgängers.
#[derive(Debug, Clone, PartialEq)]
pub struct DoppelgangerPair {
    /// The first node.
    pub node_a: NodeId,
    /// The second node.
    pub node_b: NodeId,
    /// The similarity at the start of the window.
    pub initial_similarity: f32,
    /// The similarity at the end of the window.
    pub final_similarity: f32,
}

/// The Doppelgänger Detector Engine.
pub struct DoppelgangerDetector<'a> {
    db: &'a AletheiaDB,
}

impl<'a> DoppelgangerDetector<'a> {
    /// Create a new DoppelgangerDetector instance.
    pub fn new(db: &'a AletheiaDB) -> Self {
        Self { db }
    }

    /// Detect semantic doppelgängers over a time window.
    ///
    /// # Arguments
    ///
    /// * `vector_property` - The name of the vector property to compare.
    /// * `window` - The time range to analyze.
    /// * `convergence_threshold` - Minimum cosine similarity at the *end* of the window (e.g. 0.9).
    /// * `divergence_threshold` - Maximum cosine similarity at the *start* of the window (e.g. 0.5).
    ///
    /// # Returns
    ///
    /// A list of node pairs that started divergent but converged, with no structural link.
    pub fn detect(
        &self,
        vector_property: &str,
        window: TimeRange,
        convergence_threshold: f32,
        divergence_threshold: f32,
    ) -> Result<Vec<DoppelgangerPair>> {
        let mut doppelgangers = Vec::new();

        // 1. Get all nodes current vectors via a full scan
        let mut current_vectors = Vec::new();
        let scan_results = self.db.query().scan(None).execute(self.db)?;

        for row_result in scan_results {
            let row = row_result?;
            if let Some(node) = row.entity.as_node() {
                if let Some(prop) = node.get_property(vector_property) {
                    if let Some(vec) = prop.as_vector() {
                        current_vectors.push((node.id, vec.to_vec()));
                    }
                }
            }
        }

        // 2. Find pairs that are currently similar (converged)
        // O(N^2) for small MVP, can be optimized with HNSW search later
        let mut converged_pairs = Vec::new();
        for i in 0..current_vectors.len() {
            for j in (i + 1)..current_vectors.len() {
                let (id_a, vec_a) = &current_vectors[i];
                let (id_b, vec_b) = &current_vectors[j];

                if let Ok(sim) = cosine_similarity(vec_a, vec_b) {
                    if sim >= convergence_threshold {
                        converged_pairs.push((*id_a, *id_b, sim));
                    }
                }
            }
        }

        // 3. For each converged pair, check if they were divergent at start of window
        for (id_a, id_b, final_sim) in converged_pairs {
            // Check structural link first (faster to fail)
            if self.are_connected(id_a, id_b) {
                continue;
            }

            // Check historical vectors at start of window
            let hist_a = self.get_historical_vector(id_a, vector_property, window.start());
            let hist_b = self.get_historical_vector(id_b, vector_property, window.start());

            if let (Some(vec_a), Some(vec_b)) = (hist_a, hist_b) {
                if let Ok(initial_sim) = cosine_similarity(&vec_a, &vec_b) {
                    if initial_sim <= divergence_threshold {
                        doppelgangers.push(DoppelgangerPair {
                            node_a: id_a,
                            node_b: id_b,
                            initial_similarity: initial_sim,
                            final_similarity: final_sim,
                        });
                    }
                }
            }
        }

        Ok(doppelgangers)
    }

    /// Check if two nodes have a direct edge between them in either direction.
    fn are_connected(&self, a: NodeId, b: NodeId) -> bool {
        let outgoing = self.db.get_outgoing_edges(a);
        for edge_id in outgoing {
            if let Ok(target) = self.db.get_edge_target(edge_id) {
                if target == b {
                    return true;
                }
            }
        }

        let incoming = self.db.get_incoming_edges(a);
        for edge_id in incoming {
            if let Ok(source) = self.db.get_edge_source(edge_id) {
                if source == b {
                    return true;
                }
            }
        }

        false
    }

    /// Get a node's vector at a specific historical point in time.
    fn get_historical_vector(
        &self,
        node_id: NodeId,
        property_name: &str,
        time: crate::core::temporal::Timestamp,
    ) -> Option<Vec<f32>> {
        if let Ok(history) = self.db.get_node_history(node_id) {
            let mut latest_valid_version = None;
            for version in history.versions.iter() {
                if version.temporal.valid_time().start() <= time
                    && time <= version.temporal.valid_time().end()
                {
                    if version.temporal.transaction_time().start() <= time {
                        latest_valid_version = Some(version);
                    }
                }
            }
            if let Some(v) = latest_valid_version {
                return v
                    .properties
                    .get(property_name)
                    .and_then(|p| p.as_vector())
                    .map(|v| v.to_vec());
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::transaction::WriteOps;
    use crate::core::property::PropertyMapBuilder;
    use crate::core::temporal::time;
    use std::time::Duration;

    #[test]
    fn test_doppelganger_detection() {
        let db = AletheiaDB::new().unwrap();

        // Create nodes at T0 with divergent vectors
        let props_a = PropertyMapBuilder::new()
            .insert_vector("vec", &[1.0, 0.0])
            .build();
        let a = db.create_node("Node", props_a).unwrap();

        let props_b = PropertyMapBuilder::new()
            .insert_vector("vec", &[0.0, 1.0])
            .build();
        let b = db.create_node("Node", props_b).unwrap();

        std::thread::sleep(Duration::from_millis(10));
        let t_start = time::now(); // Start window AFTER creation
        std::thread::sleep(Duration::from_millis(10));

        // T1: Update B to converge with A
        db.write(|tx| {
            let props_b_new = PropertyMapBuilder::new()
                .insert_vector("vec", &[1.0, 0.0])
                .build();
            tx.update_node(b, props_b_new)
        })
        .unwrap();

        std::thread::sleep(Duration::from_millis(10));
        let t_end = time::now();

        let window = TimeRange::new(t_start, t_end).unwrap();
        let detector = DoppelgangerDetector::new(&db);

        let doppelgangers = detector.detect("vec", window, 0.9, 0.1).unwrap();

        assert_eq!(doppelgangers.len(), 1);
        let pair = &doppelgangers[0];
        assert_eq!(pair.initial_similarity, 0.0);
        assert_eq!(pair.final_similarity, 1.0);
        assert!((pair.node_a == a && pair.node_b == b) || (pair.node_a == b && pair.node_b == a));
    }

    #[test]
    fn test_doppelganger_structurally_connected_ignored() {
        let db = AletheiaDB::new().unwrap();

        let props_a = PropertyMapBuilder::new()
            .insert_vector("vec", &[1.0, 0.0])
            .build();
        let a = db.create_node("Node", props_a).unwrap();

        let props_b = PropertyMapBuilder::new()
            .insert_vector("vec", &[0.0, 1.0])
            .build();
        let b = db.create_node("Node", props_b).unwrap();

        // Connect them!
        db.create_edge(a, b, "KNOWS", Default::default()).unwrap();

        std::thread::sleep(Duration::from_millis(10));
        let t_start = time::now(); // Start window AFTER creation
        std::thread::sleep(Duration::from_millis(10));

        db.write(|tx| {
            let props_b_new = PropertyMapBuilder::new()
                .insert_vector("vec", &[1.0, 0.0])
                .build();
            tx.update_node(b, props_b_new)
        })
        .unwrap();

        std::thread::sleep(Duration::from_millis(10));
        let t_end = time::now();

        let window = TimeRange::new(t_start, t_end).unwrap();
        let detector = DoppelgangerDetector::new(&db);

        let doppelgangers = detector.detect("vec", window, 0.9, 0.1).unwrap();

        // Should be ignored because they are connected
        assert_eq!(doppelgangers.len(), 0);
    }
}
