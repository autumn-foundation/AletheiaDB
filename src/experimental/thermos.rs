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

        let mut total_distance = 0.0;
        let start_time = snapshots.first().unwrap().0;
        let end_time = snapshots.last().unwrap().0;
        let duration = end_time - start_time;

        for i in 0..snapshots.len() - 1 {
            let (_, v1) = &snapshots[i];
            let (_, v2) = &snapshots[i + 1];

            // Calculate distance
            let dist = euclidean_distance(v1, v2)?;
            total_distance += dist;
        }

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

    #[test]
    fn test_thermos_boundary_conditions() {
        // This test targets interval logic mutations (e.g., && -> ||, < -> <=)
        let db = AletheiaDB::new().unwrap();
        db.enable_vector_index("vec", HnswConfig::new(1, DistanceMetric::Euclidean))
            .unwrap();

        let t_base = time::now().wallclock();
        let t_window_start = t_base + 1_000_000; // +1s
        let t_window_end = t_base + 2_000_000; // +2s

        let props = PropertyMapBuilder::new()
            .insert_vector("vec", &[0.0])
            .build();

        // 1. Create node valid strictly BEFORE window (0..1s)
        // Should be excluded by strict inequality logic
        let mut tx = db.write_transaction().unwrap();
        let node = tx
            .create_node_with_valid_time(
                "Node",
                props.clone(),
                Some(crate::core::hlc::HybridTimestamp::new(t_base, 0).unwrap()),
            )
            .unwrap();
        tx.commit().unwrap();

        // Update to close validity at t_window_start
        // Effectively: valid from t_base to t_window_start
        // Actually AletheiaDB updates close previous version at new start time.
        let mut tx = db.write_transaction().unwrap();
        tx.update_node_with_valid_time(
            node,
            props.clone(),
            Some(crate::core::hlc::HybridTimestamp::new(t_window_start, 0).unwrap()),
        )
        .unwrap();
        tx.commit().unwrap();

        // Now we have:
        // v1: valid t_base .. t_window_start
        // v2: valid t_window_start .. inf

        // Window: t_window_start .. t_window_end
        let window = TimeRange::new(
            crate::core::hlc::HybridTimestamp::new(t_window_start, 0).unwrap(),
            crate::core::hlc::HybridTimestamp::new(t_window_end, 0).unwrap(),
        )
        .unwrap();

        let thermos = Thermos::new(&db);
        let reading = thermos.measure_temperature(node, "vec", window).unwrap();

        // Logic check:
        // v1: start(t_base) < window_end(t_window_end) [True]
        //     end(t_window_start) > window_start(t_window_start) [False] -> Strict inequality >
        //     Result: Excluded.
        // v2: start(t_window_start) < window_end [True]
        //     end(MAX) > window_start [True]
        //     Result: Included.

        // If || replaced &&, v1 would be included (True || False = True).
        // If >= replaced >, v1 would be included (end == start).

        // If v1 was included, we'd have 2 snapshots (v1, v2).
        // If excluded, 1 snapshot (v2).
        // Thermos returns default (0.0) if < 2 snapshots.

        assert_eq!(
            reading.update_count, 1,
            "Boundary version should be excluded (killed || and >= mutants)"
        );
        assert_eq!(reading.volatility, 0.0);
    }

    #[test]
    fn test_thermos_math_integrity() {
        // Targets math mutations (+ -> -, += -> -=, etc)
        let db = AletheiaDB::new().unwrap();
        db.enable_vector_index("vec", HnswConfig::new(1, DistanceMetric::Euclidean))
            .unwrap();

        let t0 = time::now();
        let props = PropertyMapBuilder::new()
            .insert_vector("vec", &[0.0])
            .build();

        let ts0 = t0.wallclock();
        // Use manual transaction to control creation timestamp exactly
        let mut tx = db.write_transaction().unwrap();
        let node = tx
            .create_node_with_valid_time(
                "MathNode",
                props,
                Some(crate::core::hlc::HybridTimestamp::new(ts0, 0).unwrap()),
            )
            .unwrap();
        tx.commit().unwrap();

        // v1: [0.0] at t0
        // v2: [3.0] at t0 + 1s. Dist = 3.0
        // v3: [7.0] at t0 + 2s. Dist = 4.0. Total = 7.0.

        std::thread::sleep(std::time::Duration::from_millis(10)); // Ensure distinct TS
        let _t1 = time::now(); // t0 + small epsilon

        // We manually control TS to ensure precise math check isn't flaky
        let ts0 = t0.wallclock();
        let ts1 = ts0 + 1_000_000;
        let ts2 = ts0 + 2_000_000;

        let mut tx = db.write_transaction().unwrap();
        tx.update_node_with_valid_time(
            node,
            PropertyMapBuilder::new()
                .insert_vector("vec", &[3.0])
                .build(),
            Some(crate::core::hlc::HybridTimestamp::new(ts1, 0).unwrap()),
        )
        .unwrap();
        tx.commit().unwrap();

        let mut tx = db.write_transaction().unwrap();
        tx.update_node_with_valid_time(
            node,
            PropertyMapBuilder::new()
                .insert_vector("vec", &[7.0])
                .build(),
            Some(crate::core::hlc::HybridTimestamp::new(ts2, 0).unwrap()),
        )
        .unwrap();
        tx.commit().unwrap();

        let window = TimeRange::new(
            crate::core::hlc::HybridTimestamp::new(ts0, 0).unwrap(),
            crate::core::hlc::HybridTimestamp::new(ts2 + 1, 0).unwrap(),
        )
        .unwrap();

        let thermos = Thermos::new(&db);
        let reading = thermos.measure_temperature(node, "vec", window).unwrap();

        // Volatility = |3-0| + |7-3| = 3 + 4 = 7.0
        // Duration = ts2 - ts0 (wait, first snapshot effective time might be clamped)
        // v1 start = t0 (actually creation time). Let's say t0.
        // effective v1 = max(t0, window_start=ts0) = ts0.
        // v2 start = ts1.
        // v3 start = ts2.
        // Snapshots: (ts0, [0]), (ts1, [3]), (ts2, [7]).
        // Duration = ts2 - ts0 = 2_000_000 us = 2.0s.
        // Temperature = 7.0 / 2.0 = 3.5.

        assert_eq!(reading.update_count, 3);
        assert!(
            (reading.volatility - 7.0).abs() < 1e-5,
            "Volatility math failed"
        );

        // Check temperature math
        // If duration calculation mutated (- to +), temp would be tiny
        // If dist calc mutated, temp would be wrong
        assert!(
            (reading.temperature - 3.5).abs() < 1e-5,
            "Temperature math failed"
        );
    }
}
