//! Thermos: Semantic Temperature & Volatility Gauge.
//!
//! "Is your data heating up?"
//!
//! Thermos measures the rate of change in a node's vector embedding over time.
//! High temperature indicates rapid semantic shift (e.g., a user exploring new topics quickly).
//! Low temperature indicates semantic stability.
//!
//! # Concepts
//! - **Volatility**: Total distance traveled in vector space within a time window.
//! - **Temperature**: Volatility / Time (Velocity of semantic change).
//!
//! # Example
//! ```rust,no_run
//! use aletheiadb::AletheiaDB;
//! use aletheiadb::experimental::thermos::{Thermos, ThermalReading};
//! use aletheiadb::core::temporal::{TimeRange, time};
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let db = AletheiaDB::new()?;
//! # let node_id = db.create_node("N", Default::default())?;
//! let thermos = Thermos::new(&db);
//!
//! let range = TimeRange::new(time::from_secs(0), time::now())?;
//! let reading = thermos.measure_node(node_id, range, "embedding")?;
//!
//! println!("Semantic Volatility: {:.4}", reading.volatility);
//! println!("Semantic Temperature: {:.4} units/sec", reading.temperature);
//! # Ok(())
//! # }
//! ```

use crate::AletheiaDB;
use crate::core::id::NodeId;
use crate::core::temporal::{TimeRange, time};
use crate::core::vector::ops::euclidean_distance;
use crate::core::error::Result;

/// A reading of semantic activity.
#[derive(Debug, Clone, Copy)]
pub struct ThermalReading {
    /// Total Euclidean distance traveled in vector space.
    pub volatility: f32,
    /// Average velocity of change (volatility / time duration in seconds).
    pub temperature: f32,
    /// Number of vector updates considered in the window.
    pub updates: usize,
}

/// The Thermos engine.
pub struct Thermos<'a> {
    db: &'a AletheiaDB,
}

impl<'a> Thermos<'a> {
    /// Create a new Thermos instance.
    pub fn new(db: &'a AletheiaDB) -> Self {
        Self { db }
    }

