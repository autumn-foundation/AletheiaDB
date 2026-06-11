//! Doppelganger: Temporal Convergence Detector.
//!
//! "We are becoming the same."
//!
//! Detects nodes that start far apart semantically but converge over time.

use crate::AletheiaDB;
use crate::api::transaction::ReadOps;
use crate::core::error::Result;
use crate::core::id::NodeId;
use crate::core::property::PropertyValue;
use crate::core::temporal::TimeRange;
use crate::core::vector::ops::{cosine_similarity_normalized, euclidean_distance};

/// Represents a pair of nodes that converged over time.
#[derive(Debug, Clone, PartialEq)]
pub struct DoppelgangerResult {
    /// The first node.
    pub source: NodeId,
    /// The second node.
    pub target: NodeId,
    /// Their distance at the start.
    pub initial_distance: f32,
    /// Their similarity at the end.
    pub final_similarity: f32,
}

/// The Doppelganger Detector engine.
pub struct DoppelgangerDetector<'a> {
    db: &'a AletheiaDB,
}

impl<'a> DoppelgangerDetector<'a> {
    /// Creates a new detector.
    pub fn new(db: &'a AletheiaDB) -> Self {
        Self { db }
    }

    /// Finds nodes that started distant but ended up similar.
    pub fn find_doppelgangers(
        &self,
        candidates: &[NodeId],
        window: TimeRange,
        property_name: &str,
        min_initial_distance: f32,
        min_final_similarity: f32,
    ) -> Result<Vec<DoppelgangerResult>> {
        let mut results = Vec::new();

        let start_time = window.start();
        let end_time = window.end();

        // Pre-fetch vectors at start and end for candidates
        let mut start_vectors = std::collections::HashMap::new();
        let mut end_vectors = std::collections::HashMap::new();

        for &id in candidates {
            if let Ok(history) = self.db.get_node_history(id) {
                // Find version valid at start_time
                let mut start_vec = None;
                for v in &history.versions {
                    if v.temporal.valid_time().start() <= start_time
                        && start_time <= v.temporal.valid_time().end()
                        && v.temporal.transaction_time().start() <= start_time
                    {
                        #[allow(clippy::collapsible_match)]
                        if let Some(PropertyValue::Vector(v)) = v.properties.get(property_name) {
                            start_vec = Some(v.to_vec());
                        }
                    }
                }

                #[allow(clippy::collapsible_if)]
                if start_vec.is_none() {
                    if let Some(first_ver) = history.versions.first() {
                        #[allow(clippy::collapsible_match)]
                        if let Some(PropertyValue::Vector(v)) =
                            first_ver.properties.get(property_name)
                        {
                            start_vec = Some(v.to_vec());
                        }
                    }
                }

                if let Some(sv) = start_vec {
                    start_vectors.insert(id, sv);
                }

                // Find version valid at end_time
                let mut end_vec = None;
                for v in &history.versions {
                    if v.temporal.valid_time().start() <= end_time
                        && end_time <= v.temporal.valid_time().end()
                        && v.temporal.transaction_time().start() <= end_time
                    {
                        #[allow(clippy::collapsible_match)]
                        if let Some(PropertyValue::Vector(v)) = v.properties.get(property_name) {
                            end_vec = Some(v.to_vec());
                        }
                    }
                }

                if end_vec.is_none() {
                    let _ = self.db.read(|tx| {
                        if let Ok(node) = tx.get_node(id) {
                            #[allow(clippy::collapsible_match)]
                            if let Some(PropertyValue::Vector(v)) =
                                node.properties.get(property_name)
                            {
                                end_vec = Some(v.to_vec());
                            }
                        }
                        Ok::<(), crate::core::error::Error>(())
                    });
                }

                if let Some(ev) = end_vec {
                    end_vectors.insert(id, ev);
                }
            }
        }

        for i in 0..candidates.len() {
            for j in (i + 1)..candidates.len() {
                let a = candidates[i];
                let b = candidates[j];

                if let (Some(a_start), Some(b_start)) =
                    (start_vectors.get(&a), start_vectors.get(&b))
                {
                    let initial_dist = euclidean_distance(a_start, b_start)?;
                    #[allow(clippy::collapsible_if)]
                    if initial_dist >= min_initial_distance {
                        if let (Some(a_end), Some(b_end)) =
                            (end_vectors.get(&a), end_vectors.get(&b))
                        {
                            let mut a_norm = a_end.clone();
                            let mut b_norm = b_end.clone();
                            crate::core::vector::ops::normalize_in_place(&mut a_norm);
                            crate::core::vector::ops::normalize_in_place(&mut b_norm);
                            let final_sim = cosine_similarity_normalized(&a_norm, &b_norm)?;

                            if final_sim >= min_final_similarity {
                                results.push(DoppelgangerResult {
                                    source: a,
                                    target: b,
                                    initial_distance: initial_dist,
                                    final_similarity: final_sim,
                                });
                            }
                        }
                    }
                }
            }
        }

        Ok(results)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::transaction::WriteOps;
    use crate::core::hlc::HybridTimestamp;
    use crate::core::property::PropertyMapBuilder;

    #[test]
    fn test_doppelganger_detection() {
        let db = AletheiaDB::new().unwrap();

        let t_base = HybridTimestamp::new(100_000, 0).unwrap();

        // Time T0: Initially far apart
        let a = db
            .write(|tx| {
                tx.create_node_with_valid_time(
                    "Node",
                    PropertyMapBuilder::new()
                        .insert("vec", vec![1.0_f32, 0.0_f32])
                        .build(),
                    Some(t_base),
                )
            })
            .unwrap();

        let b = db
            .write(|tx| {
                tx.create_node_with_valid_time(
                    "Node",
                    PropertyMapBuilder::new()
                        .insert("vec", vec![-1.0_f32, 0.0_f32])
                        .build(),
                    Some(t_base),
                )
            })
            .unwrap();

        let t1 = HybridTimestamp::new(150_000, 0).unwrap();

        // Converge at Time T1
        db.write(|tx| {
            tx.update_node_with_valid_time(
                b,
                PropertyMapBuilder::new()
                    .insert("vec", vec![0.9_f32, 0.1_f32])
                    .build(),
                Some(t1),
            )
        })
        .unwrap();

        let detector = DoppelgangerDetector::new(&db);
        let window = TimeRange::new(t_base, t1).unwrap();

        let results = detector
            .find_doppelgangers(&[a, b], window, "vec", 1.0, 0.8)
            .unwrap();

        assert_eq!(results.len(), 1);
        assert!(
            (results[0].source == a && results[0].target == b)
                || (results[0].source == b && results[0].target == a)
        );
    }
}
