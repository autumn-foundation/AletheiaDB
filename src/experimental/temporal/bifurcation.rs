//! Bifurcation: Semantic Divergence Detection.
//!
//! "When did we grow apart?"
//!
//! The Bifurcation Engine analyzes the temporal history of two nodes
//! and finds the exact moment their semantic vectors diverged (or converged).
//!
//! # Concepts
//! - **Divergence**: The moment when the cosine similarity between two nodes drops below a threshold.
//! - **Convergence**: The moment when the cosine similarity rises above a threshold.
//!
//! # Example
//! ```rust,no_run
//! # #[cfg(feature = "semantic-temporal")]
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! use aletheiadb::AletheiaDB;
//! use aletheiadb::experimental::temporal::bifurcation::BifurcationEngine;
//! use aletheiadb::core::id::NodeId;
//!
//! let db = AletheiaDB::new()?;
//! let engine = BifurcationEngine::new(&db);
//!
//! # let node_a = NodeId::new(1).unwrap();
//! # let node_b = NodeId::new(2).unwrap();
//! // Find when node_a and node_b's similarity dropped below 0.5
//! if let Some(point) = engine.find_divergence(node_a, node_b, "embedding", 0.5)? {
//!     println!("Diverged at {}! Similarity dropped from {} to {}",
//!         point.timestamp, point.similarity_before, point.similarity_after);
//! }
//! # Ok(())
//! # }
//! # #[cfg(not(feature = "semantic-temporal"))]
//! # fn main() {}
//! ```

#![allow(clippy::collapsible_if)]

use crate::AletheiaDB;
use crate::core::error::Result;
use crate::core::id::NodeId;
use crate::core::temporal::Timestamp;
use crate::core::vector::ops::cosine_similarity;
use std::collections::BTreeMap;

/// A point in time where a significant semantic shift occurred between two nodes.
#[derive(Debug, Clone, PartialEq)]
pub struct BifurcationPoint {
    /// The timestamp where the divergence/convergence occurred.
    pub timestamp: Timestamp,
    /// The cosine similarity right before this timestamp.
    pub similarity_before: f32,
    /// The cosine similarity at this timestamp.
    pub similarity_after: f32,
}

/// The Bifurcation Engine for detecting semantic divergence and convergence over time.
pub struct BifurcationEngine<'a> {
    db: &'a AletheiaDB,
}

#[cfg(feature = "semantic-temporal")]
type VectorTimelineEntry = (Option<Vec<f32>>, Option<Vec<f32>>);

#[cfg(feature = "semantic-temporal")]
impl<'a> BifurcationEngine<'a> {
    /// Create a new BifurcationEngine.
    pub fn new(db: &'a AletheiaDB) -> Self {
        Self { db }
    }

    /// Finds the exact timestamp when the semantic similarity between two nodes
    /// dropped below the given `threshold`.
    pub fn find_divergence(
        &self,
        node_a: NodeId,
        node_b: NodeId,
        property_key: &str,
        threshold: f32,
    ) -> Result<Option<BifurcationPoint>> {
        self.find_shift(node_a, node_b, property_key, |before, after| {
            before >= threshold && after < threshold
        })
    }

    /// Finds the exact timestamp when the semantic similarity between two nodes
    /// rose above the given `threshold`.
    pub fn find_convergence(
        &self,
        node_a: NodeId,
        node_b: NodeId,
        property_key: &str,
        threshold: f32,
    ) -> Result<Option<BifurcationPoint>> {
        self.find_shift(node_a, node_b, property_key, |before, after| {
            before < threshold && after >= threshold
        })
    }

    /// Internal helper to find a shift based on a predicate.
    fn find_shift<F>(
        &self,
        node_a: NodeId,
        node_b: NodeId,
        property_key: &str,
        predicate: F,
    ) -> Result<Option<BifurcationPoint>>
    where
        F: Fn(f32, f32) -> bool,
    {
        let history_a = self.db.get_node_history(node_a)?;
        let history_b = self.db.get_node_history(node_b)?;

        // Build a combined timeline of vector updates.
        // BTreeMap keeps the timestamps ordered.
        // Value is (Option<Vec<f32>>, Option<Vec<f32>>) representing the vector of A and B at that timestamp.
        let mut timeline: BTreeMap<Timestamp, VectorTimelineEntry> = BTreeMap::new();

        for version in history_a.versions.iter() {
            if let Some(prop) = version.properties.get(property_key) {
                if let Some(vec) = prop.as_vector() {
                    let ts = version.temporal.transaction_time().start();
                    timeline.entry(ts).or_insert((None, None)).0 = Some(vec.to_vec());
                }
            }
        }

        for version in history_b.versions.iter() {
            if let Some(prop) = version.properties.get(property_key) {
                if let Some(vec) = prop.as_vector() {
                    let ts = version.temporal.transaction_time().start();
                    timeline.entry(ts).or_insert((None, None)).1 = Some(vec.to_vec());
                }
            }
        }

        let mut current_vec_a: Option<Vec<f32>> = None;
        let mut current_vec_b: Option<Vec<f32>> = None;
        let mut last_similarity: Option<f32> = None;

        for (ts, (vec_a_opt, vec_b_opt)) in timeline {
            if let Some(v) = vec_a_opt {
                current_vec_a = Some(v);
            }
            if let Some(v) = vec_b_opt {
                current_vec_b = Some(v);
            }

            if let (Some(a), Some(b)) = (&current_vec_a, &current_vec_b) {
                if a.len() != b.len() {
                    continue; // Skip dimension mismatch
                }

                let similarity = cosine_similarity(a, b).unwrap_or(0.0);

                if let Some(before) = last_similarity {
                    if predicate(before, similarity) {
                        return Ok(Some(BifurcationPoint {
                            timestamp: ts,
                            similarity_before: before,
                            similarity_after: similarity,
                        }));
                    }
                }

                last_similarity = Some(similarity);
            }
        }

        Ok(None)
    }
}

