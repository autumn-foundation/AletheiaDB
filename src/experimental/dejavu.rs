#![allow(clippy::collapsible_if)]

//! Déjà Vu: Temporal Cyclic Return Detector.
//!
//! "History doesn't repeat itself, but it often rhymes."
//!
//! The Déjà Vu engine identifies when an entity's semantic state diverges
//! significantly from a past state, only to return to a highly similar state later.
//!
//! # Concepts
//! - **Origin**: The baseline semantic state.
//! - **Divergence**: A point where the semantic state drifted away from the origin.
//! - **Return**: A subsequent point where the semantic state returned close to the origin.
//!
//! # Use Cases
//! - **Agent Behavior**: Detecting LLM agents that get distracted and then return to their original prompt/goal.
//! - **Trend Analysis**: Finding cyclical concepts or cyclical user sentiment.
//!
//! # Example
//! ```rust
//! // Requires features = ["nova"]
//! use aletheiadb::AletheiaDB;
//! use aletheiadb::experimental::dejavu::DejaVuDetector;
//! use aletheiadb::core::NodeId;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let db = AletheiaDB::new()?;
//! // ... setup temporal graph ...
//! # let subject_node = NodeId::new(0).unwrap();
//!
//! let detector = DejaVuDetector::new(&db);
//! // Look for similarity dropping below 0.5, then returning above 0.9
//! let cycles = detector.find_cycles(subject_node, "embedding", 0.5, 0.9)?;
//!
//! println!("Found {} Déjà Vu events!", cycles.len());
//! # Ok(())
//! # }
//! ```

use crate::AletheiaDB;
use crate::core::NodeId;
use crate::core::error::Result;
use crate::core::temporal::Timestamp;
use crate::core::vector::ops;

/// A detected Déjà Vu cycle.
#[derive(Debug, Clone, PartialEq)]
pub struct DejaVuEvent {
    /// The timestamp of the original state.
    pub origin_time: Timestamp,
    /// The timestamp where divergence was greatest.
    pub divergence_time: Timestamp,
    /// The timestamp where the state returned to the origin.
    pub return_time: Timestamp,
    /// Similarity between origin and return (should be > return_threshold).
    pub return_similarity: f32,
    /// Similarity between origin and divergence point (should be < divergence_threshold).
    pub max_divergence_similarity: f32,
}

/// The Déjà Vu Detector.
pub struct DejaVuDetector<'a> {
    db: &'a AletheiaDB,
}

impl<'a> DejaVuDetector<'a> {
    /// Create a new Déjà Vu Detector.
    pub fn new(db: &'a AletheiaDB) -> Self {
        Self { db }
    }

