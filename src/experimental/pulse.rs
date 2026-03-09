//! Pulse: Semantic Rhythm & Frequency Analysis.
//!
//! "What is the heartbeat of your data?"
//!
//! Pulse measures the rhythm of semantic updates to a node's vector embedding over time.
//! While Thermos measures *how far* a node's meaning shifts, Pulse measures *how often* and
//! *how steadily* those shifts occur.
//!
//! # Concepts
//! - **Update Frequency**: The average time interval between consecutive semantic updates.
//! - **Rhythm Variance**: The variance of those intervals. Low variance means a steady,
//!   predictable heartbeat (e.g., periodic bot updates). High variance indicates bursty,
//!   erratic updates (e.g., intense learning sessions followed by long periods of inactivity).
//!
//! # Example
//! ```rust,no_run
//! use aletheiadb::AletheiaDB;
//! use aletheiadb::experimental::pulse::{Pulse, PulseReading};
//! use aletheiadb::core::temporal::{TimeRange, time};
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let db = AletheiaDB::new()?;
//! # let node_id = db.create_node("N", Default::default())?;
//! let pulse = Pulse::new(&db);
//!
//! let range = TimeRange::new(time::from_secs(0), time::now())?;
//! let reading = pulse.measure_node(node_id, range, "embedding")?;
//!
//! println!("Semantic Beat Count: {}", reading.beat_count);
//! println!("Average Update Frequency: {:.2}s", reading.update_frequency);
//! println!("Rhythm Variance: {:.2}", reading.rhythm_variance);
//! # Ok(())
//! # }
//! ```

use crate::AletheiaDB;
use crate::core::error::Result;
use crate::core::id::NodeId;
use crate::core::temporal::TimeRange;
use crate::core::vector::ops::euclidean_distance;

/// A reading of a node's semantic rhythm.
#[derive(Debug, Clone, Copy)]
pub struct PulseReading {
    /// Number of semantic updates (where the vector value changed) in the window.
    pub beat_count: usize,
    /// Average time in seconds between consecutive semantic updates.
    pub update_frequency: f32,
    /// The variance of the time intervals between semantic updates.
    /// Low variance = steady heartbeat. High variance = bursty/erratic updates.
    pub rhythm_variance: f32,
}

/// The Pulse engine.
pub struct Pulse<'a> {
    db: &'a AletheiaDB,
}

impl<'a> Pulse<'a> {
    /// Create a new Pulse instance.
    pub fn new(db: &'a AletheiaDB) -> Self {
        Self { db }
    }

