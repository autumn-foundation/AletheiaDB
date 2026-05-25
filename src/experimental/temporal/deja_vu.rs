//! DejaVu: Semantic Cycle Detector.
//!
//! "Wait, haven't we been here before?"
//!
//! DejaVu analyzes the temporal history of a node to identify "cycles" in
//! semantic space. A cycle occurs when a node starts at semantic position A,
//! drifts significantly far away to position B (the excursion), and then
//! eventually returns to a position very close to A.
//!
//! # Concepts
//! - **Excursion**: The maximum distance the node's vector reaches from the start state.
//! - **Return**: The distance of the final state back to the start state.
//! - **Cycle**: When the Excursion is large (meaning changed) but the Return is small (meaning reverted).
//!
//! # Use Cases
//! - **Flip-Flopping**: Detecting a user or concept that frequently changes opinions and reverts.
//! - **Oscillation Detection**: Finding systems or nodes stuck in a loop.
//! - **Regression Analysis**: Did a product feature evolve only to end up back where it started?

use crate::AletheiaDB;
use crate::core::error::Result;
use crate::core::id::NodeId;
use crate::core::vector::euclidean_distance;

/// Configuration for the DejaVu detector.
#[derive(Debug, Clone)]
pub struct DejaVuConfig {
    /// Minimum distance from the origin to be considered an "excursion" (it actually left).
    pub min_excursion: f32,
    /// Maximum distance from the origin at the end to be considered a "return" (it came back).
    pub max_return: f32,
    /// The property key containing the vector (default: "embedding").
    pub property_key: String,
}

impl Default for DejaVuConfig {
    fn default() -> Self {
        Self {
            min_excursion: 0.5,
            max_return: 0.1,
            property_key: "embedding".to_string(),
        }
    }
}

/// A detected cycle in semantic space.
#[derive(Debug, Clone, PartialEq)]
pub struct CycleDetection {
    /// The node that exhibited cyclic behavior.
    pub node_id: NodeId,
    /// How far the node traveled from its origin.
    pub max_excursion_distance: f32,
    /// How close it ended up to its origin.
    pub return_distance: f32,
    /// Number of versions involved in the history.
    pub version_count: usize,
}

/// The DejaVu Detector engine.
pub struct DejaVuDetector<'a> {
    db: &'a AletheiaDB,
}

#[cfg(feature = "semantic-temporal")]
impl<'a> DejaVuDetector<'a> {
    /// Create a new DejaVuDetector.
    pub fn new(db: &'a AletheiaDB) -> Self {
        Self { db }
    }

    /// Analyze a specific node for semantic cycles across its entire history.
    pub fn analyze_node(
        &self,
        node_id: NodeId,
        config: &DejaVuConfig,
    ) -> Result<Option<CycleDetection>> {
        let history = self.db.get_node_history(node_id)?;

        if history.versions.len() < 3 {
            // Need at least: Start, Excursion, Return
            return Ok(None);
        }

        let mut initial_vector = None;
        let mut max_excursion = 0.0_f32;
        let mut current_vector = None;

        for version in &history.versions {
            if let Some(prop) = version.properties.get(&config.property_key) {
                let vec = if let Some(v) = prop.as_vector() {
                    v
                } else {
                    continue;
                };

                let vec_slice = vec;
                if initial_vector.is_none() {
                    initial_vector = Some(vec_slice.to_vec());
                    current_vector = Some(vec_slice.to_vec());
                    continue;
                }

                if let Some(ref start) = initial_vector {
                    let dist = euclidean_distance(start, vec_slice)?;
                    if dist > max_excursion {
                        max_excursion = dist;
                    }
                }
                current_vector = Some(vec_slice.to_vec());
            }
        }

        if let (Some(start), Some(current)) = (initial_vector, current_vector) {
            let return_dist = euclidean_distance(&start, &current)?;

            if max_excursion >= config.min_excursion && return_dist <= config.max_return {
                return Ok(Some(CycleDetection {
                    node_id,
                    max_excursion_distance: max_excursion,
                    return_distance: return_dist,
                    version_count: history.versions.len(),
                }));
            }
        }

        Ok(None)
    }
}

#[cfg(all(test, feature = "semantic-temporal"))]
mod tests {
    use super::*;
    use crate::PropertyMapBuilder;
    use crate::WriteOps;

    #[test]
    fn test_dejavu_detects_cycle() {
        let db = AletheiaDB::new().unwrap();

        // V1: Origin
        let props1 = PropertyMapBuilder::new()
            .insert_vector("embedding", &[0.0, 0.0, 0.0])
            .build();
        let node_id = db.write(|tx| tx.create_node("Concept", props1)).unwrap();

        // V2: Excursion (drifts far away)
        let props2 = PropertyMapBuilder::new()
            .insert_vector("embedding", &[1.0, 1.0, 1.0])
            .build();
        db.write(|tx| tx.update_node(node_id, props2)).unwrap();

        // V3: Return (comes back close to origin)
        let props3 = PropertyMapBuilder::new()
            .insert_vector("embedding", &[0.05, 0.0, 0.0])
            .build();
        db.write(|tx| tx.update_node(node_id, props3)).unwrap();

        let detector = DejaVuDetector::new(&db);
        let config = DejaVuConfig {
            min_excursion: 1.0,
            max_return: 0.1,
            property_key: "embedding".to_string(),
        };

        let result = detector.analyze_node(node_id, &config).unwrap();
        assert!(result.is_some());

        let cycle = result.unwrap();
        assert_eq!(cycle.node_id, node_id);
        assert!(cycle.max_excursion_distance > 1.0);
        assert!(cycle.return_distance < 0.1);
        assert_eq!(cycle.version_count, 3);
    }

    #[test]
    fn test_dejavu_ignores_linear_drift() {
        let db = AletheiaDB::new().unwrap();

        // V1: Origin
        let props1 = PropertyMapBuilder::new()
            .insert_vector("embedding", &[0.0, 0.0, 0.0])
            .build();
        let node_id = db.write(|tx| tx.create_node("Concept", props1)).unwrap();

        // V2: Excursion
        let props2 = PropertyMapBuilder::new()
            .insert_vector("embedding", &[1.0, 1.0, 1.0])
            .build();
        db.write(|tx| tx.update_node(node_id, props2)).unwrap();

        // V3: Kept going (no return)
        let props3 = PropertyMapBuilder::new()
            .insert_vector("embedding", &[2.0, 2.0, 2.0])
            .build();
        db.write(|tx| tx.update_node(node_id, props3)).unwrap();

        let detector = DejaVuDetector::new(&db);
        let config = DejaVuConfig::default();

        let result = detector.analyze_node(node_id, &config).unwrap();
        assert!(
            result.is_none(),
            "Should not detect a cycle for linear drift"
        );
    }
}
