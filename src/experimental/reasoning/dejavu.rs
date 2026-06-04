#![allow(clippy::collapsible_if)]
//! DejaVu: Historical Semantic Recurrence.
//!
//! "We've been here before."
//!
//! DejaVu detects if a node's current semantic state is a repetition of a past state,
//! after having deviated significantly in between. It answers questions like:
//! - "Is this user reverting to their old habits?"
//! - "Has this project's focus returned to its original scope after a pivot?"
//!
//! # Example
//! ```rust
//! # #[cfg(feature = "semantic-reasoning")]
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! use aletheiadb::AletheiaDB;
//! use aletheiadb::experimental::dejavu::DejavuEngine;
//! use aletheiadb::core::property::PropertyMapBuilder;
//! use aletheiadb::api::transaction::WriteOps;
//!
//! let db = AletheiaDB::new()?;
//! # let node_id = db.write(|tx| {
//! #     tx.create_node(
//! #         "Topic",
//! #         PropertyMapBuilder::new()
//! #             .insert_vector("embedding", &[1.0, 0.0, 0.0])
//! #             .build()
//! #     )
//! # })?;
//!
//! let engine = DejavuEngine::new(&db);
//! let result = engine.detect_recurrence(node_id, "embedding", 0.9, 0.5)?;
//! if result.is_recurring {
//!     println!("Node reverted to state from version {}", result.historical_version.unwrap());
//! }
//! # Ok(())
//! # }
//! # #[cfg(not(feature = "semantic-reasoning"))]
//! # fn main() {}
//! ```

use crate::AletheiaDB;
use crate::core::error::Result;
use crate::core::id::NodeId;
use crate::core::vector::ops::cosine_similarity;

/// Result of a DejaVu recurrence check.
#[derive(Debug, Clone, PartialEq)]
pub struct DejavuResult {
    /// True if the node has returned to a past state.
    pub is_recurring: bool,
    /// The version number of the past state it resembles.
    pub historical_version: Option<u64>,
    /// The similarity score to the past state.
    pub similarity_score: f32,
}

/// The DejaVu Engine.
pub struct DejavuEngine<'a> {
    db: &'a AletheiaDB,
}

impl<'a> DejavuEngine<'a> {
    /// Create a new DejaVu Engine.
    pub fn new(db: &'a AletheiaDB) -> Self {
        Self { db }
    }

    /// Detect if a node has reverted to a past semantic state.
    ///
    /// # Arguments
    /// * `node_id` - The node to analyze.
    /// * `property_name` - The vector property to compare.
    /// * `similarity_threshold` - How similar the states need to be to count as a recurrence (e.g., 0.9).
    /// * `deviation_threshold` - How far the node must have drifted in between (e.g., < 0.7).
    pub fn detect_recurrence(
        &self,
        node_id: NodeId,
        property_name: &str,
        similarity_threshold: f32,
        deviation_threshold: f32,
    ) -> Result<DejavuResult> {
        let history = self.db.get_node_history(node_id)?;

        if history.versions.len() < 3 {
            return Ok(DejavuResult {
                is_recurring: false,
                historical_version: None,
                similarity_score: 0.0,
            });
        }

        let current_version = history.versions.last().unwrap();
        let current_val = match current_version.properties.get(property_name) {
            Some(v) => v,
            None => {
                return Ok(DejavuResult {
                    is_recurring: false,
                    historical_version: None,
                    similarity_score: 0.0,
                });
            }
        };

        let current_vec = match current_val.as_vector() {
            Some(v) => v,
            None => {
                return Ok(DejavuResult {
                    is_recurring: false,
                    historical_version: None,
                    similarity_score: 0.0,
                });
            }
        };

        // Track the lowest similarity seen between the current state and any past state we iterate through.
        let mut max_deviation = 1.0_f32;

        // We iterate backwards from the second-to-last version
        for i in (0..history.versions.len() - 1).rev() {
            let past_version = &history.versions[i];

            if let Some(past_val) = past_version.properties.get(property_name) {
                if let Some(past_vec) = past_val.as_vector() {
                    if let Ok(similarity) = cosine_similarity(current_vec, past_vec) {
                        if similarity < max_deviation {
                            max_deviation = similarity;
                        }

                        // If it's similar to this past version, AND it deviated at some point in between
                        if similarity >= similarity_threshold
                            && max_deviation <= deviation_threshold
                        {
                            return Ok(DejavuResult {
                                is_recurring: true,
                                historical_version: Some(past_version.version_number),
                                similarity_score: similarity,
                            });
                        }
                    }
                }
            }
        }

        Ok(DejavuResult {
            is_recurring: false,
            historical_version: None,
            similarity_score: 0.0,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::AletheiaDB;
    use crate::api::transaction::WriteOps;
    use crate::core::property::PropertyMapBuilder;

    #[test]
    fn test_dejavu_detection() {
        let db = AletheiaDB::new().unwrap();

        // V1: Origin
        let node_id = db
            .write(|tx| {
                tx.create_node(
                    "Topic",
                    PropertyMapBuilder::new()
                        .insert_vector("embedding", &[1.0, 0.0, 0.0])
                        .build(),
                )
            })
            .unwrap();

        // V2: Deviation
        db.write(|tx| {
            tx.update_node(
                node_id,
                PropertyMapBuilder::new()
                    .insert_vector("embedding", &[0.0, 1.0, 0.0])
                    .build(),
            )
        })
        .unwrap();

        // V3: Return to Origin
        db.write(|tx| {
            tx.update_node(
                node_id,
                PropertyMapBuilder::new()
                    .insert_vector("embedding", &[0.99, 0.1, 0.0])
                    .build(),
            )
        })
        .unwrap();

        let engine = DejavuEngine::new(&db);
        let result = engine
            .detect_recurrence(node_id, "embedding", 0.9, 0.5)
            .unwrap();

        assert!(result.is_recurring);
        assert_eq!(result.historical_version, Some(1));
    }

    #[test]
    fn test_no_recurrence_if_no_deviation() {
        let db = AletheiaDB::new().unwrap();

        let node_id = db
            .write(|tx| {
                tx.create_node(
                    "Topic",
                    PropertyMapBuilder::new()
                        .insert_vector("embedding", &[1.0, 0.0, 0.0])
                        .build(),
                )
            })
            .unwrap();

        // Stays similar
        db.write(|tx| {
            tx.update_node(
                node_id,
                PropertyMapBuilder::new()
                    .insert_vector("embedding", &[0.99, 0.1, 0.0])
                    .build(),
            )
        })
        .unwrap();

        db.write(|tx| {
            tx.update_node(
                node_id,
                PropertyMapBuilder::new()
                    .insert_vector("embedding", &[0.98, 0.1, 0.1])
                    .build(),
            )
        })
        .unwrap();

        let engine = DejavuEngine::new(&db);
        let result = engine
            .detect_recurrence(node_id, "embedding", 0.9, 0.5)
            .unwrap();

        // Should not be recurring because it never deviated below 0.5
        assert!(!result.is_recurring);
    }
}
