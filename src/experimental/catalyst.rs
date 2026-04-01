#![allow(clippy::collapsible_if)]

//! Catalyst: Semantic Catalyst Detector.
//!
//! "Who started the trend?"
//!
//! Catalyst identifies nodes that initiated a global semantic shift. It finds nodes
//! that moved in a certain semantic direction *before* the rest of the graph followed.
//!
//! # Concepts
//! - **Global Shift**: The difference between the global centroid at `time_a` and `time_b`.
//! - **Node Delta**: The difference between a node's vector before the shift and after its first update within the window.
//! - **Earliness**: How close to `time_a` the node's update occurred.
//! - **Catalyst Score**: A combination of alignment with the global shift and earliness.
//!
//! # Example
//! ```rust,no_run
//! use aletheiadb::AletheiaDB;
//! use aletheiadb::experimental::catalyst::CatalystEngine;
//! use aletheiadb::core::temporal::time;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let db = AletheiaDB::new()?;
//! let catalyst = CatalystEngine::new(&db);
//!
//! let t1 = time::now() - 3600 * 1_000_000 * 24 * 7; // Last week
//! let t2 = time::now(); // Now
//!
//! let top_catalysts = catalyst.find_catalysts(t1, t2, "embedding", 10)?;
//! for (node_id, score) in top_catalysts {
//!     println!("Node: {}, Score: {:.4}", node_id, score);
//! }
//! # Ok(())
//! # }
//! ```

use crate::AletheiaDB;
use crate::core::error::{Error, Result};
use crate::core::id::NodeId;
use crate::core::temporal::Timestamp;
use crate::core::vector::ops;
use std::collections::HashSet;

/// The Catalyst Engine for identifying early adopters/trendsetters.
pub struct CatalystEngine<'a> {
    db: &'a AletheiaDB,
}

impl<'a> CatalystEngine<'a> {
    /// Create a new CatalystEngine instance.
    pub fn new(db: &'a AletheiaDB) -> Self {
        Self { db }
    }

    /// Find the top catalysts for a global semantic shift between two time points.
    ///
    /// # Arguments
    /// * `time_a` - The start of the time window.
    /// * `time_b` - The end of the time window.
    /// * `property_name` - The vector property to analyze.
    /// * `limit` - The maximum number of catalysts to return.
    pub fn find_catalysts(
        &self,
        time_a: Timestamp,
        time_b: Timestamp,
        property_name: &str,
        limit: usize,
    ) -> Result<Vec<(NodeId, f32)>> {
        // Collect all node IDs that currently exist.
        let mut all_node_ids = HashSet::new();
        let results = self.db.query().scan(None).execute(self.db)?;
        for row in results {
            let row = row?;
            if let Some(node) = row.entity.as_node() {
                all_node_ids.insert(node.id);
            }
        }

        let node_ids: Vec<_> = all_node_ids.into_iter().collect();

        // 1. Calculate the global shift (Centroid B - Centroid A)
        let centroid_a = self.calculate_centroid_at(&node_ids, time_a, property_name)?;
        let centroid_b = self.calculate_centroid_at(&node_ids, time_b, property_name)?;

        if centroid_a.is_empty() || centroid_b.is_empty() {
            return Err(Error::other("Insufficient data to calculate global shift"));
        }

        if centroid_a.len() != centroid_b.len() {
            return Err(Error::other("Dimension mismatch between time windows"));
        }

        let global_shift: Vec<f32> = centroid_b
            .iter()
            .zip(centroid_a.iter())
            .map(|(b, a)| b - a)
            .collect();

        // 2. Score each node
        let mut node_scores = Vec::new();
        let window_duration = time_b.wallclock() - time_a.wallclock();

        if window_duration <= 0 {
            return Err(Error::other("time_b must be after time_a"));
        }

        for node_id in node_ids {
            if let Ok(history) = self.db.get_node_history(node_id) {
                // Find the node's state just before or at time_a
                let mut state_a_vec: Option<Vec<f32>> = None;
                // Find the first update *within* the window
                let mut first_update_vec: Option<Vec<f32>> = None;
                let mut first_update_time: Option<i64> = None;

                for version in &history.versions {
                    let ts = version.temporal.valid_time().start().wallclock();
                    let vec = version
                        .properties
                        .get(property_name)
                        .and_then(|v| v.as_vector())
                        .map(|v| v.to_vec());

                    if ts <= time_a.wallclock() {
                        if vec.is_some() {
                            state_a_vec = vec;
                        }
                    } else if ts > time_a.wallclock() && ts <= time_b.wallclock() {
                        if first_update_time.is_none() && vec.is_some() {
                            first_update_vec = vec;
                            first_update_time = Some(ts);
                        }
                    }
                }

                if let (Some(vec_a), Some(vec_update), Some(update_time)) =
                    (state_a_vec, first_update_vec, first_update_time)
                {
                    if vec_a.len() != vec_update.len() || vec_a.len() != global_shift.len() {
                        continue;
                    }

                    // Node Delta
                    let node_delta: Vec<f32> = vec_update
                        .iter()
                        .zip(vec_a.iter())
                        .map(|(u, a)| u - a)
                        .collect();

                    // Calculate Cosine Similarity between Node Delta and Global Shift
                    let alignment = ops::cosine_similarity(&node_delta, &global_shift)
                        .unwrap_or(0.0)
                        .max(0.0); // Ignore negative alignment (moving against the trend)

                    // Earliness: 1.0 if it updated exactly at time_a, 0.0 if at time_b
                    let elapsed = update_time - time_a.wallclock();
                    let earliness = 1.0 - (elapsed as f32 / window_duration as f32);
                    let earliness = earliness.clamp(0.0, 1.0);

                    // Combined Score (can weight differently)
                    let score = alignment * earliness;

                    if score > 0.0 {
                        node_scores.push((node_id, score));
                    }
                }
            }
        }

        // 3. Sort and limit
        node_scores.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        node_scores.truncate(limit);

        Ok(node_scores)
    }

