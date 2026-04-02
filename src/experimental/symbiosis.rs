//! # Context
//! Symbiosis: Semantic Co-evolution Detector.
//!
//! "Are these two concepts evolving together?"
//!
//! Symbiosis measures the alignment of vector trajectories between two nodes over time.
//! If Node A and Node B frequently change their semantic embeddings in similar directions,
//! they have high symbiosis (co-evolution). If they move independently or oppositely,
//! their symbiosis is low or negative.
//!
//! # Details
//! - **Displacement**: The vector change between two consecutive versions of a node.
//! - **Co-evolution Score**: The average cosine similarity between displacement vectors
//!   of two nodes during parallel time windows.
//!
//! # Examples
//! ```rust,no_run
//! // [dependencies]
//! // aletheiadb = { version = "0.1", features = ["nova"] }
//!
//! # #[cfg(feature = "nova")]
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! use aletheiadb::AletheiaDB;
//! use aletheiadb::experimental::symbiosis::Symbiosis;
//! use aletheiadb::core::temporal::{TimeRange, time};
//!
//! let db = AletheiaDB::new()?;
//! # let node_a = db.create_node("Concept", Default::default())?;
//! # let node_b = db.create_node("Concept", Default::default())?;
//! let symbiosis = Symbiosis::new(&db);
//!
//! let range = TimeRange::new(time::from_secs(0), time::now())?;
//! let score = symbiosis.measure_co_evolution(node_a, node_b, range, "embedding")?;
//!
//! println!("Co-evolution Score: {:.4}", score);
//! # Ok(())
//! # }
//! # #[cfg(not(feature = "nova"))]
//! # fn main() {}
//! ```

use crate::AletheiaDB;
use crate::core::error::Error;
use crate::core::error::Result;
use crate::core::error::VectorError;
use crate::core::id::NodeId;
use crate::core::temporal::TimeRange;
use crate::core::vector::ops::cosine_similarity;
use std::collections::BTreeMap;

/// The Symbiosis engine.
pub struct Symbiosis<'a> {
    db: &'a AletheiaDB,
}

impl<'a> Symbiosis<'a> {
    /// Create a new Symbiosis instance.
    pub fn new(db: &'a AletheiaDB) -> Self {
        Self { db }
    }

