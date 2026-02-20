//! Gravity: Semantic Mass and Orbit Analysis.
//!
//! "Who are the real influencers?"
//!
//! The Gravity engine analyzes the semantic influence of nodes over time.
//! It measures how much a node's neighbors are "pulled" towards it in vector space.
//!
//! # Concepts
//! - **Mass**: The degree centrality of a node (number of connections).
//! - **Orbit**: The trajectory of a neighbor relative to the center node.
//! - **Velocity**: The rate at which a neighbor is moving towards (attraction) or away from (repulsion) the center.
//!
//! # Use Cases
//! - **Influence Mapping**: Identifying nodes that drive semantic change in their community.
//! - **Community Dynamics**: Detecting if a community is tightening (converging) or dispersing (diverging).
//! - **Trend Analysis**: Measuring the velocity of adoption for new concepts.

use crate::AletheiaDB;
use crate::core::error::Result;
use crate::core::id::NodeId;
use crate::core::temporal::{TimeRange, Timestamp, time};

/// Metrics describing the orbital dynamics of a neighbor relative to a center node.
#[derive(Debug, Clone, PartialEq)]
pub struct OrbitMetrics {
    /// The neighbor node being analyzed.
    pub neighbor_id: NodeId,
    /// Semantic distance at the start of the window.
    pub start_distance: Option<f32>,
    /// Semantic distance at the end of the window.
    pub end_distance: Option<f32>,
    /// Velocity of semantic movement (change in distance per second).
    /// Negative = Attraction (moving closer).
    /// Positive = Repulsion (moving away).
    pub velocity: Option<f32>,
}

/// The Gravity engine for analyzing semantic influence.
pub struct GravityWell<'a> {
    db: &'a AletheiaDB,
}

impl<'a> GravityWell<'a> {
    /// Create a new GravityWell.
    pub fn new(db: &'a AletheiaDB) -> Self {
        Self { db }
    }

    /// Analyze the orbit of neighbors around a center node.
    ///
    /// # Arguments
    ///
    /// * `center_id` - The central node (the "Sun").
    /// * `property` - The vector property to analyze (e.g., "embedding").
    /// * `window` - The time range for analysis.
    ///
    /// # Returns
    ///
    /// A list of `OrbitMetrics` for each neighbor connected at `window.end`.
    pub fn analyze_orbit(
        &self,
        center_id: NodeId,
        property: &str,
        window: TimeRange,
    ) -> Result<Vec<OrbitMetrics>> {
        // Fix transaction time to "now" for consistent view
        let tx_time = time::now();

        // 1. Get neighbors at the end of the window
        // We only care about nodes that are currently connected (or at window end)
        let edge_ids = self.db.get_outgoing_edges_at_time(center_id, window.end(), tx_time);

        if edge_ids.is_empty() {
            return Ok(Vec::new());
        }

        let edges = self.db.get_edges_at_time(&edge_ids, window.end(), tx_time)?;

        // Extract valid target IDs
        let target_ids: Vec<NodeId> = edges
            .into_iter()
            .filter_map(|(_, edge_opt)| edge_opt.map(|e| e.target))
            .collect();

        // 2. Get Center Node vectors at start and end
        let center_start_vec = self.get_vector_at(center_id, property, window.start(), tx_time)?;
        let center_end_vec = self.get_vector_at(center_id, property, window.end(), tx_time)?;

        let duration_secs = window.duration_micros().unwrap_or(0) as f32 / 1_000_000.0;
        let mut metrics = Vec::with_capacity(target_ids.len());

        for target_id in target_ids {
            let target_start_vec = self.get_vector_at(target_id, property, window.start(), tx_time)?;
            let target_end_vec = self.get_vector_at(target_id, property, window.end(), tx_time)?;

            let start_dist = if let (Some(c), Some(t)) = (&center_start_vec, &target_start_vec) {
                Some(cosine_distance(c, t))
            } else {
                None
            };

            let end_dist = if let (Some(c), Some(t)) = (&center_end_vec, &target_end_vec) {
                Some(cosine_distance(c, t))
            } else {
                None
            };

            let velocity = if let (Some(s), Some(e)) = (start_dist, end_dist) {
                if duration_secs > 0.0 {
                    Some((e - s) / duration_secs)
                } else {
                    Some(0.0)
                }
            } else {
                None
            };

            metrics.push(OrbitMetrics {
                neighbor_id: target_id,
                start_distance: start_dist,
                end_distance: end_dist,
                velocity,
            });
        }

        Ok(metrics)
    }

