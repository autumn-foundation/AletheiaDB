//! Kairos: Semantic Event Detection & History Summarization.
//!
//! "What *really* happened?"
//!
//! Kairos scans the history of a node and filters out "noise" (small updates)
//! to return a timeline of "Significant Events".
//!
//! # The Hook
//! "LLMs have limited context windows. Instead of feeding them 1,000 minor updates,
//! feed them the 5 moments that mattered."
//!
//! # Logic
//! - **Vector Significance**: Only emit an event if the semantic distance from the *last significant event*
//!   exceeds a threshold. This filters out drift and micro-adjustments.
//! - **Property Significance**: Emits events for property additions/removals/modifications (configurable).
//!
//! # Example
//!
//! ```rust,no_run
//! use aletheiadb::AletheiaDB;
//! use aletheiadb::experimental::kairos::Kairos;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let db = AletheiaDB::new()?;
//! let kairos = Kairos::new(&db);
//! # let node_id = db.create_node("Test", Default::default())?;
//!
//! // Get significant events where vector changed by > 0.1 distance
//! let timeline = kairos.extract_timeline(node_id, "embedding", 0.1)?;
//!
//! for event in timeline {
//!     println!("[{}] {}", event.timestamp, event.description);
//! }
//! # Ok(())
//! # }
//! ```

use crate::AletheiaDB;
use crate::core::error::Result;
use crate::core::id::NodeId;
use crate::core::temporal::{Timestamp, time};
use crate::core::vector::cosine_similarity;

/// A significant event in the node's history.
#[derive(Debug, Clone)]
pub struct TimelineEvent {
    /// When the event was recorded (ISO 8601 string).
    pub timestamp: String,
    /// The timestamp object.
    pub timestamp_obj: Timestamp,
    /// The version number where this event occurred.
    pub version_id: u64,
    /// The type of event.
    pub event_type: EventType,
    /// Human-readable description.
    pub description: String,
    /// Significance score (0.0 - 1.0+).
    pub score: f32,
}

/// The type of significant event.
#[derive(Debug, Clone, PartialEq)]
pub enum EventType {
    /// Node created.
    Created,
    /// Vector changed significantly.
    SemanticJump {
        /// Distance from previous significant state.
        distance: f32,
    },
    /// Property added, removed, or changed.
    PropertyChange {
        /// The property key that changed.
        key: String,
    },
}

/// The Kairos Engine.
pub struct Kairos<'a> {
    db: &'a AletheiaDB,
}

impl<'a> Kairos<'a> {
    /// Create a new Kairos engine.
    pub fn new(db: &'a AletheiaDB) -> Self {
        Self { db }
    }

