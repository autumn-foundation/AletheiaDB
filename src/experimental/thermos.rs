//! Thermos: Semantic Temperature and Volatility.
//!
//! "How fast is this concept evolving?"
//!
//! This module measures the "temperature" (rate of change) and "volatility"
//! (total distance traveled) of a vector property over time.
//!
//! # Use Cases
//! - **Concept Drift Detection**: Identify when a definition becomes unstable.
//! - **Hotspot Analysis**: Find areas of the graph undergoing rapid semantic change.
//! - **Caching**: Prioritize caching for "cold" (stable) nodes.

use crate::AletheiaDB;
use crate::core::id::NodeId;
use crate::core::temporal::TimeRange;
use crate::core::vector::ops::euclidean_distance;
use crate::utils::Result;

/// A reading of a node's semantic activity.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ThermalReading {
    /// Total Euclidean distance traveled by the vector in the time window.
    pub volatility: f32,
    /// Average velocity (distance / second) in the time window.
    pub temperature: f32,
    /// Number of updates (versions) in the window.
    pub update_count: usize,
    /// The time span covered by the updates (first to last update).
    pub duration_micros: i64,
}

/// The Thermos engine for semantic temperature measurement.
pub struct Thermos<'a> {
    db: &'a AletheiaDB,
}

impl<'a> Thermos<'a> {
    /// Create a new Thermos instance.
    pub fn new(db: &'a AletheiaDB) -> Self {
        Self { db }
    }

