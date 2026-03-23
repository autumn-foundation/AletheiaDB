//! Lighthouse: Semantic Convergence Tracking.
//!
//! "Are these nodes drifting apart?"
//!
//! The Lighthouse module tracks the semantic evolution of two nodes over time,
//! analyzing how their vector embeddings converge or diverge. It is designed to
//! measure shifts in meaning, relationships, or identity across their historical
//! trajectory.
//!
//! # Use Cases
//! - **Relationship Drift**: Detecting when two users, concepts, or documents begin
//!   to diverge semantically.
//! - **Ideological Convergence**: Identifying when distinct concepts align over time.
//! - **Echo Chamber Detection**: Monitoring the tightening of semantic similarity
//!   within a group.
//!
//! # Example
//! ```rust,no_run
//! use aletheiadb::AletheiaDB;
//! use aletheiadb::experimental::lighthouse::Lighthouse;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let db = AletheiaDB::new()?;
//! # let node1 = db.create_node("Concept", Default::default())?;
//! # let node2 = db.create_node("Concept", Default::default())?;
//! let lighthouse = Lighthouse::new(&db);
//!
//! // Track convergence of "embedding" property
//! let trajectory = lighthouse.track_convergence(node1, node2, "embedding")?;
//!
//! if trajectory.is_converging() {
//!     println!("Concepts are aligning!");
//! } else if trajectory.is_diverging() {
//!     println!("Concepts are drifting apart!");
//! }
//! # Ok(())
//! # }
//! ```

use crate::AletheiaDB;
use crate::core::error::Result;
use crate::core::id::NodeId;
use crate::core::temporal::Timestamp;
use crate::core::vector::cosine_similarity;

/// A single observation of semantic similarity between two nodes at a point in time.
#[derive(Debug, Clone, PartialEq)]
pub struct ConvergencePoint {
    /// The transaction time of the observation.
    pub timestamp: Timestamp,
    /// The cosine similarity between the two nodes at this point.
    pub similarity: f32,
    /// The version numbers of the nodes at this point (source_version, target_version).
    pub versions: (u64, u64),
}

/// The trajectory of semantic convergence or divergence between two nodes.
#[derive(Debug, Clone, PartialEq)]
pub struct ConvergenceTrajectory {
    /// The source node ID.
    pub source: NodeId,
    /// The target node ID.
    pub target: NodeId,
    /// The property key used for the comparison.
    pub property_key: String,
    /// The chronological sequence of convergence points.
    pub points: Vec<ConvergencePoint>,
}

impl ConvergenceTrajectory {
    /// Returns true if the nodes are generally converging (similarity increasing).
    ///
    /// Evaluates the linear trend of similarities over time. Returns true if the slope is positive.
    pub fn is_converging(&self) -> bool {
        self.trend_slope() > 0.0
    }

    /// Returns true if the nodes are generally diverging (similarity decreasing).
    ///
    /// Evaluates the linear trend of similarities over time. Returns true if the slope is negative.
    pub fn is_diverging(&self) -> bool {
        self.trend_slope() < 0.0
    }

    /// Calculates the linear regression slope of the similarity over time.
    ///
    /// A positive slope indicates convergence, negative indicates divergence.
    /// Returns 0.0 if there are less than 2 points.
    pub fn trend_slope(&self) -> f32 {
        if self.points.len() < 2 {
            return 0.0;
        }

        let n = self.points.len() as f32;

        // Use normalized time steps for numerical stability
        let mut sum_x = 0.0;
        let mut sum_y = 0.0;
        let mut sum_xy = 0.0;
        let mut sum_xx = 0.0;

        for (i, point) in self.points.iter().enumerate() {
            let x = i as f32;
            let y = point.similarity;

            sum_x += x;
            sum_y += y;
            sum_xy += x * y;
            sum_xx += x * x;
        }

        let denominator = n * sum_xx - sum_x * sum_x;
        if denominator == 0.0 {
            return 0.0; // Avoid division by zero
        }

        (n * sum_xy - sum_x * sum_y) / denominator
    }