    /// Measure the semantic rhythm of a node over a time range.
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
    ) -> Result<PulseReading> {
        let history = self.db.get_node_history(node_id)?;

        let mut vectors_with_timestamps: Vec<(i64, Vec<f32>)> = Vec::new();

        for version in history.versions {
            if !version.temporal.valid_time().overlaps(&window) {
                continue;
            }

            if let Some(vec) = version.properties.get(property).and_then(|v| v.as_vector()) {
                let timestamp = version.temporal.valid_time().start().wallclock();
                vectors_with_timestamps.push((timestamp, vec.to_vec()));
            }
        }

        vectors_with_timestamps.sort_by_key(|(t, _)| *t);

        let mut semantic_update_timestamps = Vec::new();

        if !vectors_with_timestamps.is_empty() {
            // First observed vector in the window is a "beat"
            semantic_update_timestamps.push(vectors_with_timestamps[0].0);

            for i in 1..vectors_with_timestamps.len() {
                let (_, prev_vec) = &vectors_with_timestamps[i - 1];
                let (curr_t, curr_vec) = &vectors_with_timestamps[i];

                // Consider it a beat only if the vector actually moved
                let dist = euclidean_distance(prev_vec, curr_vec)?;
                if dist > 1e-5 {
                    semantic_update_timestamps.push(*curr_t);
                }
            }
        }

        let beat_count = semantic_update_timestamps.len();

        if beat_count < 2 {
            return Ok(PulseReading {
                beat_count,
                update_frequency: 0.0,
                rhythm_variance: 0.0,
            });
        }

        let mut intervals = Vec::with_capacity(beat_count - 1);
        for i in 1..beat_count {
            let dt_micros = semantic_update_timestamps[i] - semantic_update_timestamps[i - 1];
            intervals.push(dt_micros as f32 / 1_000_000.0);
        }

        let sum_intervals: f32 = intervals.iter().sum();
        let update_frequency = sum_intervals / intervals.len() as f32;

        let mut sum_squared_diff = 0.0;
        for &interval in &intervals {
            let diff = interval - update_frequency;
            sum_squared_diff += diff * diff;
        }

        let rhythm_variance = if intervals.len() > 1 {
            sum_squared_diff / (intervals.len() as f32 - 1.0)
        } else {
            0.0 // Sample variance is 0 if there's only 1 interval
        };

        Ok(PulseReading {
            beat_count,
            update_frequency,
            rhythm_variance,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::transaction::WriteOps;
    use crate::core::property::PropertyMapBuilder;
    use crate::core::temporal::time;

    #[test]
    fn test_pulse_steady_rhythm() {
        let db = AletheiaDB::new().unwrap();
        let t0 = time::now();

        let props = PropertyMapBuilder::new()
            .insert_vector("vec", &[0.0, 0.0])
            .build();
        let node = db.create_node("Point", props).unwrap();

        // 1. Move to [1, 0] after ~100ms
        std::thread::sleep(std::time::Duration::from_millis(100));
        db.write(|tx| {
            tx.update_node(
                node,
                PropertyMapBuilder::new()
                    .insert_vector("vec", &[1.0, 0.0])
                    .build(),
            )
        })
        .unwrap();

        // 2. Move to [2, 0] after another ~100ms
        std::thread::sleep(std::time::Duration::from_millis(100));
        db.write(|tx| {
            tx.update_node(
                node,
                PropertyMapBuilder::new()
                    .insert_vector("vec", &[2.0, 0.0])
                    .build(),
            )
        })
        .unwrap();

        let t_end = time::now();

        let pulse = Pulse::new(&db);
        let window = TimeRange::new(t0, t_end).unwrap();
        let reading = pulse.measure_node(node, window, "vec").unwrap();

        assert_eq!(reading.beat_count, 3);
        // Expect average frequency to be roughly 0.1 seconds
        assert!(reading.update_frequency > 0.05 && reading.update_frequency < 0.2);
        // Variance should be very low for a steady rhythm
        assert!(reading.rhythm_variance < 0.01);
    }

    #[test]
    fn test_pulse_bursty_rhythm() {
        let db = AletheiaDB::new().unwrap();
        let t0 = time::now();

        let props = PropertyMapBuilder::new()
            .insert_vector("vec", &[0.0, 0.0])
            .build();
        let node = db.create_node("Point", props).unwrap();

        // 1. Long pause, then rapid movement (burst)
        std::thread::sleep(std::time::Duration::from_millis(200));

        db.write(|tx| {
            tx.update_node(
                node,
                PropertyMapBuilder::new()
                    .insert_vector("vec", &[1.0, 0.0])
                    .build(),
            )
        })
        .unwrap();

        std::thread::sleep(std::time::Duration::from_millis(10));

        db.write(|tx| {
            tx.update_node(
                node,
                PropertyMapBuilder::new()
                    .insert_vector("vec", &[2.0, 0.0])
                    .build(),
            )
        })
        .unwrap();

        let t_end = time::now();

        let pulse = Pulse::new(&db);
        let window = TimeRange::new(t0, t_end).unwrap();
        let reading = pulse.measure_node(node, window, "vec").unwrap();

        assert_eq!(reading.beat_count, 3);
        // Variance should be much higher due to the 200ms -> 10ms gap differences
        assert!(reading.rhythm_variance > 0.005);
    }

    #[test]
    fn test_pulse_ignore_static_updates() {
        let db = AletheiaDB::new().unwrap();
        let t0 = time::now();

        let props = PropertyMapBuilder::new()
            .insert_vector("vec", &[1.0, 1.0])
            .build();
        let node = db.create_node("Point", props).unwrap();

        // Update with SAME vector multiple times. These shouldn't count as semantic beats.
        std::thread::sleep(std::time::Duration::from_millis(50));
        db.write(|tx| {
            tx.update_node(
                node,
                PropertyMapBuilder::new()
                    .insert_vector("vec", &[1.0, 1.0])
                    .build(),
            )
        })
        .unwrap();

        std::thread::sleep(std::time::Duration::from_millis(50));
        db.write(|tx| {
            tx.update_node(
                node,
                PropertyMapBuilder::new()
                    .insert_vector("vec", &[1.0, 1.0])
                    .build(),
            )
        })
        .unwrap();

        let t_end = time::now();

        let pulse = Pulse::new(&db);
        let window = TimeRange::new(t0, t_end).unwrap();
        let reading = pulse.measure_node(node, window, "vec").unwrap();

        // Only the initial creation counts as a "beat"
        assert_eq!(reading.beat_count, 1);
        assert_eq!(reading.update_frequency, 0.0);
        assert_eq!(reading.rhythm_variance, 0.0);
    }
}
