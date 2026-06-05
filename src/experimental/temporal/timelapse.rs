//! Timelapse: Semantic Trajectory Analyzer.
//!
//! "How has this concept evolved over time?"
//!
//! Timelapse analyzes the bi-temporal history of a node, extracting its vector
//! embedding at each version to compute its "Semantic Trajectory".
//!
//! It calculates:
//! - **Total Distance**: Cumulative cosine distance traveled across all versions.
//! - **Net Displacement**: Cosine distance between the first and last version.
//! - **Volatility**: Total Distance / Net Displacement. High volatility means it
//!   wanders in semantic space. Low volatility means it moves in a straight line.
//!
//! # Example
//! ```rust,no_run
//! use aletheiadb::AletheiaDB;
//! use aletheiadb::experimental::temporal::timelapse::TimelapseEngine;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let db = AletheiaDB::new()?;
//! let engine = TimelapseEngine::new(&db);
//!
//! # let start_node = aletheiadb::core::id::NodeId::new(1).unwrap();
//! let metrics = engine.analyze(start_node, "embedding")?;
//! println!("Semantic Volatility: {}", metrics.volatility);
//! # Ok(())
//! # }
//! ```

use crate::AletheiaDB;
use crate::core::error::Result;
use crate::core::id::NodeId;
use crate::core::property::PropertyValue;
use crate::core::vector::cosine_similarity;

/// Metrics summarizing a node's semantic trajectory.
#[derive(Debug, Clone, PartialEq)]
pub struct TrajectoryMetrics {
    /// Cumulative cosine distance traveled across all versions.
    pub total_distance: f32,
    /// Cosine distance between the first and last version.
    pub net_displacement: f32,
    /// Ratio of total distance to net displacement (how direct the path is).
    /// If net displacement is 0, this is 0.0.
    pub volatility: f32,
    /// Number of versions that contained the specified vector property.
    pub valid_versions: usize,
}

/// Engine for analyzing semantic trajectories of nodes over time.
pub struct TimelapseEngine<'a> {
    db: &'a AletheiaDB,
}

#[cfg(feature = "semantic-temporal")]
impl<'a> TimelapseEngine<'a> {
    /// Create a new TimelapseEngine instance.
    pub fn new(db: &'a AletheiaDB) -> Self {
        Self { db }
    }

    /// Analyzes the semantic trajectory of a given node using a specific vector property.
    pub fn analyze(&self, node_id: NodeId, vector_key: &str) -> Result<TrajectoryMetrics> {
        let history = self.db.get_node_history(node_id)?;

        let mut vectors = Vec::new();

        // Extract vectors from history, oldest to newest
        for version in history.versions {
            if let Some(PropertyValue::Vector(v)) = version.properties.get(vector_key) {
                vectors.push(v.clone());
            }
        }

        if vectors.len() < 2 {
            return Ok(TrajectoryMetrics {
                total_distance: 0.0,
                net_displacement: 0.0,
                volatility: 0.0,
                valid_versions: vectors.len(),
            });
        }

        let mut total_distance = 0.0;
        for i in 1..vectors.len() {
            let sim = cosine_similarity(&vectors[i - 1], &vectors[i]).unwrap_or(1.0);
            total_distance += 1.0 - sim;
        }

        let first_vec = &vectors[0];
        let last_vec = &vectors[vectors.len() - 1];
        let net_sim = cosine_similarity(first_vec, last_vec).unwrap_or(1.0);
        let net_displacement = 1.0 - net_sim;

        let volatility = if net_displacement > 1e-6 {
            total_distance / net_displacement
        } else if total_distance > 1e-6 {
            // Infinite volatility (wandered and came back to exactly the same point)
            f32::INFINITY
        } else {
            0.0
        };

        Ok(TrajectoryMetrics {
            total_distance,
            net_displacement,
            volatility,
            valid_versions: vectors.len(),
        })
    }
}

#[cfg(all(test, feature = "semantic-temporal"))]
mod tests {
    use super::*;
    use crate::api::transaction::WriteOps;
    use crate::core::property::PropertyMapBuilder;

    #[test]
    fn test_timelapse_trajectory() -> Result<()> {
        let db = AletheiaDB::new()?;
        let engine = TimelapseEngine::new(&db);

        // Version 1: [1.0, 0.0]
        let props1 = PropertyMapBuilder::new()
            .insert("embedding", vec![1.0_f32, 0.0_f32])
            .build();
        let node_id = db.create_node("Concept", props1)?;

        // Version 2: [0.707, 0.707] (45 degrees)
        let props2 = PropertyMapBuilder::new()
            .insert("embedding", vec![0.707_f32, 0.707_f32])
            .build();
        db.write(|tx| tx.update_node(node_id, props2))?;

        // Version 3: [0.0, 1.0] (90 degrees)
        let props3 = PropertyMapBuilder::new()
            .insert("embedding", vec![0.0_f32, 1.0_f32])
            .build();
        db.write(|tx| tx.update_node(node_id, props3))?;

        let metrics = engine.analyze(node_id, "embedding")?;

        assert_eq!(metrics.valid_versions, 3);

        // Net displacement from [1.0, 0.0] to [0.0, 1.0] is 1.0 - cos(90) = 1.0
        assert!((metrics.net_displacement - 1.0).abs() < 1e-5);

        // Total distance is 1.0 - cos(45) + 1.0 - cos(45) = 2 * (1.0 - 0.7071) = 0.58578
        // Volatility is total_distance / net_displacement ~ 0.58578
        assert!(metrics.total_distance > 0.0);
        assert!((metrics.volatility - metrics.total_distance).abs() < 1e-5);

        Ok(())
    }

    #[test]
    fn test_timelapse_insufficient_history() -> Result<()> {
        let db = AletheiaDB::new()?;
        let engine = TimelapseEngine::new(&db);

        // Only 1 version
        let props1 = PropertyMapBuilder::new()
            .insert("embedding", vec![1.0_f32, 0.0_f32])
            .build();
        let node_id = db.create_node("Concept", props1)?;

        let metrics = engine.analyze(node_id, "embedding")?;

        assert_eq!(metrics.valid_versions, 1);
        assert_eq!(metrics.total_distance, 0.0);
        assert_eq!(metrics.net_displacement, 0.0);
        assert_eq!(metrics.volatility, 0.0);

        Ok(())
    }
}
