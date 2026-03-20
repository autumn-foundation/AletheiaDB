//! Pulsar: Semantic Rhythm & Periodicity Detector.
//!
//! "Does this node have a heartbeat?"
//!
//! Pulsar analyzes the temporal history of an entity to detect rhythmic patterns in its
//! updates. It can identify if a node or a specific property is updated on a regular schedule
//! (e.g., a daily price update, a weekly status report) by computing the variance in the
//! time intervals between changes.
//!
//! # How it works
//! 1. Retrieves the full temporal history of a node.
//! 2. Filters versions where a specific property changed (if requested).
//! 3. Calculates the time intervals ($\Delta t$) between consecutive changes.
//! 4. Computes the Coefficient of Variation (CV = standard deviation / mean).
//! 5. If CV is below a threshold, the updates are classified as `Periodic`.

use crate::{
    AletheiaDB,
    core::{NodeId, Result},
};
use std::time::Duration;

/// The rhythm of an entity's updates.
#[derive(Debug, Clone, PartialEq)]
pub enum Rhythm {
    /// Not enough historical data to determine a rhythm.
    InsufficientData,
    /// Updates occur at regular intervals.
    Periodic {
        /// The average time between updates.
        interval: Duration,
        /// The Coefficient of Variation (CV). Lower means more periodic.
        cv: f64,
    },
    /// Updates are erratic and unpredictable.
    Erratic {
        /// The average time between updates.
        average_interval: Duration,
        /// The Coefficient of Variation (CV).
        cv: f64,
    },
}

/// The Pulsar engine for detecting temporal periodicity.
pub struct Pulsar<'a> {
    db: &'a AletheiaDB,
    /// The threshold for the Coefficient of Variation (CV) below which a series is considered periodic.
    /// Default is 0.2.
    periodicity_threshold: f64,
}

impl<'a> Pulsar<'a> {
    /// Creates a new Pulsar engine.
    pub fn new(db: &'a AletheiaDB) -> Self {
        Self {
            db,
            periodicity_threshold: 0.2,
        }
    }

    /// Sets the Coefficient of Variation (CV) threshold for periodicity.
    /// A lower threshold requires stricter regularity.
    pub fn with_threshold(mut self, threshold: f64) -> Self {
        self.periodicity_threshold = threshold;
        self
    }

    /// Detects the rhythm of overall updates to a node.
    pub fn detect_node_rhythm(&self, node_id: NodeId) -> Result<Rhythm> {
        let history = self.db.get_node_history(node_id)?;

        let timestamps: Vec<i64> = history
            .versions
            .iter()
            .map(|v| v.temporal.valid_time().start().wallclock())
            .collect();

        Ok(self.analyze_timestamps(&timestamps))
    }

    /// Detects the rhythm of updates to a specific property of a node.
    pub fn detect_property_rhythm(&self, node_id: NodeId, property_key: &str) -> Result<Rhythm> {
        let history = self.db.get_node_history(node_id)?;

        let mut timestamps = Vec::new();
        let mut last_value = None;

        for version in &history.versions {
            let current_value = version.properties.get(property_key);

            // Only record timestamp if the property value actually changed
            if current_value != last_value.as_ref() {
                timestamps.push(version.temporal.valid_time().start().wallclock());
                last_value = current_value.cloned();
            }
        }

        Ok(self.analyze_timestamps(&timestamps))
    }

