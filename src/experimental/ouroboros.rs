#![allow(clippy::collapsible_if)]

//! Ouroboros: Cyclic Semantic Drift Engine.
//!
//! "The serpent that eats its own tail."
//!
//! Ouroboros detects entities whose semantic meaning (vectors) drifts significantly
//! over time, but eventually returns to a state very close to its original starting point.
//! This represents a cyclical journey in semantic space.
//!
//! # Concepts
//! - **Departure**: The entity's vector moves far away from its initial state (exceeds a divergence threshold).
//! - **Return**: The entity's vector eventually comes back close to the initial state (falls below a return threshold).
//! - **Cycle**: A completed departure and return.
//!
//! # Use Cases
//! - **LLM Persona Regression**: An agent learns new concepts, drifts, but eventually "forgets" or regresses to its base prompt semantics.
//! - **Market Cycles**: Detecting trends or assets that go through hype cycles and return to fundamental semantic baselines.
//! - **User Behavior**: Identifying users who explore new interests but eventually return to their core habits.
//!
//! # Example
//! ```rust
//! // Requires features = ["nova"]
//! use aletheiadb::AletheiaDB;
//! use aletheiadb::experimental::ouroboros::Ouroboros;
//! use aletheiadb::core::id::NodeId;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let db = AletheiaDB::new()?;
//! // ... setup temporal graph ...
//! # let subject_node = NodeId::new(0).unwrap();
//!
//! let detector = Ouroboros::new(&db);
//! let cycles = detector.detect_cycles(
//!     subject_node,
//!     "embedding",
//!     0.5, // divergence threshold
//!     0.1, // return threshold
//! )?;
//!
//! println!("Detected {} semantic cycles", cycles.len());
//! # Ok(())
//! # }
//! ```

use crate::AletheiaDB;
use crate::core::error::{Error, Result};
use crate::core::id::NodeId;
use crate::core::temporal::Timestamp;
use crate::core::vector::ops;

/// A detected semantic cycle for a node.
#[derive(Debug, Clone)]
pub struct SemanticCycle {
    /// The node that completed the cycle.
    pub node_id: NodeId,
    /// The timestamp when the cycle started.
    pub start_time: Timestamp,
    /// The timestamp of maximum divergence.
    pub peak_divergence_time: Timestamp,
    /// The timestamp when the cycle completed (returned to origin).
    pub end_time: Timestamp,
    /// The maximum divergence distance from the origin (1.0 - cosine_similarity).
    pub max_divergence: f32,
}

/// The Ouroboros Engine.
pub struct Ouroboros<'a> {
    db: &'a AletheiaDB,
}

impl<'a> Ouroboros<'a> {
    /// Create a new Ouroboros engine.
    pub fn new(db: &'a AletheiaDB) -> Self {
        Self { db }
    }

    /// Detect semantic cycles for a given node over its history.
    ///
    /// # Arguments
    /// * `node_id` - The node to analyze.
    /// * `property_name` - The vector property to track.
    /// * `divergence_threshold` - Minimum distance (1.0 - sim) to consider it a "departure" (e.g., 0.3).
    /// * `return_threshold` - Maximum distance (1.0 - sim) to consider it a "return" (e.g., 0.05).
    ///
    /// # Returns
    /// A list of detected cycles.
    pub fn detect_cycles(
        &self,
        node_id: NodeId,
        property_name: &str,
        divergence_threshold: f32,
        return_threshold: f32,
    ) -> Result<Vec<SemanticCycle>> {
        if divergence_threshold <= return_threshold {
            return Err(Error::other(
                "divergence_threshold must be greater than return_threshold",
            ));
        }

        let history = self.db.get_node_history(node_id)?;
        if history.versions.is_empty() {
            return Ok(Vec::new());
        }

        let mut cycles = Vec::new();

        let mut temporal_states = Vec::new();
        for version in history.versions.iter() {
            if let Some(prop) = version
                .properties
                .get(property_name)
                .and_then(|p| p.as_vector())
            {
                let mut vec = prop.to_vec();
                ops::normalize_in_place(&mut vec);
                temporal_states.push((version.temporal.valid_time().start(), vec));
            }
        }

        temporal_states.sort_by_key(|(t, _)| *t);

        if temporal_states.len() < 3 {
            return Ok(cycles);
        }

        let mut current_origin: Option<(Timestamp, Vec<f32>)> = None;
        let mut max_div = 0.0;
        let mut peak_time = Timestamp::from(0);
        let mut has_departed = false;

        for (time, vec) in temporal_states {
            if current_origin.is_none() {
                current_origin = Some((time, vec));
                continue;
            }

            let (origin_time, origin_vec) = current_origin.as_ref().unwrap();
            let sim = ops::cosine_similarity(origin_vec, &vec)?;
            let distance = (1.0_f32 - sim).max(0.0);

            if !has_departed {
                if distance >= divergence_threshold {
                    has_departed = true;
                    max_div = distance;
                    peak_time = time;
                }
            } else {
                if distance > max_div {
                    max_div = distance;
                    peak_time = time;
                }

                if distance <= return_threshold {
                    cycles.push(SemanticCycle {
                        node_id,
                        start_time: *origin_time,
                        peak_divergence_time: peak_time,
                        end_time: time,
                        max_divergence: max_div,
                    });

                    current_origin = Some((time, vec));
                    has_departed = false;
                    max_div = 0.0;
                }
            }
        }

        Ok(cycles)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::AletheiaDB;
    use crate::api::transaction::WriteOps;
    use crate::core::property::PropertyMapBuilder;

    #[test]
    fn test_ouroboros_cycle_detection() {
        let db = AletheiaDB::new().unwrap();

        let mut node_id = NodeId::new(0).unwrap();

        db.write(|tx| {
            node_id = tx
                .create_node(
                    "Subject",
                    PropertyMapBuilder::new()
                        .insert_vector("embedding", &[1.0, 0.0, 0.0])
                        .build(),
                )
                .unwrap();
            Ok::<(), Error>(())
        })
        .unwrap();

        std::thread::sleep(std::time::Duration::from_millis(10));
        db.write(|tx| {
            tx.update_node(
                node_id,
                PropertyMapBuilder::new()
                    .insert_vector("embedding", &[0.0, 1.0, 0.0])
                    .build(),
            )
            .unwrap();
            Ok::<(), Error>(())
        })
        .unwrap();

        std::thread::sleep(std::time::Duration::from_millis(10));
        db.write(|tx| {
            tx.update_node(
                node_id,
                PropertyMapBuilder::new()
                    .insert_vector("embedding", &[-1.0, 0.0, 0.0])
                    .build(),
            )
            .unwrap();
            Ok::<(), Error>(())
        })
        .unwrap();

        std::thread::sleep(std::time::Duration::from_millis(10));
        db.write(|tx| {
            tx.update_node(
                node_id,
                PropertyMapBuilder::new()
                    .insert_vector("embedding", &[0.99, 0.01, 0.0])
                    .build(),
            )
            .unwrap();
            Ok::<(), Error>(())
        })
        .unwrap();

        let ouroboros = Ouroboros::new(&db);
        let cycles = ouroboros
            .detect_cycles(node_id, "embedding", 0.5, 0.1)
            .unwrap();

        assert_eq!(cycles.len(), 1);
        assert!(cycles[0].max_divergence > 1.9);
    }
}
