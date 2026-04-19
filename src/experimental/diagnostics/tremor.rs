//! Tremor: Semantic Earthquake Detector.
//!
//! "Did the underlying meaning of our data just shift?"
//!
//! Tremor detects macro-level semantic shifts across the entire graph (or a subset)
//! by comparing the global distribution of vectors between two points in time.
//! It is useful for finding "paradigm shifts" where concepts drift apart or cluster together
//! over time.
//!
//! # Concepts
//! - **Centroid**: The average vector of a set of nodes at a specific time.
//! - **Shift Magnitude**: The Euclidean distance between centroids of two time windows.
//! - **Tremor Score**: A normalized score representing the severity of the semantic shift.
//!
//! # Example
//! ```rust,no_run
//! use aletheiadb::AletheiaDB;
//! use aletheiadb::experimental::tremor::TremorEngine;
//! use aletheiadb::core::temporal::time;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let db = AletheiaDB::new()?;
//! let tremor = TremorEngine::new(&db);
//!
//! let t1 = time::now() - 3600 * 1_000_000 * 24 * 7; // Last week
//! let t2 = time::now(); // Now
//!
//! let score = tremor.detect_shift(t1, t2, "embedding")?;
//! println!("Semantic Shift Magnitude: {:.4}", score.shift_magnitude);
//! # Ok(())
//! # }
//! ```

use crate::AletheiaDB;
use crate::core::error::{Error, Result};
use crate::core::temporal::Timestamp;
use std::collections::HashSet;

/// The result of a Tremor analysis.
#[derive(Debug, Clone)]
pub struct TremorScore {
    /// The centroid vector in the first time window.
    pub centroid_a: Vec<f32>,
    /// The centroid vector in the second time window.
    pub centroid_b: Vec<f32>,
    /// The Euclidean distance between the two centroids.
    pub shift_magnitude: f32,
    /// Number of nodes analyzed in window A.
    pub nodes_analyzed_a: usize,
    /// Number of nodes analyzed in window B.
    pub nodes_analyzed_b: usize,
}

/// The Tremor Engine for detecting global semantic shifts.
pub struct TremorEngine<'a> {
    db: &'a AletheiaDB,
}

impl<'a> TremorEngine<'a> {
    /// Create a new TremorEngine instance.
    pub fn new(db: &'a AletheiaDB) -> Self {
        Self { db }
    }

    /// Detect the global semantic shift between two points in time.
    ///
    /// # Arguments
    /// * `time_a` - The first (historical) time point.
    /// * `time_b` - The second (recent) time point.
    /// * `property_name` - The vector property to analyze.
    pub fn detect_shift(
        &self,
        time_a: Timestamp,
        time_b: Timestamp,
        property_name: &str,
    ) -> Result<TremorScore> {
        // Collect all node IDs that currently exist.
        // We do a full scan of the current graph to find nodes, then fetch their historical state.
        // In a real production scenario, this might be optimized to only scan active nodes
        // during those specific time windows.

        let mut all_node_ids = HashSet::new();
        let results = self.db.query().scan(None).execute(self.db)?;
        for row in results {
            let row = row?;
            if let Some(node) = row.entity.as_node() {
                all_node_ids.insert(node.id);
            }
        }

        let node_ids: Vec<_> = all_node_ids.into_iter().collect();

        // Query state at time A
        let (centroid_a, count_a) = self.calculate_centroid_at(&node_ids, time_a, property_name)?;

        // Query state at time B
        let (centroid_b, count_b) = self.calculate_centroid_at(&node_ids, time_b, property_name)?;

        if count_a == 0 || count_b == 0 {
            return Err(Error::other(
                "Insufficient data to calculate centroids (0 valid vectors found in one or both time windows).",
            ));
        }

        if centroid_a.len() != centroid_b.len() {
            return Err(Error::other("Dimension mismatch between time windows."));
        }

        let shift_magnitude = centroid_a
            .iter()
            .zip(centroid_b.iter())
            .map(|(a, b)| (a - b).powi(2))
            .sum::<f32>()
            .sqrt();

        Ok(TremorScore {
            centroid_a,
            centroid_b,
            shift_magnitude,
            nodes_analyzed_a: count_a,
            nodes_analyzed_b: count_b,
        })
    }