    /// Returns the most recent similarity score, or None if no points exist.
    pub fn current_similarity(&self) -> Option<f32> {
        self.points.last().map(|p| p.similarity)
    }

    /// Returns the initial similarity score, or None if no points exist.
    pub fn initial_similarity(&self) -> Option<f32> {
        self.points.first().map(|p| p.similarity)
    }
}

/// The Lighthouse engine for tracking semantic convergence.
pub struct Lighthouse<'a> {
    db: &'a AletheiaDB,
}

impl<'a> Lighthouse<'a> {
    /// Creates a new Lighthouse instance.
    pub fn new(db: &'a AletheiaDB) -> Self {
        Self { db }
    }

    /// Tracks the semantic convergence trajectory between two nodes over time.
    ///
    /// Extracts the history of both nodes, aligns their versions temporally,
    /// and calculates the cosine similarity of the specified vector property at each distinct point in time
    /// where either node changed.
    ///
    /// # Arguments
    /// * `source` - The first node.
    /// * `target` - The second node.
    /// * `property_key` - The property key containing the vector embedding.
    pub fn track_convergence(
        &self,
        source: NodeId,
        target: NodeId,
        property_key: &str,
    ) -> Result<ConvergenceTrajectory> {
        let source_history = self.db.get_node_history(source)?;
        let target_history = self.db.get_node_history(target)?;

        if source_history.version_count() == 0 || target_history.version_count() == 0 {
            return Ok(ConvergenceTrajectory {
                source,
                target,
                property_key: property_key.to_string(),
                points: Vec::new(),
            });
        }

        // Collect all distinct transaction timestamps where either node was modified
        let mut timestamps: Vec<Timestamp> = Vec::new();

        for v in &source_history.versions {
            timestamps.push(v.temporal.transaction_time().start());
        }
        for v in &target_history.versions {
            timestamps.push(v.temporal.transaction_time().start());
        }

        // Sort and deduplicate timestamps
        timestamps.sort();
        timestamps.dedup();

        let mut points = Vec::new();

        // For each distinct point in time, find the active version for both nodes
        // and compute similarity if both have the vector property.
        for ts in timestamps {
            let source_version = self.find_active_version(&source_history.versions, ts);
            let target_version = self.find_active_version(&target_history.versions, ts);

            if let (Some(sv), Some(tv)) = (source_version, target_version) {
                let s_prop = sv.properties.get(property_key);
                let t_prop = tv.properties.get(property_key);

                #[allow(clippy::collapsible_if)]
                if let (Some(sp), Some(tp)) = (s_prop, t_prop) {
                    #[allow(clippy::collapsible_if)]
                    if let (Some(s_vec), Some(t_vec)) = (sp.as_vector(), tp.as_vector()) {
                        if let Ok(sim) = cosine_similarity(s_vec, t_vec) {
                            points.push(ConvergencePoint {
                                timestamp: ts,
                                similarity: sim,
                                versions: (sv.version_number, tv.version_number),
                            });
                        }
                    }
                }
            }
        }

        // It's possible for multiple timestamps to map to the same pair of versions,
        // resulting in duplicate similarity points. Deduplicate them while preserving chronological order.
        points.dedup_by(|a, b| {
            a.versions == b.versions && (a.similarity - b.similarity).abs() < f32::EPSILON
        });

        Ok(ConvergenceTrajectory {
            source,
            target,
            property_key: property_key.to_string(),
            points,
        })
    }