    /// Extract a timeline of significant events.
    ///
    /// # Arguments
    /// * `node_id` - The node to analyze.
    /// * `vector_property` - The name of the vector property to track for jumps.
    /// * `significance_threshold` - Minimum cosine distance (0.0-2.0) to trigger a SemanticJump.
    pub fn extract_timeline(
        &self,
        node_id: NodeId,
        vector_property: &str,
        significance_threshold: f32,
    ) -> Result<Vec<TimelineEvent>> {
        let history = self.db.get_node_history(node_id)?;
        let mut timeline = Vec::new();

        // Baseline vector for comparison (the vector at the *last significant event*)
        let mut last_significant_vector: Option<Vec<f32>> = None;

        // Iterate strictly chronologically (Version 1 -> N)
        // EntityHistory guarantees versions are sorted by version_number.
        for (i, version) in history.versions.iter().enumerate() {
            let timestamp_obj = version.temporal.transaction_time().start();
            let timestamp = time::to_iso8601(timestamp_obj);
            let version_id = version.version_number;

            // 1. Check for Creation (First version)
            if i == 0 {
                // Initialize vector baseline
                if let Some(vec) = version
                    .properties
                    .get(vector_property)
                    .and_then(|v| v.as_vector())
                {
                    last_significant_vector = Some(vec.to_vec());
                }

                timeline.push(TimelineEvent {
                    timestamp,
                    timestamp_obj,
                    version_id,
                    event_type: EventType::Created,
                    description: format!("Node created with label '{}'", version.label),
                    score: 1.0, // Creation is always significant
                });
                continue;
            }

            // 2. Check Vector Significance
            let current_vector = version
                .properties
                .get(vector_property)
                .and_then(|v| v.as_vector());

            if let Some(current) = current_vector {
                if let Some(baseline) = &last_significant_vector {
                    // Calculate Cosine Distance = 1.0 - Cosine Similarity
                    let similarity = cosine_similarity(baseline, current)?;
                    let dist = 1.0 - similarity;

                    if dist >= significance_threshold {
                        // Significant Jump!
                        timeline.push(TimelineEvent {
                            timestamp: timestamp.clone(),
                            timestamp_obj,
                            version_id,
                            event_type: EventType::SemanticJump { distance: dist },
                            description: format!(
                                "Significant semantic shift (distance: {:.4})",
                                dist
                            ),
                            score: dist,
                        });
                        // Update baseline
                        last_significant_vector = Some(current.to_vec());
                    }
                } else {
                    // Vector appeared for the first time (or re-appeared)
                    timeline.push(TimelineEvent {
                        timestamp: timestamp.clone(),
                        timestamp_obj,
                        version_id,
                        event_type: EventType::SemanticJump { distance: 1.0 },
                        description: "Vector property initialized/added".to_string(),
                        score: 1.0,
                    });
                    last_significant_vector = Some(current.to_vec());
                }
            } else {
                // Vector missing in this version.
                if last_significant_vector.is_some() {
                    last_significant_vector = None;
                }
            }

            // 3. Check Property Changes (Simple check vs previous version for now)
            if i > 0 {
                let prev = &history.versions[i - 1];
                let diff = crate::core::history::VersionDiff::compute(
                    &prev.properties,
                    &version.properties,
                    prev.version_id,
                    version.version_id,
                );

                let mut significant_props = Vec::new();

                // Check added
                for (key, _) in diff.added.iter() {
                    let key_str = crate::core::GLOBAL_INTERNER
                        .resolve_with(*key, |s| s.to_string())
                        .unwrap();
                    if key_str != vector_property {
                        significant_props.push(format!("Added '{}'", key_str));
                    }
                }

                // Check modified
                for (key, _, _) in diff.modified.iter() {
                    let key_str = crate::core::GLOBAL_INTERNER
                        .resolve_with(*key, |s| s.to_string())
                        .unwrap();
                    if key_str != vector_property {
                        significant_props.push(format!("Modified '{}'", key_str));
                    }
                }

                // Check removed
                for (key, _) in diff.removed.iter() {
                    let key_str = crate::core::GLOBAL_INTERNER
                        .resolve_with(*key, |s| s.to_string())
                        .unwrap();
                    if key_str != vector_property {
                        significant_props.push(format!("Removed '{}'", key_str));
                    }
                }

                if !significant_props.is_empty() {
                    timeline.push(TimelineEvent {
                        timestamp,
                        timestamp_obj,
                        version_id,
                        event_type: EventType::PropertyChange {
                            key: "multiple".to_string(),
                        },
                        description: format!(
                            "Properties changed: {}",
                            significant_props.join(", ")
                        ),
                        score: 0.5, // Standard significance
                    });
                }
            }
        }

        Ok(timeline)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::transaction::WriteOps;
    use crate::core::property::PropertyMapBuilder;
    use crate::index::vector::{DistanceMetric, HnswConfig};

    #[test]
    fn test_kairos_noise_filtering() {
        let db = AletheiaDB::new().unwrap();
        // Enable index
        db.enable_vector_index("vec", HnswConfig::new(2, DistanceMetric::Cosine))
            .unwrap();

        // 1. Create Node [1.0, 0.0]
        let props = PropertyMapBuilder::new()
            .insert_vector("vec", &[1.0, 0.0])
            .build();
        let node = db.create_node("Test", props).unwrap();

        // 2. Small Drift (Distance ~0.01) - Should be ignored (Threshold 0.1)
        // [0.999, 0.045] -> Cosine ~0.999 -> Dist 0.001
        db.write(|tx| {
            tx.update_node(
                node,
                PropertyMapBuilder::new()
                    .insert_vector("vec", &[0.999, 0.045])
                    .build(),
            )
        })
        .unwrap();

        // 3. Another Small Drift
        db.write(|tx| {
            tx.update_node(
                node,
                PropertyMapBuilder::new()
                    .insert_vector("vec", &[0.995, 0.1])
                    .build(),
            )
        })
        .unwrap();

        // 4. BIG JUMP (Distance > 0.1)
        // [0.0, 1.0] -> Cosine 0 with original -> Dist 1.0
        // Cosine 0 with previous ([0.995, 0.1]) -> Dist ~0.9
        db.write(|tx| {
            tx.update_node(
                node,
                PropertyMapBuilder::new()
                    .insert_vector("vec", &[0.0, 1.0])
                    .build(),
            )
        })
        .unwrap();

        let kairos = Kairos::new(&db);
        let timeline = kairos.extract_timeline(node, "vec", 0.1).unwrap();

        // Should have:
        // 1. Creation
        // 2. Big Jump
        // Should NOT have the small drifts.

        assert_eq!(timeline.len(), 2);
        assert_eq!(timeline[0].event_type, EventType::Created);

        match &timeline[1].event_type {
            EventType::SemanticJump { distance } => {
                assert!(*distance > 0.1, "Distance should be significant");
            }
            _ => panic!("Expected SemanticJump"),
        }
    }

    #[test]
    fn test_kairos_property_changes() {
        let db = AletheiaDB::new().unwrap();
        let props = PropertyMapBuilder::new().insert("name", "Alice").build();
        let node = db.create_node("Person", props).unwrap();

        // Update name (Property Change)
        db.write(|tx| {
            tx.update_node(
                node,
                PropertyMapBuilder::new().insert("name", "Bob").build(),
            )
        })
        .unwrap();

        let kairos = Kairos::new(&db);
        // High threshold for vector (none exists anyway)
        let timeline = kairos.extract_timeline(node, "vec", 1.0).unwrap();

        assert_eq!(timeline.len(), 2);
        assert_eq!(
            timeline[1].description,
            "Properties changed: Modified 'name'"
        );
    }
}