    fn calculate_centroid_at(
        &self,
        node_ids: &[crate::core::id::NodeId],
        time: Timestamp,
        property_name: &str,
    ) -> Result<(Vec<f32>, usize)> {
        let mut sum_vector = Vec::new();
        let mut count = 0;

        // Batch fetch nodes
        // Use 'time' for BOTH valid_time and transaction_time to see what the state
        // actually was at that point in the past.
        let nodes_at_time = self.db.get_nodes_at_time(node_ids, time, time)?;

        for (_, node_opt) in nodes_at_time {
            if let Some(node) = node_opt
                && let Some(prop) = node.get_property(property_name)
                && let Some(vec) = prop.as_vector()
            {
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

        if count > 0 {
            for v in &mut sum_vector {
                *v /= count as f32;
            }
        }

        Ok((sum_vector, count))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::transaction::WriteOps;
    use crate::core::property::PropertyMapBuilder;

    #[test]
    fn test_tremor_shift_detection() {
        let db = AletheiaDB::new().unwrap();

        // Ensure some initial time passes so tx time is distinctly after db creation
        std::thread::sleep(std::time::Duration::from_millis(10));

        // Time A: Nodes are clustered around [0, 0]
        let mut n1 = crate::core::id::NodeId::new(0).unwrap();
        let mut n2 = crate::core::id::NodeId::new(0).unwrap();

        db.write(|tx| {
            n1 = tx
                .create_node(
                    "Concept",
                    PropertyMapBuilder::new()
                        .insert_vector("vec", &[-0.1, 0.0])
                        .build(),
                )
                .unwrap();

            n2 = tx
                .create_node(
                    "Concept",
                    PropertyMapBuilder::new()
                        .insert_vector("vec", &[0.1, 0.0])
                        .build(),
                )
                .unwrap();
            Ok::<(), Error>(())
        })
        .unwrap();

        std::thread::sleep(std::time::Duration::from_millis(10));
        let time_a = crate::core::temporal::time::now();
        std::thread::sleep(std::time::Duration::from_millis(50));

        // Time B: Nodes drift significantly to [10, 10]
        db.write(|tx| {
            tx.update_node(
                n1,
                PropertyMapBuilder::new()
                    .insert_vector("vec", &[9.9, 10.0])
                    .build(),
            )
            .unwrap();

            tx.update_node(
                n2,
                PropertyMapBuilder::new()
                    .insert_vector("vec", &[10.1, 10.0])
                    .build(),
            )
            .unwrap();
            Ok::<(), Error>(())
        })
        .unwrap();

        let time_b = crate::core::temporal::time::now();

        let engine = TremorEngine::new(&db);
        let score = engine.detect_shift(time_a, time_b, "vec").unwrap();

        // Centroid A should be near [0, 0]
        assert!((score.centroid_a[0] - 0.0).abs() < 0.1);
        assert!((score.centroid_a[1] - 0.0).abs() < 0.1);

        // Centroid B should be near [10, 10]
        assert!((score.centroid_b[0] - 10.0).abs() < 0.1);
        assert!((score.centroid_b[1] - 10.0).abs() < 0.1);

        // Magnitude should be sqrt(10^2 + 10^2) = ~14.14
        assert!((score.shift_magnitude - 14.14).abs() < 0.1);

        assert_eq!(score.nodes_analyzed_a, 2);
        assert_eq!(score.nodes_analyzed_b, 2);
    }

    #[test]
    fn test_tremor_no_shift() {
        let db = AletheiaDB::new().unwrap();

        // Ensure some initial time passes so tx time is distinctly after db creation
        std::thread::sleep(std::time::Duration::from_millis(10));

        // Time A: Nodes are clustered around [5, 5]
        let mut n1 = crate::core::id::NodeId::new(0).unwrap();

        db.write(|tx| {
            n1 = tx
                .create_node(
                    "Concept",
                    PropertyMapBuilder::new()
                        .insert_vector("vec", &[5.0, 5.0])
                        .build(),
                )
                .unwrap();
            Ok::<(), Error>(())
        })
        .unwrap();

        std::thread::sleep(std::time::Duration::from_millis(10));
        let time_a = crate::core::temporal::time::now();
        std::thread::sleep(std::time::Duration::from_millis(50));

        // Time B: Node stays at [5, 5] (we update a non-vector property to create a version)
        db.write(|tx| {
            tx.update_node(
                n1,
                PropertyMapBuilder::new()
                    .insert_vector("vec", &[5.0, 5.0])
                    .insert("status", "unchanged")
                    .build(),
            )
            .unwrap();
            Ok::<(), Error>(())
        })
        .unwrap();

        let time_b = crate::core::temporal::time::now();

        let engine = TremorEngine::new(&db);
        let score = engine.detect_shift(time_a, time_b, "vec").unwrap();

        // Magnitude should be 0
        assert!(score.shift_magnitude < 0.01);
    }
}