    fn analyze_timestamps(&self, timestamps: &[i64]) -> Rhythm {
        if timestamps.len() < 3 {
            return Rhythm::InsufficientData;
        }

        // Calculate intervals
        let mut intervals = Vec::with_capacity(timestamps.len() - 1);
        for i in 1..timestamps.len() {
            intervals.push(timestamps[i].saturating_sub(timestamps[i - 1]) as f64);
        }

        // Calculate mean
        let sum: f64 = intervals.iter().sum();
        let mean = sum / intervals.len() as f64;

        if mean == 0.0 {
            // All updates happened at the exact same microsecond (unlikely, but possible)
            return Rhythm::Periodic {
                interval: Duration::from_micros(0),
                cv: 0.0,
            };
        }

        // Calculate standard deviation
        let variance: f64 = intervals
            .iter()
            .map(|&interval| {
                let diff = interval - mean;
                diff * diff
            })
            .sum::<f64>()
            / intervals.len() as f64;

        let std_dev = variance.sqrt();
        let cv = std_dev / mean;
        let avg_duration = Duration::from_micros(mean as u64);

        if cv <= self.periodicity_threshold {
            Rhythm::Periodic {
                interval: avg_duration,
                cv,
            }
        } else {
            Rhythm::Erratic {
                average_interval: avg_duration,
                cv,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::temporal::time;
    use crate::{WriteOps, properties};

    #[test]
    fn test_pulsar_erratic_updates() -> Result<()> {
        let db = AletheiaDB::new()?;

        // Use backdated transactions to simulate history
        let start_time = time::now();

        // Initial creation
        let node_id = db.write(|tx| {
            tx.create_node_with_valid_time(
                "Stock",
                properties! { "price" => 100 },
                Some(start_time),
            )
        })?;

        // Erratic updates
        let t2 = crate::core::hlc::HybridTimestamp::new(start_time.wallclock() as i64 + 10_000, 0)
            .unwrap();
        db.write(|tx| {
            tx.update_node_with_valid_time(node_id, properties! { "price" => 105 }, Some(t2))
        })?;

        let t3 = crate::core::hlc::HybridTimestamp::new(t2.wallclock() as i64 + 50_000, 0).unwrap();
        db.write(|tx| {
            tx.update_node_with_valid_time(node_id, properties! { "price" => 110 }, Some(t3))
        })?;

        let t4 = crate::core::hlc::HybridTimestamp::new(t3.wallclock() as i64 + 5_000, 0).unwrap();
        db.write(|tx| {
            tx.update_node_with_valid_time(node_id, properties! { "price" => 108 }, Some(t4))
        })?;

        let pulsar = Pulsar::new(&db);
        let rhythm = pulsar.detect_property_rhythm(node_id, "price")?;

        match rhythm {
            Rhythm::Erratic { .. } => {} // Expected
            _ => panic!("Expected erratic rhythm, got {:?}", rhythm),
        }

        Ok(())
    }

    #[test]
    fn test_pulsar_periodic_updates() -> Result<()> {
        let db = AletheiaDB::new()?;

        let base_time = time::now();
        let interval = 86_400_000; // 1 day in microseconds

        let node_id = db.write(|tx| {
            tx.create_node_with_valid_time(
                "Server",
                properties! { "status" => "Ok" },
                Some(base_time),
            )
        })?;

        // Periodic updates every ~1 day
        for i in 1..=5 {
            // Add a tiny bit of noise (+- 100 microseconds)
            let noise: i64 = if i % 2 == 0 { 50 } else { -50 };
            let mut t = crate::core::hlc::HybridTimestamp::new(
                base_time.wallclock() as i64 + (interval as i64 * i as i64),
                0,
            )
            .unwrap();
            if noise > 0 {
                t = crate::core::hlc::HybridTimestamp::new(t.wallclock() as i64 + noise, 0)
                    .unwrap();
            } else {
                t = crate::core::hlc::HybridTimestamp::new(t.wallclock() - noise.abs(), 0).unwrap();
            }

            db.write(|tx| {
                tx.update_node_with_valid_time(
                    node_id,
                    properties! { "status" => format!("Ok_{}", i) },
                    Some(t),
                )
            })?;
        }

        let pulsar = Pulsar::new(&db);
        let rhythm = pulsar.detect_property_rhythm(node_id, "status")?;

        match rhythm {
            Rhythm::Periodic {
                interval: avg_interval,
                cv,
            } => {
                assert!(cv < 0.05); // Very periodic
                assert!((avg_interval.as_micros() as i128 - interval as i128).abs() < 200);
            }
            _ => panic!("Expected periodic rhythm, got {:?}", rhythm),
        }

        Ok(())
    }
}