    fn calculate_centroid_at(
        &self,
        node_ids: &[NodeId],
        time: Timestamp,
        property_name: &str,
    ) -> Result<Vec<f32>> {
        let mut sum_vector = Vec::new();
        let mut count = 0;

        let nodes_at_time = self.db.get_nodes_at_time(node_ids, time, time)?;

        for (_, node_opt) in nodes_at_time {
            if let Some(node) = node_opt {
                if let Some(prop) = node.get_property(property_name) {
                    if let Some(vec) = prop.as_vector() {
                        if sum_vector.is_empty() {
                            sum_vector = vec![0.0; vec.len()];
                        }

                        if vec.len() == sum_vector.len() {
                            for i in 0..vec.len() {
                                sum_vector[i] += vec[i];
                            }
                            count += 1;
                        }
                    }
                }
            }
        }

        if count > 0 {
            for v in &mut sum_vector {
                *v /= count as f32;
            }
        }

        Ok(sum_vector)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::transaction::WriteOps;
    use crate::core::property::PropertyMapBuilder;

    #[test]
    fn test_find_catalysts() {
        let db = AletheiaDB::new().unwrap();

        // 1. Setup initial state at T0
        let mut n_catalyst = crate::core::id::NodeId::new(0).unwrap();
        let mut n_follower_1 = crate::core::id::NodeId::new(0).unwrap();
        let mut n_follower_2 = crate::core::id::NodeId::new(0).unwrap();

        db.write(|tx| {
            n_catalyst = tx
                .create_node(
                    "Concept",
                    PropertyMapBuilder::new()
                        .insert_vector("vec", &[0.0, 0.0])
                        .build(),
                )
                .unwrap();

            n_follower_1 = tx
                .create_node(
                    "Concept",
                    PropertyMapBuilder::new()
                        .insert_vector("vec", &[0.0, 0.0])
                        .build(),
                )
                .unwrap();

            n_follower_2 = tx
                .create_node(
                    "Concept",
                    PropertyMapBuilder::new()
                        .insert_vector("vec", &[0.0, 0.0])
                        .build(),
                )
                .unwrap();
            Ok::<(), Error>(())
        })
        .unwrap();

        let time_a_val = crate::core::temporal::time::now().wallclock();
        let time_a = crate::core::temporal::Timestamp::new(time_a_val, 0).unwrap();

        // 2. Catalyst moves to [1.0, 1.0] early
        let time_catalyst = crate::core::temporal::Timestamp::new(time_a_val + 50_000, 0).unwrap();
        db.write(|tx| {
            tx.update_node_with_valid_time(
                n_catalyst,
                PropertyMapBuilder::new()
                    .insert_vector("vec", &[1.0, 1.0])
                    .build(),
                Some(time_catalyst),
            )
            .unwrap();
            Ok::<(), Error>(())
        })
        .unwrap();

        // 3. Followers move to [1.0, 1.0] later
        let time_followers =
            crate::core::temporal::Timestamp::new(time_a_val + 100_000, 0).unwrap();
        db.write(|tx| {
            tx.update_node_with_valid_time(
                n_follower_1,
                PropertyMapBuilder::new()
                    .insert_vector("vec", &[1.0, 1.0])
                    .build(),
                Some(time_followers),
            )
            .unwrap();

            tx.update_node_with_valid_time(
                n_follower_2,
                PropertyMapBuilder::new()
                    .insert_vector("vec", &[1.0, 1.0])
                    .build(),
                Some(time_followers),
            )
            .unwrap();
            Ok::<(), Error>(())
        })
        .unwrap();

        let time_b = crate::core::temporal::Timestamp::new(time_a_val + 150_000, 0).unwrap();

        let engine = CatalystEngine::new(&db);
        let catalysts = engine.find_catalysts(time_a, time_b, "vec", 10).unwrap();

        assert!(!catalysts.is_empty());
        // The catalyst should be the top result
        assert_eq!(catalysts[0].0, n_catalyst);
        // The score of the catalyst should be higher than the followers
        let catalyst_score = catalysts.iter().find(|c| c.0 == n_catalyst).unwrap().1;
        let follower_score = catalysts.iter().find(|c| c.0 == n_follower_1).unwrap().1;
        assert!(catalyst_score > follower_score);
    }
}