    /// Helper to get vector property at a specific time.
    fn get_vector_at(
        &self,
        node_id: NodeId,
        property: &str,
        valid_time: Timestamp,
        tx_time: Timestamp,
    ) -> Result<Option<Vec<f32>>> {
        // 1. Try full history scan (most robust for experimental analysis)
        // This ensures we find the exact version even if temporal indices are lagging.
        // We use is_valid_at (ignoring transaction time) to find *any* version that covers the valid time.
        // We iterate through all versions and keep the last match, effectively preferring the
        // most recent version (highest ID) that claims validity for that time.
        // This works around issues where past versions might be transactionally closed (corrections)
        // but still represent the best knowledge of that time period.
        if let Ok(history) = self.db.get_node_history(node_id) {
            let mut best_match: Option<Vec<f32>> = None;
            for version in history.versions {
                if version.temporal.is_valid_at(valid_time) {
                    if let Some(val) = version.properties.get(property) {
                        if let Some(vec) = val.as_vector() {
                            best_match = Some(vec.to_vec());
                        }
                    }
                }
            }
            if let Some(vec) = best_match {
                return Ok(Some(vec));
            }
        }

        // 2. Try standard temporal lookup (fast path)
        if let Ok(node) = self.db.get_node_at_time(node_id, valid_time, tx_time) {
            return Self::extract_vector(&node, property);
        }

        // 3. Fallback: Current state (latest)
        if let Ok(node) = self.db.get_node(node_id) {
            return Self::extract_vector(&node, property);
        }

        Ok(None)
    }

    fn extract_vector(node: &crate::core::graph::Node, property: &str) -> Result<Option<Vec<f32>>> {
        if let Some(val) = node.properties.get(property) {
            if let Some(vec) = val.as_vector() {
                return Ok(Some(vec.to_vec()));
            }
        }
        Ok(None)
    }
}

/// Calculate Cosine Distance (1.0 - Cosine Similarity).
/// Range: [0.0, 2.0]. 0.0 = identical, 1.0 = orthogonal, 2.0 = opposite.
fn cosine_distance(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() {
        return 1.0;
    }

    let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    let norm_a: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let norm_b: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();

    if norm_a == 0.0 || norm_b == 0.0 {
        return 1.0;
    }

    let similarity = dot / (norm_a * norm_b);
    // Clamp to handle potential float precision issues
    let similarity = similarity.clamp(-1.0, 1.0);
    1.0 - similarity
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::property::PropertyMapBuilder;
    use crate::api::transaction::WriteOps;

    #[test]
    fn test_gravity_attraction() {
        let db = AletheiaDB::new().unwrap();

        // 1. Setup Center Node (The Sun) at [1.0, 0.0]
        let center_props = PropertyMapBuilder::new()
            .insert_vector("vec", &[1.0, 0.0])
            .build();
        let center = db.create_node("Sun", center_props).unwrap();

        // 2. Setup Neighbor (The Planet) at [0.0, 1.0] (Orthogonal, dist = 1.0)
        let neighbor_props = PropertyMapBuilder::new()
            .insert_vector("vec", &[0.0, 1.0])
            .build();
        let neighbor = db.create_node("Planet", neighbor_props).unwrap();

        // Connect them
        db.create_edge(
            center,
            neighbor,
            "ORBITS",
            PropertyMapBuilder::new().build()
        ).unwrap();

        // Ensure start time is strictly after creation
        std::thread::sleep(std::time::Duration::from_millis(10));
        let t_start = time::now();

        // Wait a bit to establish duration
        std::thread::sleep(std::time::Duration::from_millis(50));

        // 3. Move Neighbor closer to [1.0, 0.0] -> [0.7, 0.7] (approx 45 deg)
        let update_props = PropertyMapBuilder::new()
            .insert_vector("vec", &[0.707, 0.707])
            .build();

        db.write(|tx| tx.update_node(neighbor, update_props)).unwrap();

        std::thread::sleep(std::time::Duration::from_millis(50));
        let t_end = time::now();

        // 4. Analyze Orbit
        let gravity = GravityWell::new(&db);
        let window = TimeRange::new(t_start, t_end).unwrap();

        let metrics = gravity.analyze_orbit(center, "vec", window).unwrap();

        assert_eq!(metrics.len(), 1);
        let m = &metrics[0];

        assert!(m.start_distance.is_some(), "Start distance is None");
        assert!(m.end_distance.is_some(), "End distance is None");

        // Start dist (orthogonal) ~= 1.0
        // End dist (45 deg) ~= 1.0 - 0.707 = 0.293
        assert!(m.start_distance.unwrap() > 0.9, "Start distance should be high");
        assert!(m.end_distance.unwrap() < 0.4, "End distance should be lower");

        // Velocity should be negative (attraction)
        assert!(m.velocity.unwrap() < 0.0, "Velocity should be negative (attraction)");
    }
}