    /// Measure the semantic temperature of a node over a time range.
    ///
    /// # Arguments
    /// * `node_id` - The node to measure.
    /// * `window` - The time range to analyze.
    /// * `property` - The name of the vector property to track.
    pub fn measure_node(
        &self,
        node_id: NodeId,
        window: TimeRange,
        property: &str,
    ) -> Result<ThermalReading> {
        // Get full history
        let history = self.db.get_node_history(node_id)?;

        // Filter versions that overlap with the window and extract vectors
        let mut vectors: Vec<(i64, Vec<f32>)> = Vec::new();

        for version in history.versions {
            // Check temporal overlap with Valid Time
            if !version.temporal.valid_time().overlaps(&window) {
                continue;
            }

            if let Some(vec) = version.properties.get(property).and_then(|v| v.as_vector()) {
                let timestamp = version.temporal.valid_time().start().wallclock();
                vectors.push((timestamp, vec.to_vec()));
            }
        }

        // Sort by timestamp
        vectors.sort_by_key(|(t, _)| *t);

        // Deduplicate consecutive identical vectors to avoid noise?
        // No, if the user updated it to the same value, it didn't move. Distance is 0.
        // But updates count increases.

        if vectors.len() < 2 {
            return Ok(ThermalReading {
                volatility: 0.0,
                temperature: 0.0,
                updates: vectors.len(),
            });
        }

        let mut total_distance = 0.0;
        // Count transitions, not just points. Updates = points - 1?
        // Let's report total points found as "updates" (versions).
        let update_count = vectors.len();

        for i in 0..vectors.len() - 1 {
            let (_, v1) = &vectors[i];
            let (_, v2) = &vectors[i + 1];

            // Calculate distance
            let dist = euclidean_distance(v1, v2)?;
            total_distance += dist;
        }

        // Calculate duration in seconds
        let duration_secs = if let Some(micros) = window.duration_micros() {
            micros as f32 / 1_000_000.0
        } else {
            // Window is open-ended [start, infinity)
            // Cap at "now"
            let start_micros = window.start().wallclock();
            let end_micros = time::now().wallclock();
            (end_micros - start_micros).max(0) as f32 / 1_000_000.0
        };

        let temperature = if duration_secs > 1e-6 {
            total_distance / duration_secs
        } else {
            0.0
        };

        Ok(ThermalReading {
            volatility: total_distance,
            temperature,
            updates: update_count,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::transaction::WriteOps;
    use crate::core::property::PropertyMapBuilder;
    use crate::index::vector::{DistanceMetric, HnswConfig};

    #[test]
    fn test_thermos_basic_movement() {
        let db = AletheiaDB::new().unwrap();
        // Enable vector index for the property (optional for Thermos but good practice)
        let config = HnswConfig::new(2, DistanceMetric::Euclidean);
        db.enable_vector_index("vec", config).unwrap();

        let t0 = time::now();
        let props = PropertyMapBuilder::new()
            .insert_vector("vec", &[0.0, 0.0])
            .build();
        let node = db.create_node("Point", props).unwrap();

        // 1. Move to [3, 0] (Dist 3)
        // Sleep to ensure measurable time duration
        std::thread::sleep(std::time::Duration::from_millis(10));
        db.write(|tx| {
            tx.update_node(
                node,
                PropertyMapBuilder::new()
                    .insert_vector("vec", &[3.0, 0.0])
                    .build(),
            )
        })
        .unwrap();

        // 2. Move to [3, 4] (Dist 4)
        std::thread::sleep(std::time::Duration::from_millis(10));
        db.write(|tx| {
            tx.update_node(
                node,
                PropertyMapBuilder::new()
                    .insert_vector("vec", &[3.0, 4.0])
                    .build(),
            )
        })
        .unwrap();

        let t_end = time::now();

        let thermos = Thermos::new(&db);
        let window = TimeRange::new(t0, t_end).unwrap();

        let reading = thermos.measure_node(node, window, "vec").unwrap();

        // Path: (0,0) -> (3,0) [dist 3] -> (3,4) [dist 4]
        // Total distance = 7.0
        assert!(
            (reading.volatility - 7.0).abs() < 1e-5,
            "Volatility should be 7.0, got {}",
            reading.volatility
        );

        assert!(reading.temperature > 0.0);
        assert_eq!(reading.updates, 3);
    }

    #[test]
    fn test_thermos_static_node() {
        let db = AletheiaDB::new().unwrap();
        let t0 = time::now();
        let props = PropertyMapBuilder::new()
            .insert_vector("vec", &[1.0, 1.0])
            .build();
        let node = db.create_node("Point", props).unwrap();

        std::thread::sleep(std::time::Duration::from_millis(10));
        let t_end = time::now();

        let thermos = Thermos::new(&db);
        let window = TimeRange::new(t0, t_end).unwrap();

        let reading = thermos.measure_node(node, window, "vec").unwrap();

        assert_eq!(reading.volatility, 0.0);
        assert_eq!(reading.temperature, 0.0);
        assert_eq!(reading.updates, 1); // Only initial creation
    }

    #[test]
    fn test_thermos_no_vector_property() {
        let db = AletheiaDB::new().unwrap();
        let t0 = time::now();
        let props = PropertyMapBuilder::new().insert("name", "Bob").build();
        let node = db.create_node("Person", props).unwrap();

        std::thread::sleep(std::time::Duration::from_millis(10));
        let t_end = time::now();

        let thermos = Thermos::new(&db);
        let window = TimeRange::new(t0, t_end).unwrap();

        // Property "embedding" doesn't exist
        let reading = thermos.measure_node(node, window, "embedding").unwrap();

        assert_eq!(reading.volatility, 0.0);
        assert_eq!(reading.updates, 0);
    }
}