#[cfg(not(feature = "semantic-temporal"))]
#[allow(deprecated)]
impl<'a> BifurcationEngine<'a> {
    pub fn new(_db: &'a AletheiaDB) -> Self {
        panic!("BifurcationEngine requires the 'nova' feature.");
    }
    pub fn find_divergence(
        &self,
        _node_a: NodeId,
        _node_b: NodeId,
        _property_key: &str,
        _threshold: f32,
    ) -> Result<Option<BifurcationPoint>> {
        panic!("BifurcationEngine requires the 'nova' feature.");
    }
    pub fn find_convergence(
        &self,
        _node_a: NodeId,
        _node_b: NodeId,
        _property_key: &str,
        _threshold: f32,
    ) -> Result<Option<BifurcationPoint>> {
        panic!("BifurcationEngine requires the 'nova' feature.");
    }
}

#[cfg(all(test, feature = "semantic-temporal"))]
mod tests {
    use super::*;
    use crate::api::transaction::WriteOps;
    use crate::core::property::PropertyMapBuilder;

    #[test]
    fn test_bifurcation_divergence() {
        let db = AletheiaDB::new().unwrap();

        // Time T1: Identical vectors
        let vec1 = vec![1.0, 0.0, 0.0];
        let vec2 = vec![1.0, 0.0, 0.0];

        let node_a = db
            .write(|tx| {
                tx.create_node(
                    "Concept",
                    PropertyMapBuilder::new()
                        .insert_vector("vec", &vec1)
                        .build(),
                )
            })
            .unwrap();

        let node_b = db
            .write(|tx| {
                tx.create_node(
                    "Concept",
                    PropertyMapBuilder::new()
                        .insert_vector("vec", &vec2)
                        .build(),
                )
            })
            .unwrap();

        // Time T2: Node B diverges to [0.0, 1.0, 0.0]
        let vec_diverged = vec![0.0, 1.0, 0.0];
        db.write(|tx| {
            tx.update_node(
                node_b,
                PropertyMapBuilder::new()
                    .insert_vector("vec", &vec_diverged)
                    .build(),
            )
        })
        .unwrap();

        let engine = BifurcationEngine::new(&db);
        let point = engine
            .find_divergence(node_a, node_b, "vec", 0.5)
            .unwrap()
            .unwrap();

        assert!(point.similarity_before > 0.99); // They were identical
        assert!(point.similarity_after < 0.01); // Now orthogonal
    }

    #[test]
    fn test_bifurcation_convergence() {
        let db = AletheiaDB::new().unwrap();

        // Time T1: Orthogonal vectors
        let vec1 = vec![1.0, 0.0, 0.0];
        let vec2 = vec![0.0, 1.0, 0.0];

        let node_a = db
            .write(|tx| {
                tx.create_node(
                    "Concept",
                    PropertyMapBuilder::new()
                        .insert_vector("vec", &vec1)
                        .build(),
                )
            })
            .unwrap();

        let node_b = db
            .write(|tx| {
                tx.create_node(
                    "Concept",
                    PropertyMapBuilder::new()
                        .insert_vector("vec", &vec2)
                        .build(),
                )
            })
            .unwrap();

        // Time T2: Node B converges to Node A
        db.write(|tx| {
            tx.update_node(
                node_b,
                PropertyMapBuilder::new()
                    .insert_vector("vec", &vec1)
                    .build(),
            )
        })
        .unwrap();

        let engine = BifurcationEngine::new(&db);
        let point = engine
            .find_convergence(node_a, node_b, "vec", 0.8)
            .unwrap()
            .unwrap();

        assert!(point.similarity_before < 0.01); // They were orthogonal
        assert!(point.similarity_after > 0.99); // Now identical
    }
}