    /// Measure the co-evolution score between two nodes over a time range.
    ///
    /// # Arguments
    /// * `node_a` - The first node.
    /// * `node_b` - The second node.
    /// * `window` - The time range to analyze.
    /// * `property` - The name of the vector property to track.
    pub fn measure_co_evolution(
        &self,
        node_a: NodeId,
        node_b: NodeId,
        window: TimeRange,
        property: &str,
    ) -> Result<f32> {
        let extract_snapshots = |node: NodeId| -> Result<BTreeMap<i64, Vec<f32>>> {
            let history = self.db.get_node_history(node)?;
            let mut snapshots = BTreeMap::new();

            for version in history.versions {
                let valid_time = version.temporal.valid_time();

                if !valid_time.overlaps(&window) {
                    continue;
                }

                if let Some(vec) = version.properties.get(property).and_then(|v| v.as_vector()) {
                    let timestamp = valid_time.start().wallclock();
                    snapshots.insert(timestamp, vec.to_vec());
                }
            }
            Ok(snapshots)
        };

        let snaps_a = extract_snapshots(node_a)?;
        let snaps_b = extract_snapshots(node_b)?;

        if snaps_a.len() < 2 || snaps_b.len() < 2 {
            return Ok(0.0);
        }

        // Align snapshots by taking the union of timestamps, to compute differences
        let mut all_times: Vec<i64> = snaps_a.keys().chain(snaps_b.keys()).copied().collect();
        all_times.sort_unstable();
        all_times.dedup();

        let mut current_a = match snaps_a.iter().next() {
            Some((_, v)) => v.clone(),
            None => return Ok(0.0),
        };
        let mut current_b = match snaps_b.iter().next() {
            Some((_, v)) => v.clone(),
            None => return Ok(0.0),
        };

        let mut total_score = 0.0;
        let mut valid_steps = 0;

        for &t in &all_times {
            let mut moved_a = false;
            let mut moved_b = false;
            let mut next_a = current_a.clone();
            let mut next_b = current_b.clone();

            if let Some(vec) = snaps_a.get(&t) {
                next_a = vec.clone();
                moved_a = true;
            }

            if let Some(vec) = snaps_b.get(&t) {
                next_b = vec.clone();
                moved_b = true;
            }

            if moved_a || moved_b {
                // Calculate displacements
                if current_a.len() != next_a.len() || current_b.len() != next_b.len() {
                    return Err(Error::Vector(VectorError::DimensionMismatch {
                        expected: current_a.len(),
                        actual: next_a.len(),
                    }));
                }

                let disp_a: Vec<f32> = next_a
                    .iter()
                    .zip(current_a.iter())
                    .map(|(n, c)| n - c)
                    .collect();
                let disp_b: Vec<f32> = next_b
                    .iter()
                    .zip(current_b.iter())
                    .map(|(n, c)| n - c)
                    .collect();

                let mag_a = disp_a.iter().map(|x| x * x).sum::<f32>().sqrt();
                let mag_b = disp_b.iter().map(|x| x * x).sum::<f32>().sqrt();

                if mag_a > 1e-6 && mag_b > 1e-6 {
                    let sim = cosine_similarity(&disp_a, &disp_b)?;
                    total_score += sim;
                    valid_steps += 1;
                }

                current_a = next_a;
                current_b = next_b;
            }
        }

        if valid_steps == 0 {
            Ok(0.0)
        } else {
            Ok(total_score / valid_steps as f32)
        }
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
    fn test_symbiosis_co_evolution() {
        let db = AletheiaDB::new().unwrap();
        let config = HnswConfig::new(2, DistanceMetric::Cosine);
        db.enable_vector_index("vec", config).unwrap();

        let t0 = time::now();

        let node_a = db
            .create_node(
                "Point",
                PropertyMapBuilder::new()
                    .insert_vector("vec", &[0.0, 0.0])
                    .build(),
            )
            .unwrap();

        let node_b = db
            .create_node(
                "Point",
                PropertyMapBuilder::new()
                    .insert_vector("vec", &[1.0, 1.0])
                    .build(),
            )
            .unwrap();

        std::thread::sleep(std::time::Duration::from_millis(10));
        let _t1 = time::now();

        // Both move by +[1, 0]
        db.write(|tx| {
            tx.update_node(
                node_a,
                PropertyMapBuilder::new()
                    .insert_vector("vec", &[1.0, 0.0])
                    .build(),
            )
            .unwrap();
            tx.update_node(
                node_b,
                PropertyMapBuilder::new()
                    .insert_vector("vec", &[2.0, 1.0])
                    .build(),
            )
            .unwrap();
            Ok::<(), crate::core::error::Error>(())
        })
        .unwrap();

        std::thread::sleep(std::time::Duration::from_millis(10));
        let _t2 = time::now();

        // Both move by +[0, 1]
        db.write(|tx| {
            tx.update_node(
                node_a,
                PropertyMapBuilder::new()
                    .insert_vector("vec", &[1.0, 1.0])
                    .build(),
            )
            .unwrap();
            tx.update_node(
                node_b,
                PropertyMapBuilder::new()
                    .insert_vector("vec", &[2.0, 2.0])
                    .build(),
            )
            .unwrap();
            Ok::<(), crate::core::error::Error>(())
        })
        .unwrap();

        std::thread::sleep(std::time::Duration::from_millis(10));
        let t_end = time::now();

        let symbiosis = Symbiosis::new(&db);
        let window = TimeRange::new(t0, t_end).unwrap();

        let score = symbiosis
            .measure_co_evolution(node_a, node_b, window, "vec")
            .unwrap();

        // They evolved identically, their displacement vectors are parallel
        assert!(
            score > 0.9,
            "Expected high co-evolution score, got {}",
            score
        );
    }

    #[test]
    fn test_symbiosis_independent_evolution() {
        let db = AletheiaDB::new().unwrap();
        let config = HnswConfig::new(2, DistanceMetric::Cosine);
        db.enable_vector_index("vec", config).unwrap();

        let t0 = time::now();

        let node_a = db
            .create_node(
                "Point",
                PropertyMapBuilder::new()
                    .insert_vector("vec", &[0.0, 0.0])
                    .build(),
            )
            .unwrap();

        let node_b = db
            .create_node(
                "Point",
                PropertyMapBuilder::new()
                    .insert_vector("vec", &[1.0, 1.0])
                    .build(),
            )
            .unwrap();

        std::thread::sleep(std::time::Duration::from_millis(10));

        // Move A along X axis, move B along Y axis
        db.write(|tx| {
            tx.update_node(
                node_a,
                PropertyMapBuilder::new()
                    .insert_vector("vec", &[1.0, 0.0])
                    .build(),
            )
            .unwrap();
            tx.update_node(
                node_b,
                PropertyMapBuilder::new()
                    .insert_vector("vec", &[1.0, 2.0])
                    .build(),
            )
            .unwrap();
            Ok::<(), crate::core::error::Error>(())
        })
        .unwrap();

        std::thread::sleep(std::time::Duration::from_millis(10));
        let t_end = time::now();

        let symbiosis = Symbiosis::new(&db);
        let window = TimeRange::new(t0, t_end).unwrap();

        let score = symbiosis
            .measure_co_evolution(node_a, node_b, window, "vec")
            .unwrap();

        // They evolved orthogonally, their displacement vectors are orthogonal
        assert!(
            score.abs() < 0.1,
            "Expected low co-evolution score, got {}",
            score
        );
    }
}
