//! Pulse: Semantic Activity Rate Engine.
//!
//! "How actively is this part of the graph changing right now?"
//!
//! Pulse measures the rate of mutation for a given node or subgraph over a
//! specific time window. It acts as a "heartbeat" monitor for data, identifying
//! concepts that are currently "hot" or "trending" based on how frequently
//! they are updated.
//!
//! # Concepts
//! - **Activity Rate**: The number of modifications a node undergoes per second
//!   within a specified time window.
//! - **Time Window**: A defined `TimeRange` over which the pulse is calculated.
//!
//! # Example
//! ```rust,no_run
//! use aletheiadb::AletheiaDB;
//! use aletheiadb::experimental::pulse::PulseEngine;
//! use aletheiadb::core::temporal::{time, TimeRange};
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let db = AletheiaDB::new()?;
//! # let node_id = db.create_node("Node", Default::default())?;
//!
//! let engine = PulseEngine::new(&db);
//!
//! // Calculate pulse over the last hour
//! let now = time::now();
//! let one_hour_ago = now - 3600 * 1_000_000;
//! let window = TimeRange::new(one_hour_ago, now)?;
//!
//! let pulse = engine.calculate_pulse(node_id, window)?;
//! println!("Node Activity Rate: {} modifications/second", pulse);
//! # Ok(())
//! # }
//! ```

use crate::AletheiaDB;
use crate::core::error::Result;
use crate::core::id::NodeId;
use crate::core::temporal::TimeRange;

/// The Pulse Engine for measuring activity rates.
pub struct PulseEngine<'a> {
    db: &'a AletheiaDB,
}

impl<'a> PulseEngine<'a> {
    /// Create a new PulseEngine instance.
    pub fn new(db: &'a AletheiaDB) -> Self {
        Self { db }
    }

    /// Calculate the pulse (activity rate) of a specific node within a time window.
    ///
    /// The pulse is defined as the number of modifications (new versions)
    /// created within the specified `window` divided by the duration of the
    /// window in seconds.
    ///
    /// # Arguments
    ///
    /// * `node_id` - The node to analyze.
    /// * `window` - The time range over which to calculate the pulse.
    ///
    /// # Returns
    ///
    /// The activity rate in modifications per second (Hz).
    pub fn calculate_pulse(&self, node_id: NodeId, window: TimeRange) -> Result<f32> {
        let duration_micros = window.duration_micros().unwrap_or(0);
        if duration_micros == 0 {
            return Ok(0.0);
        }

        let history = self.db.get_node_history(node_id)?;
        let mut modifications = 0;

        let window_start = window.start().wallclock();
        let window_end = window.end().wallclock();

        for version in history.versions {
            // We measure activity by when it was recorded (transaction time).
            let tx_time = version.temporal.transaction_time().start().wallclock();

            if tx_time >= window_start && tx_time <= window_end {
                modifications += 1;
            }
        }

        let duration_secs = duration_micros as f32 / 1_000_000.0;
        let pulse = modifications as f32 / duration_secs;

        Ok(pulse)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::transaction::WriteOps;
    use crate::core::property::PropertyMapBuilder;
    use crate::core::temporal::time;

    #[test]
    fn test_pulse_active_node() {
        let db = AletheiaDB::new().unwrap();

        // 1. Create a node
        let node_id = {
            let mut tx = db.write_transaction().unwrap();
            let props = PropertyMapBuilder::new().insert("status", "init").build();
            let id = tx.create_node("Node", props).unwrap();
            tx.commit().unwrap();
            id
        };

        // Capture window start time
        let window_start = time::now();
        std::thread::sleep(std::time::Duration::from_millis(10));

        // 2. Perform 5 modifications
        for i in 0..5 {
            let mut tx = db.write_transaction().unwrap();
            let props = PropertyMapBuilder::new().insert("status", i as i64).build();
            tx.update_node(node_id, props).unwrap();
            tx.commit().unwrap();
            std::thread::sleep(std::time::Duration::from_millis(5)); // ensure time elapses between commits
        }

        std::thread::sleep(std::time::Duration::from_millis(10));
        let window_end = time::now();

        // 3. Calculate pulse
        let engine = PulseEngine::new(&db);
        let window = TimeRange::new(window_start, window_end).unwrap();
        let pulse = engine.calculate_pulse(node_id, window).unwrap();

        let duration_secs =
            (window_end.wallclock() - window_start.wallclock()) as f32 / 1_000_000.0;
        let expected_pulse = 5.0 / duration_secs; // 5 modifications

        // Pulse should be close to 5.0 / duration_secs
        assert!(
            (pulse - expected_pulse).abs() < 0.1,
            "Expected pulse ~{}, got {}",
            expected_pulse,
            pulse
        );
        assert!(pulse > 0.0);
    }

    #[test]
    fn test_pulse_static_node() {
        let db = AletheiaDB::new().unwrap();

        // 1. Create a node
        let node_id = {
            let mut tx = db.write_transaction().unwrap();
            let props = PropertyMapBuilder::new().insert("status", "init").build();
            let id = tx.create_node("Node", props).unwrap();
            tx.commit().unwrap();
            id
        };

        // Capture window after creation
        std::thread::sleep(std::time::Duration::from_millis(10));
        let window_start = time::now();
        std::thread::sleep(std::time::Duration::from_millis(50));
        let window_end = time::now();

        // 2. Calculate pulse
        let engine = PulseEngine::new(&db);
        let window = TimeRange::new(window_start, window_end).unwrap();
        let pulse = engine.calculate_pulse(node_id, window).unwrap();

        // Zero modifications in the window
        assert_eq!(pulse, 0.0);
    }
}
