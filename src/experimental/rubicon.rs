//! Rubicon: Semantic Boundary Crossing Detector.
//!
//! "When did we cross the point of no return?"
//!
//! Rubicon detects the exact moments in a node's history when its semantic vector
//! crossed a defined boundary threshold relative to a target "Anchor" vector.
//!
//! # Concepts
//! - **Anchor Vector**: A specific concept in vector space (e.g., the vector for "Suspicious Behavior").
//! - **Threshold**: The maximum cosine distance to be considered "inside" the boundary.
//! - **Crossing**: An event where a node moves from "outside" to "inside" (Entered) or "inside" to "outside" (Exited) the boundary.
//!
//! # Example
//!
//! ```rust,no_run
//! // Requires features = ["nova"]
//! use aletheiadb::AletheiaDB;
//! use aletheiadb::experimental::rubicon::{RubiconEngine, Boundary};
//! use aletheiadb::core::id::NodeId;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let db = AletheiaDB::new()?;
//! let engine = RubiconEngine::new(&db);
//!
//! // Define a boundary: Within 0.2 cosine distance of the "Anomaly" vector
//! let anomaly_vector = vec![1.0, 0.0, 0.5]; // Example vector
//! let boundary = Boundary::new(anomaly_vector, 0.2);
//!
//! # let subject_node = NodeId::new(0).unwrap();
//!
//! // Find all boundary crossings for a node
//! let crossings = engine.detect_crossings(subject_node, "embedding", &boundary)?;
//!
//! for crossing in crossings {
//!     println!("Crossed {:?} boundary at version {}", crossing.direction, crossing.version_number);
//! }
//! # Ok(())
//! # }
//! ```

use crate::AletheiaDB;
use crate::core::error::Result;
use crate::core::id::NodeId;
use crate::core::temporal::Timestamp;
use crate::core::vector::ops;

/// Defines a semantic boundary zone.
#[derive(Debug, Clone)]
pub struct Boundary {
    /// The center vector of the boundary zone.
    pub anchor_vector: Vec<f32>,
    /// The maximum cosine distance to be considered "inside" the boundary.
    pub distance_threshold: f32,
}

impl Boundary {
    /// Create a new Boundary.
    pub fn new(anchor_vector: Vec<f32>, distance_threshold: f32) -> Self {
        Self {
            anchor_vector,
            distance_threshold,
        }
    }

    /// Check if a given vector is inside the boundary.
    pub fn is_inside(&self, vector: &[f32]) -> Result<bool> {
        if self.anchor_vector.len() != vector.len() {
            // Dimension mismatch implies infinite distance (outside)
            return Ok(false);
        }

        let sim = ops::cosine_similarity(&self.anchor_vector, vector)?;
        let dist = (1.0 - sim).max(0.0);
        Ok(dist <= self.distance_threshold)
    }
}

/// The direction of a boundary crossing.
#[derive(Debug, Clone, PartialEq)]
pub enum CrossingDirection {
    /// Node moved from outside to inside the boundary.
    Entered,
    /// Node moved from inside to outside the boundary.
    Exited,
}

/// A recorded boundary crossing event.
#[derive(Debug, Clone)]
pub struct CrossingEvent {
    /// The direction of the crossing.
    pub direction: CrossingDirection,
    /// The version number where the crossing occurred.
    pub version_number: u64,
    /// The transaction time when this version was recorded.
    pub transaction_time: Timestamp,
    /// The valid time when this version became true.
    pub valid_time: Timestamp,
    /// The distance to the anchor vector at this version.
    pub distance: f32,
}

/// The Rubicon Engine.
pub struct RubiconEngine<'a> {
    db: &'a AletheiaDB,
}

impl<'a> RubiconEngine<'a> {
    /// Create a new Rubicon Engine.
    pub fn new(db: &'a AletheiaDB) -> Self {
        Self { db }
    }

