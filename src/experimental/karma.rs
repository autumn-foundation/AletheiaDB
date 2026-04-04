//! Karma: Semantic Reciprocity Engine.
//!
//! "Are we growing closer, or drifting apart?"
//!
//! Karma measures the relative semantic trajectory of two nodes over time.
//! It evaluates whether their embeddings are converging (getting closer),
//! diverging (moving further apart), or remaining parallel.
//!
//! # Use Cases
//! - **Social Dynamics**: Detecting polarization or consensus building.
//! - **Recommendation Systems**: Identifying users whose interests are aligning.
//! - **Concept Drift**: Tracking if two previously related topics are separating.
//!
//! # Example
//! ```rust,no_run
//! use aletheiadb::AletheiaDB;
//! use aletheiadb::experimental::karma::{Karma, ReciprocityStatus};
//! use aletheiadb::core::temporal::{TimeRange, time};
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let db = AletheiaDB::new()?;
//! # let node_a = db.create_node("User", Default::default())?;
//! # let node_b = db.create_node("User", Default::default())?;
//! let karma = Karma::new(&db);
//!
//! let range = TimeRange::new(time::from_secs(0), time::now())?;
//! let score = karma.measure_reciprocity(node_a, node_b, range, "embedding")?;
//!
//! match score.status {
//!     ReciprocityStatus::Converging => println!("They are growing closer!"),
//!     ReciprocityStatus::Diverging => println!("They are drifting apart!"),
//!     ReciprocityStatus::Parallel => println!("Their distance is stable."),
//!     ReciprocityStatus::InsufficientData => println!("Not enough history to tell."),
//! }
//! # Ok(())
//! # }
//! ```

use crate::AletheiaDB;
use crate::core::error::Result;
use crate::core::id::NodeId;
use crate::core::temporal::TimeRange;
use crate::core::vector::ops::euclidean_distance;

/// The relationship dynamic between two nodes over time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReciprocityStatus {
    /// Nodes are moving closer together in vector space.
    Converging,
    /// Nodes are moving further apart in vector space.
    Diverging,
    /// The distance between nodes remains roughly constant.
    Parallel,
    /// Not enough historical data overlap to determine reciprocity.
    InsufficientData,
}

/// A reading of semantic reciprocity.
#[derive(Debug, Clone, Copy)]
pub struct ReciprocityScore {
    /// The starting distance between the two nodes.
    pub initial_distance: f32,
    /// The ending distance between the two nodes.
    pub final_distance: f32,
    /// The overall change in distance (negative means converging).
    pub delta: f32,
    /// The classification of the dynamic.
    pub status: ReciprocityStatus,
}

/// The Karma engine for reciprocity analysis.
pub struct Karma<'a> {
    db: &'a AletheiaDB,
}

impl<'a> Karma<'a> {
    /// Create a new Karma instance.
    pub fn new(db: &'a AletheiaDB) -> Self {
        Self { db }
    }

