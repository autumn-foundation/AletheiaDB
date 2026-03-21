//! Stasis: Semantic Conservation Detector.
//!
//! "What is truly unchanging in the chaos?"
//!
//! The Stasis engine identifies nodes whose semantic vectors have remained
//! exceptionally stable across long periods of time, despite their connections
//! or the graph around them changing. These nodes are the "anchors" or
//! "fundamental truths" of the dataset.
//!
//! # Example
//! ```rust,no_run
//! use aletheiadb::AletheiaDB;
//! use aletheiadb::experimental::stasis::StasisEngine;
//! use aletheiadb::core::temporal::time;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let db = AletheiaDB::new()?;
//! let engine = StasisEngine::new(&db);
//!
//! let start_time = time::now() - 3600 * 1_000_000 * 24 * 30; // 30 days ago
//! let end_time = time::now();
//!
//! // Find nodes that have barely moved (max distance 0.05)
//! let anchors = engine.find_anchors("embedding", start_time, end_time, 0.05)?;
//! # Ok(())
//! # }
//! ```

use crate::AletheiaDB;
use crate::core::error::Result;
use crate::core::id::NodeId;
use crate::core::temporal::Timestamp;
use crate::core::vector::ops;

/// Result of a Stasis analysis.
#[derive(Debug, Clone)]
pub struct AnchorNode {
    /// The stable node
    pub node_id: NodeId,
    /// The maximum deviation across the timeframe
    pub max_deviation: f32,
    /// The number of versions that were analyzed for this node
    pub versions_analyzed: usize,
}

/// The Stasis Engine.
pub struct StasisEngine<'a> {
    db: &'a AletheiaDB,
}

impl<'a> StasisEngine<'a> {
    /// Create a new StasisEngine instance.
    pub fn new(db: &'a AletheiaDB) -> Self {
        Self { db }
    }

    /// Find nodes that have barely moved over a time window.
    pub fn find_anchors(
        &self,
        property_name: &str,
        start_time: Timestamp,
        end_time: Timestamp,
        max_deviation_threshold: f32,
    ) -> Result<Vec<AnchorNode>> {
        let mut anchors = Vec::new();

        // 1. Scan all current nodes to get candidates
        // In a real system, we might only check nodes modified in the timeframe
        let mut node_ids = Vec::new();
        let results = self.db.query().scan(None).execute(self.db)?;
        for row in results {
            if let Some(node) = row?.entity.as_node() {
                node_ids.push(node.id);
            }
        }

        // 2. For each node, analyze history
        for node_id in node_ids {
            if let Ok(history) = self.db.get_node_history(node_id) {
                let mut vectors = Vec::new();

                for version in history.versions {
                    let vt = version.temporal.valid_time();
                    // We only care about versions valid within our window
                    #[allow(clippy::collapsible_if)]
                    if vt.end() >= start_time && vt.start() <= end_time {
                        #[allow(clippy::collapsible_if)]
                        if let Some(prop) = version.properties.get(property_name) {
                            if let Some(vec) = prop.as_vector() {
                                vectors.push(vec.to_vec());
                            }
                        }
                    }
                }

                if vectors.len() < 2 {
                    // Not enough history to prove stasis
                    continue;
                }

                // 3. Calculate max deviation from the FIRST observed vector in the window
                let baseline = &vectors[0];
                let mut max_dev = 0.0_f32;

                for vec in vectors.iter().skip(1) {
                    let similarity = ops::cosine_similarity(baseline, vec)?;
                    let deviation = 1.0 - similarity;
                    if deviation > max_dev {
                        max_dev = deviation;
                    }
                }

                if max_dev <= max_deviation_threshold {
                    anchors.push(AnchorNode {
                        node_id,
                        max_deviation: max_dev,
                        versions_analyzed: vectors.len(),
                    });
                }
            }
        }

        // Sort by lowest deviation (most stable first)
        anchors.sort_by(|a, b| {
            a.max_deviation
                .partial_cmp(&b.max_deviation)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        Ok(anchors)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::transaction::WriteOps;
    use crate::core::property::PropertyMapBuilder;

    #[test]
    fn test_stasis_anchors() {
        let db = AletheiaDB::new().unwrap();
        let engine = StasisEngine::new(&db);

        let start_time = crate::core::temporal::time::now();

        // Node 1: Unchanging Anchor
        let mut n1 = NodeId::new(0).unwrap();
        // Node 2: Volatile
        let mut n2 = NodeId::new(0).unwrap();

        db.write(|tx| {
            n1 = tx
                .create_node(
                    "Fact",
                    PropertyMapBuilder::new()
                        .insert_vector("vec", &[1.0, 0.0])
                        .build(),
                )
                .unwrap();

            n2 = tx
                .create_node(
                    "Opinion",
                    PropertyMapBuilder::new()
                        .insert_vector("vec", &[1.0, 0.0])
                        .build(),
                )
                .unwrap();
            Ok::<(), crate::core::error::Error>(())
        })
        .unwrap();

        // Update them multiple times
        for i in 1..=3 {
            db.write(|tx| {
                // n1 barely changes
                tx.update_node(
                    n1,
                    PropertyMapBuilder::new()
                        .insert_vector("vec", &[1.0, 0.001 * (i as f32)])
                        .build(),
                )
                .unwrap();

                // n2 changes wildly
                tx.update_node(
                    n2,
                    PropertyMapBuilder::new()
                        .insert_vector("vec", &[1.0, 10.0 * (i as f32)])
                        .build(),
                )
                .unwrap();
                Ok::<(), crate::core::error::Error>(())
            })
            .unwrap();
        }

        let end_time = crate::core::temporal::time::now();

        // Find anchors with deviation < 0.05
        let anchors = engine
            .find_anchors("vec", start_time, end_time, 0.05)
            .unwrap();

        assert_eq!(anchors.len(), 1);
        assert_eq!(anchors[0].node_id, n1);
        assert!(anchors[0].max_deviation < 0.05);
        assert_eq!(anchors[0].versions_analyzed, 4); // 1 create + 3 updates
    }
}