    /// Measure the semantic temperature of a node's vector property.
    ///
    /// # Arguments
    /// * `node_id` - The node to analyze.
    /// * `property` - The vector property name.
    /// * `window` - The time range to analyze.
    pub fn measure_temperature(
        &self,
        node_id: NodeId,
        property: &str,
        window: TimeRange,
    ) -> Result<ThermalReading> {
        // 1. Fetch History
        let history = self.db.get_node_history(node_id)?;

        // Extract vector snapshots: (timestamp_micros, vector)
        let mut snapshots: Vec<(i64, Vec<f32>)> = Vec::new();

        for version in &history.versions {
            let valid_time = version.temporal.valid_time();

            // Check if version is relevant to the window
            if valid_time.start() < window.end() && valid_time.end() > window.start() {
                // Get the property value from this version
                if let Some(prop_val) = version.properties.get(property) {
                    if let Some(vec) = prop_val.as_vector() {
                        let effective_time = valid_time
                            .start()
                            .wallclock()
                            .max(window.start().wallclock());

                        // We only care about versions strictly inside or starting after window start
                        // to calculate movement *within* the window.
                        snapshots.push((effective_time, vec.to_vec()));
                    }
                }
            }
        }

        // Sort by time
        snapshots.sort_by_key(|(t, _)| *t);
        snapshots.dedup_by_key(|(t, _)| *t);

        if snapshots.len() < 2 {
            return Ok(ThermalReading {
                volatility: 0.0,
                temperature: 0.0,
                update_count: snapshots.len(),
                duration_micros: 0,
            });
        }

        let start_time = snapshots.first().unwrap().0;
        let end_time = snapshots.last().unwrap().0;
        let duration = end_time - start_time;

        let total_distance: f32 = snapshots
            .windows(2)
            .map(|w| {
                let (_, v1) = &w[0];
                let (_, v2) = &w[1];
                euclidean_distance(v1, v2)
            })
            .sum::<Result<f32>>()?;


        // Calculate Temperature (Velocity)
        // Avoid division by zero if updates happened in the same microsecond (unlikely but possible with logical clocks)
        let duration_secs = if duration > 0 {
            duration as f32 / 1_000_000.0
        } else {
            1e-6 // Epsilon
        };

        let temperature = total_distance / duration_secs;

        Ok(ThermalReading {
            volatility: total_distance,
            temperature,
            update_count: snapshots.len(),
            duration_micros: duration,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::transaction::WriteOps;
    use crate::core::property::PropertyMapBuilder;
    use crate::core::temporal::time;
    use crate::index::vector::{DistanceMetric, HnswConfig};

    #[test]
    fn test_thermos_static_node() {
        let db = AletheiaDB::new().unwrap();
        db.enable_vector_index("vec", HnswConfig::new(2, DistanceMetric::Euclidean))
            .unwrap();

        let t0 = time::now();
        let props = PropertyMapBuilder::new()
            .insert_vector("vec", &[1.0, 1.0])
            .build();
        let node = db.create_node("Node", props).unwrap();

        // Wait and "update" to same value
        std::thread::sleep(std::time::Duration::from_millis(10));
        let update_props = PropertyMapBuilder::new()
            .insert_vector("vec", &[1.0, 1.0])
            .build();
        db.write(|tx| tx.update_node(node, update_props)).unwrap();

        let t1 = time::now();

        let thermos = Thermos::new(&db);
        let window = TimeRange::new(t0, t1).unwrap();

        let reading = thermos.measure_temperature(node, "vec", window).unwrap();

        assert_eq!(reading.update_count, 2);
        assert!(reading.volatility < 1e-5);
        assert!(reading.temperature < 1e-5);
    }

    #[test]
    fn test_thermos_moving_node() {
        let db = AletheiaDB::new().unwrap();
        db.enable_vector_index("vec", HnswConfig::new(2, DistanceMetric::Euclidean))
            .unwrap();

        let t0 = time::now();

        // Point A: [0, 0]
        let props = PropertyMapBuilder::new()
            .insert_vector("vec", &[0.0, 0.0])
            .build();
        let node = db.create_node("Node", props).unwrap();

        // Wait ~50ms
        std::thread::sleep(std::time::Duration::from_millis(50));

        // Point B: [3, 4] -> Distance 5.0
        let update_props = PropertyMapBuilder::new()
            .insert_vector("vec", &[3.0, 4.0])
            .build();
        db.write(|tx| tx.update_node(node, update_props)).unwrap();

        let t1 = time::now();

        let thermos = Thermos::new(&db);
        let window = TimeRange::new(t0, t1).unwrap();

        let reading = thermos.measure_temperature(node, "vec", window).unwrap();

        assert_eq!(reading.update_count, 2);
        // Volatility should be exactly 5.0
        assert!((reading.volatility - 5.0).abs() < 1e-5);

        // Temperature = 5.0 / ~0.05s = ~100.0
        // Check range to account for timing jitter
        assert!(reading.temperature > 50.0);
        assert!(reading.temperature < 200.0);
    }

    #[test]
    fn test_thermos_oscillating_node() {
        // [0,0] -> [1,0] -> [0,0]
        // Distance: 1 + 1 = 2
        // Displacement: 0
        // Thermos measures path length (volatility), so should be 2.

        let db = AletheiaDB::new().unwrap();
        db.enable_vector_index("vec", HnswConfig::new(2, DistanceMetric::Euclidean))
            .unwrap();

        let t0 = time::now();

        let props = PropertyMapBuilder::new()
            .insert_vector("vec", &[0.0, 0.0])
            .build();
        let node = db.create_node("Oscillator", props).unwrap();

        std::thread::sleep(std::time::Duration::from_millis(10));

        let update1 = PropertyMapBuilder::new()
            .insert_vector("vec", &[1.0, 0.0])
            .build();
        db.write(|tx| tx.update_node(node, update1)).unwrap();

        std::thread::sleep(std::time::Duration::from_millis(10));

        let update2 = PropertyMapBuilder::new()
            .insert_vector("vec", &[0.0, 0.0])
            .build();
        db.write(|tx| tx.update_node(node, update2)).unwrap();

        let t1 = time::now();

        let thermos = Thermos::new(&db);
        let window = TimeRange::new(t0, t1).unwrap();

        let reading = thermos.measure_temperature(node, "vec", window).unwrap();

        assert_eq!(reading.update_count, 3);
        assert!((reading.volatility - 2.0).abs() < 1e-5);
    }
}