    /// Find Déjà Vu cycles for a node's semantic property.
    ///
    /// # Arguments
    /// * `node_id` - The node to analyze.
    /// * `property_name` - The vector property to use for semantic analysis.
    /// * `divergence_threshold` - Maximum similarity allowed during the divergence phase (e.g., 0.5).
    /// * `return_threshold` - Minimum similarity required to consider it a return to origin (e.g., 0.9).
    pub fn find_cycles(
        &self,
        node_id: NodeId,
        property_name: &str,
        divergence_threshold: f32,
        return_threshold: f32,
    ) -> Result<Vec<DejaVuEvent>> {
        let history = self.db.get_node_history(node_id)?;

        let mut states = Vec::new();

        for version in history.versions.iter() {
            if let Some(vec_prop) = version.properties.get(property_name) {
                if let Some(vec) = vec_prop.as_vector() {
                    states.push((version.temporal.valid_time().start(), vec.to_vec()));
                }
            }
        }

        // Sort states by timestamp
        states.sort_by_key(|(ts, _)| *ts);

        let mut cycles = Vec::new();

        for i in 0..states.len() {
            let (origin_ts, mut origin_vec) = states[i].clone();
            ops::normalize_in_place(&mut origin_vec);

            for j in (i + 1)..states.len() {
                let (div_ts, mut div_vec) = states[j].clone();
                ops::normalize_in_place(&mut div_vec);

                let div_sim = ops::cosine_similarity(&origin_vec, &div_vec).unwrap_or(0.0);

                if div_sim < divergence_threshold {
                    let mut found_return = false;
                    for state_k in states.iter().skip(j + 1) {
                        let (ret_ts, mut ret_vec) = state_k.clone();
                        ops::normalize_in_place(&mut ret_vec);

                        let ret_sim = ops::cosine_similarity(&origin_vec, &ret_vec).unwrap_or(0.0);

                        if ret_sim >= return_threshold {
                            cycles.push(DejaVuEvent {
                                origin_time: origin_ts,
                                divergence_time: div_ts,
                                return_time: ret_ts,
                                return_similarity: ret_sim,
                                max_divergence_similarity: div_sim,
                            });
                            found_return = true;
                            break;
                        }
                    }
                    if found_return {
                        break;
                    }
                }
            }
        }

        Ok(cycles)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_dejavu_logic() {
        let states = [
            (Timestamp::from(100), vec![1.0, 0.0]), // Origin
            (Timestamp::from(200), vec![0.0, 1.0]), // Divergence
            (Timestamp::from(300), vec![0.9, 0.1]), // Return
        ];

        let mut origin_vec = states[0].1.clone();
        ops::normalize_in_place(&mut origin_vec);

        let mut div_vec = states[1].1.clone();
        ops::normalize_in_place(&mut div_vec);

        let mut ret_vec = states[2].1.clone();
        ops::normalize_in_place(&mut ret_vec);

        let div_sim = ops::cosine_similarity(&origin_vec, &div_vec).unwrap_or(0.0);
        let ret_sim = ops::cosine_similarity(&origin_vec, &ret_vec).unwrap_or(0.0);

        assert!(div_sim < 0.5);
        assert!(ret_sim > 0.9);
    }
}

#[cfg(all(test, feature = "nova"))]
mod integration_tests {
    use super::*;
    use crate::AletheiaDB;
    use crate::api::WriteOps;
    use crate::core::property::PropertyMapBuilder;

    #[test]
    fn test_dejavu_integration() {
        let db = AletheiaDB::new().unwrap();

        // Let's create a node and simulate its vector changing over time
        // Vector property: "embedding"

        // T1: Original state (e.g. concept A)
        let vec1 = vec![1.0, 0.0, 0.0];

        // T2: Diverged state (e.g. concept B)
        let vec2 = vec![0.0, 1.0, 0.0];

        // T3: Return state (e.g. concept A')
        let vec3 = vec![0.9, 0.1, 0.0];

        let props1 = PropertyMapBuilder::new().insert("embedding", vec1).build();

        let props2 = PropertyMapBuilder::new().insert("embedding", vec2).build();

        let props3 = PropertyMapBuilder::new().insert("embedding", vec3).build();

        let node_id = {
            let mut tx = db.write_transaction().unwrap();
            let id = tx
                .create_node_with_valid_time("Agent", props1, Some(Timestamp::from(100)))
                .unwrap();
            tx.commit().unwrap();
            id
        };

        {
            let mut tx = db.write_transaction().unwrap();
            tx.update_node_with_valid_time(node_id, props2, Some(Timestamp::from(200)))
                .unwrap();
            tx.commit().unwrap();
        }

        {
            let mut tx = db.write_transaction().unwrap();
            tx.update_node_with_valid_time(node_id, props3, Some(Timestamp::from(300)))
                .unwrap();
            tx.commit().unwrap();
        }

        let detector = DejaVuDetector::new(&db);

        // Divergence threshold < 0.5
        // Return threshold > 0.8
        let cycles = detector
            .find_cycles(node_id, "embedding", 0.5, 0.8)
            .unwrap();

        assert_eq!(cycles.len(), 1, "Should find exactly 1 cycle");

        let cycle = &cycles[0];
        assert_eq!(cycle.origin_time, Timestamp::from(100));
        assert_eq!(cycle.divergence_time, Timestamp::from(200));
        assert_eq!(cycle.return_time, Timestamp::from(300));
        assert!(cycle.max_divergence_similarity < 0.5);
        assert!(cycle.return_similarity >= 0.8);
    }
}