    /// Detect all boundary crossings in a node's history.
    pub fn detect_crossings(
        &self,
        node_id: NodeId,
        vector_property: &str,
        boundary: &Boundary,
    ) -> Result<Vec<CrossingEvent>> {
        let history = self.db.get_node_history(node_id)?;
        let mut crossings = Vec::new();

        let mut currently_inside = false;
        let mut has_previous_state = false;

        // Iterate strictly chronologically (Version 1 -> N)
        for version in history.versions.iter() {
            let current_vector = version
                .properties
                .get(vector_property)
                .and_then(|v| v.as_vector());

            if let Some(current) = current_vector {
                // Determine current state relative to boundary
                let is_inside = boundary.is_inside(current)?;

                // Calculate actual distance for the event record
                let sim = ops::cosine_similarity(&boundary.anchor_vector, current).unwrap_or(0.0);
                let distance = (1.0 - sim).max(0.0);

                if !has_previous_state {
                    // Initial state - check if it starts inside
                    if is_inside {
                        crossings.push(CrossingEvent {
                            direction: CrossingDirection::Entered,
                            version_number: version.version_number,
                            transaction_time: version.temporal.transaction_time().start(),
                            valid_time: version.temporal.valid_time().start(),
                            distance,
                        });
                        currently_inside = true;
                    }
                    has_previous_state = true;
                } else if is_inside && !currently_inside {
                    // Crossed INTO the boundary
                    crossings.push(CrossingEvent {
                        direction: CrossingDirection::Entered,
                        version_number: version.version_number,
                        transaction_time: version.temporal.transaction_time().start(),
                        valid_time: version.temporal.valid_time().start(),
                        distance,
                    });
                    currently_inside = true;
                } else if !is_inside && currently_inside {
                    // Crossed OUT OF the boundary
                    crossings.push(CrossingEvent {
                        direction: CrossingDirection::Exited,
                        version_number: version.version_number,
                        transaction_time: version.temporal.transaction_time().start(),
                        valid_time: version.temporal.valid_time().start(),
                        distance,
                    });
                    currently_inside = false;
                }
            } else if currently_inside {
                // The vector property was removed, but we were inside.
                // We consider this crossing OUT of the boundary implicitly.
                crossings.push(CrossingEvent {
                    direction: CrossingDirection::Exited,
                    version_number: version.version_number,
                    transaction_time: version.temporal.transaction_time().start(),
                    valid_time: version.temporal.valid_time().start(),
                    distance: 1.0, // Infinite/max distance due to missing vector
                });
                currently_inside = false;
            }
        }

        Ok(crossings)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::transaction::WriteOps;
    use crate::core::property::PropertyMapBuilder;

    #[test]
    fn test_rubicon_crossing_detection() {
        let db = AletheiaDB::new().unwrap();

        // 1. Setup: Define boundary at [1.0, 0.0] with threshold 0.1
        let boundary = Boundary::new(vec![1.0, 0.0], 0.1);

        let engine = RubiconEngine::new(&db);

        // Version 1: Start outside boundary [0.0, 1.0]
        let mut node_id = NodeId::new(0).unwrap();
        db.write(|tx| {
            node_id = tx
                .create_node(
                    "Subject",
                    PropertyMapBuilder::new()
                        .insert_vector("vec", &[0.0, 1.0])
                        .build(),
                )
                .unwrap();
            Ok::<(), crate::core::error::Error>(())
        })
        .unwrap();

        // Version 2: Move closer, but still outside [0.8, 0.2]
        // Distance from [1.0, 0.0] to [0.8, 0.2] via cosine similarity:
        // dot = 0.8. mag_a = 1. mag_b = sqrt(0.64 + 0.04) = sqrt(0.68) = ~0.824
        // sim = 0.8 / 0.824 = ~0.97. Dist = 1 - 0.97 = 0.03. (Wait, let's pick a clearer point)
        // Let's use [0.5, 0.5] -> sim = 0.5 / sqrt(0.5) = 0.707. dist = 0.29 > 0.1 (Outside)
        db.write(|tx| {
            tx.update_node(
                node_id,
                PropertyMapBuilder::new()
                    .insert_vector("vec", &[0.5, 0.5])
                    .build(),
            )
            .unwrap();
            Ok::<(), crate::core::error::Error>(())
        })
        .unwrap();

        // Version 3: Move inside boundary [0.99, 0.01]
        // sim = 0.99 / sqrt(0.9801 + 0.0001) = 0.99 / sqrt(0.9802) = 0.99 / ~0.99 = ~1.0
        // dist < 0.1 (Inside) => ENTERED event
        db.write(|tx| {
            tx.update_node(
                node_id,
                PropertyMapBuilder::new()
                    .insert_vector("vec", &[0.99, 0.01])
                    .build(),
            )
            .unwrap();
            Ok::<(), crate::core::error::Error>(())
        })
        .unwrap();

        // Version 4: Stay inside [1.0, 0.0] -> No event
        db.write(|tx| {
            tx.update_node(
                node_id,
                PropertyMapBuilder::new()
                    .insert_vector("vec", &[1.0, 0.0])
                    .build(),
            )
            .unwrap();
            Ok::<(), crate::core::error::Error>(())
        })
        .unwrap();

        // Version 5: Move outside boundary [0.0, 1.0] => EXITED event
        db.write(|tx| {
            tx.update_node(
                node_id,
                PropertyMapBuilder::new()
                    .insert_vector("vec", &[0.0, 1.0])
                    .build(),
            )
            .unwrap();
            Ok::<(), crate::core::error::Error>(())
        })
        .unwrap();

        // Detect crossings
        let crossings = engine.detect_crossings(node_id, "vec", &boundary).unwrap();

        // We expect exactly 2 events: Entered at V3, Exited at V5
        assert_eq!(crossings.len(), 2);

        assert_eq!(crossings[0].direction, CrossingDirection::Entered);
        assert_eq!(crossings[0].version_number, 3);

        assert_eq!(crossings[1].direction, CrossingDirection::Exited);
        assert_eq!(crossings[1].version_number, 5);
    }
}
