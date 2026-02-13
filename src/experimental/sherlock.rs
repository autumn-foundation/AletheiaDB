//! Sherlock: Temporal Pattern Matching Engine.
//!
//! "It is a capital mistake to theorize before one has data."
//!
//! Sherlock allows you to define "Mysteries" (Temporal Patterns) and investigate
//! the graph history to find "Deductions" (Matches).
//!
//! It answers questions like:
//! - "Find all instances where a node's status changed from 'OK' to 'Warning' and then to 'Error' within 5 minutes."
//! - "Did a User log in and then immediately delete a file?"
//!
//! # Concepts
//! - **Clue**: A specific condition to look for (e.g., Property Change).
//! - **Mystery**: A sequence of Clues with time constraints.
//! - **Deduction**: A concrete sequence of events that matches the Mystery.

use crate::AletheiaDB;
use crate::core::id::NodeId;
use crate::core::property::PropertyValue;
use crate::core::temporal::Timestamp;
use crate::utils::error::Result;
use std::time::Duration;

/// A Clue represents a specific event or state change to look for.
#[derive(Debug, Clone)]
pub enum Clue {
    /// A property has a specific value at this version.
    /// If value is None, matches any existing value for that key.
    PropertyState {
        /// The property key to check.
        key: String,
        /// The expected value (None for existence check).
        value: Option<PropertyValue>,
    },
    // Future: Edge addition/removal, etc.
}

/// A Mystery defines the pattern to search for.
#[derive(Debug, Clone)]
pub struct Mystery {
    /// The sequence of clues to find.
    pub clues: Vec<Clue>,
    /// Maximum time allowed between the first and last clue.
    pub time_window: Duration,
}

impl Mystery {
    /// Create a new Mystery builder.
    pub fn new(time_window: Duration) -> Self {
        Self {
            clues: Vec::new(),
            time_window,
        }
    }

    /// Add a clue to the sequence.
    pub fn add_clue(mut self, clue: Clue) -> Self {
        self.clues.push(clue);
        self
    }
}

/// A Deduction represents a successful match.
#[derive(Debug, Clone)]
pub struct Deduction {
    /// The node where the pattern was found.
    pub node_id: NodeId,
    /// The timestamps of each matched clue.
    pub event_times: Vec<Timestamp>,
}

/// The Sherlock Engine.
pub struct Sherlock<'a> {
    db: &'a AletheiaDB,
}

impl<'a> Sherlock<'a> {
    /// Create a new Sherlock instance.
    pub fn new(db: &'a AletheiaDB) -> Self {
        Self { db }
    }

    /// Investigate a specific node for the given Mystery.
    pub fn investigate(&self, node_id: NodeId, mystery: &Mystery) -> Result<Vec<Deduction>> {
        let history = self.db.get_node_history(node_id)?;
        let mut deductions = Vec::new();

        // If no clues, nothing to find.
        if mystery.clues.is_empty() {
            return Ok(deductions);
        }

        // We need to find a sequence of versions that match the clues.
        // This is a simplified backtracking or iterative search.
        // Since history is linear time-wise (transaction time), but we care about valid time
        // for temporal pattern matching, we MUST sort by valid time.
        // Otherwise, backdated updates could break detection.

        // Clone and sort versions by valid_time start
        let mut versions = history.versions.clone();
        versions.sort_by_key(|v| v.temporal.valid_time().start());

        // Strategy: Find all occurrences of Clue 0.
        // For each, look ahead for Clue 1, then Clue 2...
        // Constraints:
        // 1. Order must be preserved (v[i].time < v[j].time).
        // 2. Total duration (v[last].time - v[first].time) <= time_window.

        for (i, version) in versions.iter().enumerate() {
            if self.matches_clue(version, &mystery.clues[0]) {
                // Found potential start.
                let mut current_sequence = vec![version.temporal.valid_time().start()];
                let start_time = current_sequence[0];
                let mut current_version_idx = i;
                let mut matched_so_far = true;

                for next_clue_idx in 1..mystery.clues.len() {
                    let mut found_next = false;
                    let prev_event_time = *current_sequence.last().unwrap();

                    // Search forward from current_version_idx + 1
                    for j in (current_version_idx + 1)..versions.len() {
                        let candidate = &versions[j];
                        let candidate_time = candidate.temporal.valid_time().start();

                        // Enforce strictly increasing time (if required) or just monotonic?
                        // "Sequence of events" usually implies strict ordering if distinct events.
                        // But let's assume valid time is monotonic since we sorted.
                        // We still check strictly greater to avoid matching the same instant if multiple versions exist?
                        // Or if we want strict temporal progression.
                        if candidate_time <= prev_event_time {
                            continue;
                        }

                        // Check time window constraint relative to START
                        let elapsed_micros = candidate_time.wallclock() - start_time.wallclock();
                        if elapsed_micros > mystery.time_window.as_micros() as i64 {
                            // Exceeded window. Since versions are sorted by valid time,
                            // all subsequent versions will also exceed the window.
                            // So we can safely break this inner loop.
                            break;
                        }

                        if self.matches_clue(candidate, &mystery.clues[next_clue_idx]) {
                            current_sequence.push(candidate_time);
                            current_version_idx = j;
                            found_next = true;
                            break; // Move to next clue
                        }
                    }

                    if !found_next {
                        matched_so_far = false;
                        break;
                    }
                }

                if matched_so_far {
                    deductions.push(Deduction {
                        node_id,
                        event_times: current_sequence,
                    });
                }
            }
        }

        Ok(deductions)
    }