    /// Finds the latest version active at or before the given timestamp.
    fn find_active_version<'v>(
        &self,
        versions: &'v [crate::core::history::VersionInfo],
        ts: Timestamp,
    ) -> Option<&'v crate::core::history::VersionInfo> {
        versions
            .iter()
            .rev()
            .find(|v| v.temporal.transaction_time().start() <= ts)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::WriteOps;
    use crate::core::PropertyValue;
    use crate::properties;
    use std::thread::sleep;
    use std::time::Duration;

    #[test]
    fn test_convergence_trajectory() -> Result<()> {
        let db = AletheiaDB::new()?;

        // Create nodes with divergent initial vectors (orthogonal: similarity 0.0)
        let source = db.create_node(
            "Concept",
            properties! {
                "embedding" => PropertyValue::vector(vec![1.0, 0.0]),
            },
        )?;

        let target = db.create_node(
            "Concept",
            properties! {
                "embedding" => PropertyValue::vector(vec![0.0, 1.0]),
            },
        )?;

        let lighthouse = Lighthouse::new(&db);
        let initial_traj = lighthouse.track_convergence(source, target, "embedding")?;

        assert_eq!(initial_traj.points.len(), 1);
        assert!(initial_traj.initial_similarity().unwrap().abs() < 1e-6);

        sleep(Duration::from_millis(10)); // Ensure distinct timestamps

        // Step 1: They begin converging. Target moves slightly towards source.
        // Source: [1.0, 0.0], Target: [0.5, 0.866] (normalized)
        db.write(|tx| {
            tx.update_node(
                target,
                properties! {
                    "embedding" => PropertyValue::vector(vec![0.5, 0.866]),
                },
            )
        })?;

        let step1_traj = lighthouse.track_convergence(source, target, "embedding")?;
        assert_eq!(step1_traj.points.len(), 2);
        assert!(step1_traj.is_converging());
        assert!(!step1_traj.is_diverging());
        // Similarity should be ~0.5 (cos 60 deg)
        assert!((step1_traj.current_similarity().unwrap() - 0.5).abs() < 1e-2);

        sleep(Duration::from_millis(10));

        // Step 2: Full convergence. Target becomes identical to source.
        db.write(|tx| {
            tx.update_node(
                target,
                properties! {
                    "embedding" => PropertyValue::vector(vec![1.0, 0.0]),
                },
            )
        })?;

        let step2_traj = lighthouse.track_convergence(source, target, "embedding")?;
        assert_eq!(step2_traj.points.len(), 3);
        assert!(step2_traj.is_converging());
        assert!(!step2_traj.is_diverging());
        // Similarity should be ~1.0
        assert!((step2_traj.current_similarity().unwrap() - 1.0).abs() < 1e-6);

        sleep(Duration::from_millis(10));

        // Step 3: Divergence. Source moves away.
        db.write(|tx| {
            tx.update_node(
                source,
                properties! {
                    "embedding" => PropertyValue::vector(vec![-1.0, 0.0]),
                },
            )
        })?;

        let step3_traj = lighthouse.track_convergence(source, target, "embedding")?;
        assert_eq!(step3_traj.points.len(), 4);

        // Overall trend might still be converging depending on the exact points,
        // but let's check the current similarity dropped
        let current_sim = step3_traj.current_similarity().unwrap();
        assert!((current_sim - (-1.0)).abs() < 1e-6); // Opposite vectors

        // Since we went 0.0 -> 0.5 -> 1.0 -> -1.0, the overall linear trend slope
        // will now be negative, indicating divergence.
        assert!(step3_traj.is_diverging());
        assert!(!step3_traj.is_converging());

        Ok(())
    }

    #[test]
    fn test_missing_property() -> Result<()> {
        let db = AletheiaDB::new()?;
        let source = db.create_node("Concept", Default::default())?;
        let target = db.create_node("Concept", Default::default())?;

        let lighthouse = Lighthouse::new(&db);
        let traj = lighthouse.track_convergence(source, target, "missing_prop")?;

        assert!(traj.points.is_empty());
        assert_eq!(traj.trend_slope(), 0.0);
        assert!(!traj.is_converging());
        assert!(!traj.is_diverging());

        Ok(())
    }
}