    /// Measure the semantic reciprocity between two nodes over a time range.
    ///
    /// # Arguments
    /// * `node_a` - The first node.
    /// * `node_b` - The second node.
    /// * `window` - The time range to analyze.
    /// * `property` - The name of the vector property to track.
    pub fn measure_reciprocity(
        &self,
        node_a: NodeId,
        node_b: NodeId,
        window: TimeRange,
        property: &str,
    ) -> Result<ReciprocityScore> {
        let history_a = self.db.get_node_history(node_a)?;
        let history_b = self.db.get_node_history(node_b)?;

        // Find the earliest valid vectors for both nodes within the window
        let mut initial_vec_a = None;
        let mut initial_vec_b = None;
        let mut final_vec_a = None;
        let mut final_vec_b = None;

        for version in history_a.versions.iter() {
            if !version.temporal.valid_time().overlaps(&window) {
                continue;
            }
            if let Some(vec) = version.properties.get(property).and_then(|v| v.as_vector()) {
                if initial_vec_a.is_none() {
                    initial_vec_a = Some(vec.to_vec());
                }
                final_vec_a = Some(vec.to_vec());
            }
        }

        for version in history_b.versions.iter() {
            if !version.temporal.valid_time().overlaps(&window) {
                continue;
            }
            if let Some(vec) = version.properties.get(property).and_then(|v| v.as_vector()) {
                if initial_vec_b.is_none() {
                    initial_vec_b = Some(vec.to_vec());
                }
                final_vec_b = Some(vec.to_vec());
            }
        }

        match (initial_vec_a, initial_vec_b, final_vec_a, final_vec_b) {
            (Some(ia), Some(ib), Some(fa), Some(fb)) => {
                let initial_distance = euclidean_distance(&ia, &ib)?;
                let final_distance = euclidean_distance(&fa, &fb)?;
                let delta = final_distance - initial_distance;

                let status = if delta < -1e-5 {
                    ReciprocityStatus::Converging
                } else if delta > 1e-5 {
                    ReciprocityStatus::Diverging
                } else {
                    ReciprocityStatus::Parallel
                };

                Ok(ReciprocityScore {
                    initial_distance,
                    final_distance,
                    delta,
                    status,
                })
            }
            _ => Ok(ReciprocityScore {
                initial_distance: 0.0,
                final_distance: 0.0,
                delta: 0.0,
                status: ReciprocityStatus::InsufficientData,
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::transaction::WriteOps;
    use crate::core::property::PropertyMapBuilder;
    use crate::core::temporal::time;

    #[test]
    fn test_karma_converging() {
        let db = AletheiaDB::new().unwrap();

        let props_a = PropertyMapBuilder::new()
            .insert_vector("vec", &[0.0, 0.0])
            .build();
        let props_b = PropertyMapBuilder::new()
            .insert_vector("vec", &[10.0, 0.0])
            .build();

        let node_a = db.create_node("Point", props_a).unwrap();
        let node_b = db.create_node("Point", props_b).unwrap();

        // Move closer
        std::thread::sleep(std::time::Duration::from_millis(10));
        db.write(|tx| {
            tx.update_node(
                node_a,
                PropertyMapBuilder::new()
                    .insert_vector("vec", &[3.0, 0.0])
                    .build(),
            )?;
            tx.update_node(
                node_b,
                PropertyMapBuilder::new()
                    .insert_vector("vec", &[7.0, 0.0])
                    .build(),
            )
        })
        .unwrap();

        let karma = Karma::new(&db);
        let window = TimeRange::new(time::from_secs(0), time::now()).unwrap();

        let score = karma
            .measure_reciprocity(node_a, node_b, window, "vec")
            .unwrap();

        assert_eq!(score.status, ReciprocityStatus::Converging);
        assert!(score.delta < 0.0);
        assert_eq!(score.initial_distance, 10.0);
        assert_eq!(score.final_distance, 4.0);
    }

    #[test]
    fn test_karma_diverging() {
        let db = AletheiaDB::new().unwrap();

        let props_a = PropertyMapBuilder::new()
            .insert_vector("vec", &[4.0, 0.0])
            .build();
        let props_b = PropertyMapBuilder::new()
            .insert_vector("vec", &[6.0, 0.0])
            .build();

        let node_a = db.create_node("Point", props_a).unwrap();
        let node_b = db.create_node("Point", props_b).unwrap();

        // Move apart
        std::thread::sleep(std::time::Duration::from_millis(10));
        db.write(|tx| {
            tx.update_node(
                node_a,
                PropertyMapBuilder::new()
                    .insert_vector("vec", &[0.0, 0.0])
                    .build(),
            )?;
            tx.update_node(
                node_b,
                PropertyMapBuilder::new()
                    .insert_vector("vec", &[10.0, 0.0])
                    .build(),
            )
        })
        .unwrap();

        let karma = Karma::new(&db);
        let window = TimeRange::new(time::from_secs(0), time::now()).unwrap();

        let score = karma
            .measure_reciprocity(node_a, node_b, window, "vec")
            .unwrap();

        assert_eq!(score.status, ReciprocityStatus::Diverging);
        assert!(score.delta > 0.0);
        assert_eq!(score.initial_distance, 2.0);
        assert_eq!(score.final_distance, 10.0);
    }
}