    // Helper: Does a version match a clue?
    fn matches_clue(&self, version: &crate::core::history::VersionInfo, clue: &Clue) -> bool {
        match clue {
            Clue::PropertyState { key, value } => {
                if let Some(prop_val) = version.properties.get(key) {
                    if let Some(target_val) = value {
                        prop_val == target_val
                    } else {
                        true // Key exists, value doesn't matter
                    }
                } else {
                    false // Key missing
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::transaction::WriteOps;
    use crate::core::hlc::HybridTimestamp;
    use crate::core::property::PropertyMapBuilder;
    use crate::core::temporal::time;

    #[test]
    fn test_sherlock_detects_sequence() {
        let db = AletheiaDB::new().unwrap();
        let _start_time = time::now();

        // 1. Create Node (Status: Pending)
        let props = PropertyMapBuilder::new()
            .insert("status", "Pending")
            .build();
        let node_id = db.create_node("Order", props).unwrap();

        // 2. Update to "Shipped" (after 10ms)
        std::thread::sleep(std::time::Duration::from_millis(10));
        db.write(|tx| {
            let p = PropertyMapBuilder::new()
                .insert("status", "Shipped")
                .build();
            tx.update_node(node_id, p)
        })
        .unwrap();

        // 3. Update to "Delivered" (after 10ms)
        std::thread::sleep(std::time::Duration::from_millis(10));
        db.write(|tx| {
            let p = PropertyMapBuilder::new()
                .insert("status", "Delivered")
                .build();
            tx.update_node(node_id, p)
        })
        .unwrap();

        let sherlock = Sherlock::new(&db);

        // Mystery: Pending -> Delivered (skipping Shipped is allowed if order preserved? No, we just look for A then B)
        // Let's look for Pending -> Shipped -> Delivered
        let mystery = Mystery::new(Duration::from_secs(1))
            .add_clue(Clue::PropertyState {
                key: "status".to_string(),
                value: Some(PropertyValue::from("Pending")),
            })
            .add_clue(Clue::PropertyState {
                key: "status".to_string(),
                value: Some(PropertyValue::from("Shipped")),
            })
            .add_clue(Clue::PropertyState {
                key: "status".to_string(),
                value: Some(PropertyValue::from("Delivered")),
            });

        let results = sherlock.investigate(node_id, &mystery).unwrap();

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].event_times.len(), 3);
    }

    #[test]
    fn test_sherlock_time_window_constraint() {
        let db = AletheiaDB::new().unwrap();

        // 1. Create Node (State A)
        let props = PropertyMapBuilder::new().insert("state", "A").build();
        let node_id = db.create_node("Machine", props).unwrap();

        // 2. Wait 100ms
        std::thread::sleep(std::time::Duration::from_millis(100));

        // 3. Update to State B
        db.write(|tx| {
            let p = PropertyMapBuilder::new().insert("state", "B").build();
            tx.update_node(node_id, p)
        })
        .unwrap();

        let sherlock = Sherlock::new(&db);

        // Mystery: A -> B within 10ms (Should Fail)
        let impossible_mystery = Mystery::new(Duration::from_millis(10))
            .add_clue(Clue::PropertyState {
                key: "state".to_string(),
                value: Some(PropertyValue::from("A")),
            })
            .add_clue(Clue::PropertyState {
                key: "state".to_string(),
                value: Some(PropertyValue::from("B")),
            });

        let results = sherlock.investigate(node_id, &impossible_mystery).unwrap();
        assert!(
            results.is_empty(),
            "Should not match due to time constraint"
        );

        // Mystery: A -> B within 500ms (Should Pass)
        let possible_mystery = Mystery::new(Duration::from_millis(500))
            .add_clue(Clue::PropertyState {
                key: "state".to_string(),
                value: Some(PropertyValue::from("A")),
            })
            .add_clue(Clue::PropertyState {
                key: "state".to_string(),
                value: Some(PropertyValue::from("B")),
            });

        let results_pass = sherlock.investigate(node_id, &possible_mystery).unwrap();
        assert_eq!(results_pass.len(), 1, "Should match within 500ms");
    }
}
