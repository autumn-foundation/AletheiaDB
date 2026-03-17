//! Black Hole: Semantic Sink Detector.
//!
//! "Are all paths leading to a single inescapable concept?"
//!
//! The Black Hole Detector identifies nodes that act as semantic "sinks".
//! These are concepts where many diverse ideas enter (incoming edges from distant vectors)
//! but very few ideas leave, or if they do, they don't go far semantically.
//!
//! # Concepts
//! - **Event Horizon Score**: A measure of how much a node acts as a sink.
//!   High score = many incoming connections from diverse semantic areas, few outgoing connections,
//!   and those outgoing connections stay very close to the node's own vector.
//!
//! # Use Cases
//! - **Ontology Optimization**: Finding overly generic concepts ("Thing", "Entity") that don't add value.
//! - **Reasoning Dead-ends**: Identifying where logic chains terminate prematurely.
//!
//! # Example
//! ```rust,no_run
//! use aletheiadb::AletheiaDB;
//! use aletheiadb::experimental::black_hole::BlackHoleDetector;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let db = AletheiaDB::new()?;
//! # let node_id = db.create_node("Node", Default::default())?;
//! let detector = BlackHoleDetector::new(&db);
//!
//! let score = detector.detect(node_id, "embedding")?;
//! println!("Event Horizon Score: {:.4}", score.event_horizon_score);
//! # Ok(())
//! # }
//! ```

use crate::AletheiaDB;
use crate::core::error::{Error, Result};
use crate::core::id::NodeId;
use crate::core::vector::cosine_similarity;

/// The result of a Black Hole detection analysis.
#[derive(Debug, Clone)]
pub struct BlackHoleResult {
    /// The computed Event Horizon Score (0.0 to 1.0+). Higher means it acts more like a black hole.
    pub event_horizon_score: f32,
    /// Number of incoming semantic paths analyzed.
    pub incoming_count: usize,
    /// Number of outgoing semantic paths analyzed.
    pub outgoing_count: usize,
    /// Average semantic distance from incoming nodes to this node.
    pub avg_incoming_distance: f32,
    /// Average semantic distance from this node to outgoing nodes.
    pub avg_outgoing_distance: f32,
}

/// The Black Hole Detector engine.
pub struct BlackHoleDetector<'a> {
    db: &'a AletheiaDB,
}

impl<'a> BlackHoleDetector<'a> {
    /// Create a new BlackHoleDetector instance.
    pub fn new(db: &'a AletheiaDB) -> Self {
        Self { db }
    }

    /// Analyze a node to determine if it acts as a semantic black hole.
    ///
    /// # Arguments
    /// * `node_id` - The ID of the node to analyze.
    /// * `property` - The name of the vector property to use for semantic distance calculations.
    pub fn detect(&self, node_id: NodeId, property: &str) -> Result<BlackHoleResult> {
        let node = self.db.get_node(node_id)?;
        let center_vec = node
            .properties
            .get(property)
            .and_then(|p| p.as_arc_vector())
            .ok_or_else(|| Error::other("Node does not have the specified vector property"))?;

        let incoming_edges = self.db.get_incoming_edges(node_id);
        let outgoing_edges = self.db.get_outgoing_edges(node_id);

        let mut incoming_count = 0;
        let mut total_incoming_distance = 0.0;

        for edge_id in incoming_edges {
            #[allow(clippy::collapsible_if)]
            if let Ok(source) = self.db.get_edge_source(edge_id) {
                #[allow(clippy::collapsible_if)]
                if let Ok(source_node) = self.db.get_node(source) {
                    if let Some(source_vec) = source_node
                        .properties
                        .get(property)
                        .and_then(|p| p.as_arc_vector())
                    {
                        // Calculate distance: 1.0 - cosine_similarity
                        // This measures how far the incoming concept is from our center node.
                        let sim = cosine_similarity(&source_vec, &center_vec)?;
                        let dist = (1.0 - sim).max(0.0);
                        total_incoming_distance += dist;
                        incoming_count += 1;
                    }
                }
            }
        }

        let mut outgoing_count = 0;
        let mut total_outgoing_distance = 0.0;

        for edge_id in outgoing_edges {
            #[allow(clippy::collapsible_if)]
            if let Ok(target) = self.db.get_edge_target(edge_id) {
                #[allow(clippy::collapsible_if)]
                if let Ok(target_node) = self.db.get_node(target) {
                    if let Some(target_vec) = target_node
                        .properties
                        .get(property)
                        .and_then(|p| p.as_arc_vector())
                    {
                        // Calculate distance: 1.0 - cosine_similarity
                        // This measures how far the concept "escapes" from the center node.
                        let sim = cosine_similarity(&center_vec, &target_vec)?;
                        let dist = (1.0 - sim).max(0.0);
                        total_outgoing_distance += dist;
                        outgoing_count += 1;
                    }
                }
            }
        }

        let avg_incoming_distance = if incoming_count > 0 {
            total_incoming_distance / incoming_count as f32
        } else {
            0.0
        };

        let avg_outgoing_distance = if outgoing_count > 0 {
            total_outgoing_distance / outgoing_count as f32
        } else {
            0.0
        };

        // Calculate Event Horizon Score
        // A black hole has:
        // 1. High incoming volume/distance (pulling in from far away)
        // 2. Low outgoing volume/distance (nothing escapes far)
        //
        // Formula:
        // Gravity In = incoming_count * avg_incoming_distance
        // Escape Out = outgoing_count * avg_outgoing_distance
        // Score = Gravity In / (Escape Out + 1.0)

        let gravity_in = (incoming_count as f32) * avg_incoming_distance;
        let escape_out = (outgoing_count as f32) * avg_outgoing_distance;

        let event_horizon_score = gravity_in / (escape_out + 1.0);

        Ok(BlackHoleResult {
            event_horizon_score,
            incoming_count,
            outgoing_count,
            avg_incoming_distance,
            avg_outgoing_distance,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::property::PropertyMapBuilder;

    #[test]
    fn test_black_hole_detection() {
        let db = AletheiaDB::new().unwrap();

        // Create the potential black hole node
        let center_props = PropertyMapBuilder::new()
            .insert_vector("vec", &[0.0, 1.0])
            .build();
        let center = db.create_node("Node", center_props).unwrap();

        // Create an incoming node (distant concept, orthogonal)
        let incoming_props = PropertyMapBuilder::new()
            .insert_vector("vec", &[1.0, 0.0])
            .build();
        let incoming = db.create_node("Node", incoming_props).unwrap();

        // Create an outgoing node (very close concept, same direction)
        let outgoing_props = PropertyMapBuilder::new()
            .insert_vector("vec", &[0.0, 0.9])
            .build();
        let outgoing = db.create_node("Node", outgoing_props).unwrap();

        // Connect them
        db.create_edge(incoming, center, "LEADS_TO", Default::default()).unwrap();
        db.create_edge(center, outgoing, "RELATES_TO", Default::default()).unwrap();

        let detector = BlackHoleDetector::new(&db);
        let result = detector.detect(center, "vec").unwrap();

        assert_eq!(result.incoming_count, 1);
        assert_eq!(result.outgoing_count, 1);

        // Incoming is orthogonal to center (sim=0, dist=1)
        assert!((result.avg_incoming_distance - 1.0).abs() < 1e-4);

        // Outgoing is very close to center (sim=1, dist=0)
        assert!((result.avg_outgoing_distance - 0.0).abs() < 1e-4);

        // Score = (1 * 1.0) / ((1 * 0.0) + 1.0) = 1.0
        assert!((result.event_horizon_score - 1.0).abs() < 1e-4);
    }
}
