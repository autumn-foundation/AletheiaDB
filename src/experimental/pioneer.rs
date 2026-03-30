//! Pioneer: Semantic Outlier Detection.
//!
//! "Who is bucking the trend?"
//!
//! Pioneer identifies nodes that are diverging the most from the global
//! semantic trajectory. If the entire graph (or a subset) is moving towards
//! concept A, Pioneer finds the node that is moving towards concept B (or
//! simply moving the fastest in an orthogonal direction).
//!
//! # Concepts
//! - **Global Trajectory**: The velocity vector of the global centroid between two times.
//! - **Node Trajectory**: The velocity vector of an individual node between two times.
//! - **Divergence**: The cosine distance (1 - cosine similarity) between a node's
//!   trajectory and the global trajectory.
//!
//! # Example
//! ```rust,no_run
//! use aletheiadb::AletheiaDB;
//! use aletheiadb::experimental::pioneer::PioneerEngine;
//! use aletheiadb::core::temporal::time;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let db = AletheiaDB::new()?;
//! let pioneer = PioneerEngine::new(&db);
//!
//! let t1 = time::now() - 3600 * 1_000_000 * 24 * 7; // Last week
//! let t2 = time::now(); // Now
//!
//! if let Some(score) = pioneer.detect_pioneer(t1, t2, "embedding")? {
//!     println!("Node {} is the pioneer with a divergence score of {:.4}!",
//!              score.node_id, score.divergence_score);
//! }
//! # Ok(())
//! # }
//! ```

use crate::AletheiaDB;
use crate::core::error::{Error, Result};
use crate::core::id::NodeId;
use crate::core::temporal::Timestamp;
use std::collections::HashSet;

/// The result of a Pioneer analysis.
#[derive(Debug, Clone)]
pub struct PioneerScore {
    /// The ID of the most divergent node.
    pub node_id: NodeId,
    /// The node's trajectory vector.
    pub node_trajectory: Vec<f32>,
    /// The global trajectory vector.
    pub global_trajectory: Vec<f32>,
    /// The divergence score (1.0 - cosine similarity). Higher is more divergent.
    pub divergence_score: f32,
}

/// The Pioneer Engine for detecting semantic outliers.
pub struct PioneerEngine<'a> {
    db: &'a AletheiaDB,
}

impl<'a> PioneerEngine<'a> {
    /// Create a new PioneerEngine instance.
    pub fn new(db: &'a AletheiaDB) -> Self {
        Self { db }
    }

    /// Find the node that diverges the most from the global semantic shift
    /// between two points in time.
    ///
    /// # Arguments
    /// * `time_a` - The first (historical) time point.
    /// * `time_b` - The second (recent) time point.
    /// * `property_name` - The vector property to analyze.
    pub fn detect_pioneer(
        &self,
        time_a: Timestamp,
        time_b: Timestamp,
        property_name: &str,
    ) -> Result<Option<PioneerScore>> {
        let mut all_node_ids = HashSet::new();
        let results = self.db.query().scan(None).execute(self.db)?;
        for row in results {
            let row = row?;
            if let Some(node) = row.entity.as_node() {
                all_node_ids.insert(node.id);
            }
        }

        let node_ids: Vec<_> = all_node_ids.into_iter().collect();

        // Query state at time A and B for all nodes to calculate the global centroid.
        // We'll also store the individual vectors for the node trajectories.
        let mut vecs_a = std::collections::HashMap::new();
        let mut vecs_b = std::collections::HashMap::new();

        let nodes_at_a = self.db.get_nodes_at_time(&node_ids, time_a, time_a)?;
        for (id, node_opt) in nodes_at_a {
            if let Some(node) = node_opt
                && let Some(prop) = node.get_property(property_name)
                && let Some(vec) = prop.as_vector()
            {
                vecs_a.insert(id, vec.to_vec());
            }
        }

        let nodes_at_b = self.db.get_nodes_at_time(&node_ids, time_b, time_b)?;
        for (id, node_opt) in nodes_at_b {
            if let Some(node) = node_opt
                && let Some(prop) = node.get_property(property_name)
                && let Some(vec) = prop.as_vector()
            {
                vecs_b.insert(id, vec.to_vec());
            }
        }

        let mut sum_a: Option<Vec<f32>> = None;
        let mut sum_b: Option<Vec<f32>> = None;
        let mut count_a = 0;
        let mut count_b = 0;

        for vec in vecs_a.values() {
            if sum_a.is_none() {
                sum_a = Some(vec![0.0; vec.len()]);
            }
            let sum = sum_a.as_mut().unwrap();
            if vec.len() == sum.len() {
                for i in 0..vec.len() {
                    sum[i] += vec[i];
                }
                count_a += 1;
            }
        }

        for vec in vecs_b.values() {
            if sum_b.is_none() {
                sum_b = Some(vec![0.0; vec.len()]);
            }
            let sum = sum_b.as_mut().unwrap();
            if vec.len() == sum.len() {
                for i in 0..vec.len() {
                    sum[i] += vec[i];
                }
                count_b += 1;
            }
        }

        if count_a == 0 || count_b == 0 {
            return Ok(None);
        }

        let mut centroid_a = sum_a.unwrap();
        let mut centroid_b = sum_b.unwrap();

        for v in &mut centroid_a {
            *v /= count_a as f32;
        }
        for v in &mut centroid_b {
            *v /= count_b as f32;
        }

        if centroid_a.len() != centroid_b.len() {
            return Err(Error::other("Dimension mismatch between time windows."));
        }

        // Global Trajectory Vg = centroid_b - centroid_a
        let global_traj: Vec<f32> = centroid_b
            .iter()
            .zip(centroid_a.iter())
            .map(|(b, a)| b - a)
            .collect();

        // If the global trajectory is essentially zero, we can't reliably calculate cosine similarity
        let g_norm_sq: f32 = global_traj.iter().map(|x| x * x).sum();
        if g_norm_sq < 1e-9 {
            return Ok(None); // No global movement
        }
        let g_norm = g_norm_sq.sqrt();

        let mut most_divergent_node = None;
        let mut max_divergence = -1.0;
        let mut pioneer_traj = Vec::new();

        // Calculate trajectory for each node present in both windows
        for (id, v_b) in &vecs_b {
            if let Some(v_a) = vecs_a.get(id) {
                if v_a.len() != v_b.len() || v_a.len() != global_traj.len() {
                    continue;
                }

                let node_traj: Vec<f32> = v_b.iter().zip(v_a.iter()).map(|(b, a)| b - a).collect();
                let n_norm_sq: f32 = node_traj.iter().map(|x| x * x).sum();

                if n_norm_sq < 1e-9 {
                    continue; // Node didn't move
                }
                let n_norm = n_norm_sq.sqrt();

                let dot_product: f32 = node_traj
                    .iter()
                    .zip(global_traj.iter())
                    .map(|(n, g)| n * g)
                    .sum();
                let cosine_similarity = dot_product / (n_norm * g_norm);

                // Divergence is 1 - similarity.
                // Similarity: 1.0 (same direction) -> Divergence: 0.0
                // Similarity: 0.0 (orthogonal) -> Divergence: 1.0
                // Similarity: -1.0 (opposite) -> Divergence: 2.0
                let divergence = 1.0 - cosine_similarity;

                if divergence > max_divergence {
                    max_divergence = divergence;
                    most_divergent_node = Some(*id);
                    pioneer_traj = node_traj;
                }
            }
        }

        if let Some(id) = most_divergent_node {
            Ok(Some(PioneerScore {
                node_id: id,
                node_trajectory: pioneer_traj,
                global_trajectory: global_traj,
                divergence_score: max_divergence,
            }))
        } else {
            Ok(None)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::transaction::WriteOps;
    use crate::core::property::PropertyMapBuilder;

    #[test]
    fn test_pioneer_divergence() {
        let db = AletheiaDB::new().unwrap();

        std::thread::sleep(std::time::Duration::from_millis(10));

        // Time A: Nodes start at origin
        let mut n1 = crate::core::id::NodeId::new(0).unwrap();
        let mut n2 = crate::core::id::NodeId::new(0).unwrap();
        let mut n3 = crate::core::id::NodeId::new(0).unwrap();

        db.write(|tx| {
            n1 = tx
                .create_node(
                    "Follower1",
                    PropertyMapBuilder::new()
                        .insert_vector("vec", &[0.0, 0.0])
                        .build(),
                )
                .unwrap();

            n2 = tx
                .create_node(
                    "Follower2",
                    PropertyMapBuilder::new()
                        .insert_vector("vec", &[0.0, 0.0])
                        .build(),
                )
                .unwrap();

            n3 = tx
                .create_node(
                    "Pioneer",
                    PropertyMapBuilder::new()
                        .insert_vector("vec", &[0.0, 0.0])
                        .build(),
                )
                .unwrap();
            Ok::<(), Error>(())
        })
        .unwrap();

        std::thread::sleep(std::time::Duration::from_millis(10));
        let time_a = crate::core::temporal::time::now();
        std::thread::sleep(std::time::Duration::from_millis(50));

        // Time B:
        // n1 and n2 move right (+X)
        // n3 moves left (-X) (opposite the trend)
        db.write(|tx| {
            tx.update_node(
                n1,
                PropertyMapBuilder::new()
                    .insert_vector("vec", &[10.0, 0.0])
                    .build(),
            )
            .unwrap();

            tx.update_node(
                n2,
                PropertyMapBuilder::new()
                    .insert_vector("vec", &[10.0, 0.0])
                    .build(),
            )
            .unwrap();

            tx.update_node(
                n3,
                PropertyMapBuilder::new()
                    .insert_vector("vec", &[-5.0, 0.0]) // Moves left
                    .build(),
            )
            .unwrap();
            Ok::<(), Error>(())
        })
        .unwrap();

        let time_b = crate::core::temporal::time::now();

        let engine = PioneerEngine::new(&db);
        let score_opt = engine.detect_pioneer(time_a, time_b, "vec").unwrap();

        assert!(score_opt.is_some());
        let score = score_opt.unwrap();

        // Pioneer should be n3
        assert_eq!(score.node_id, n3);

        // n3 moves left, so dot product with global trajectory (which is right overall) should be negative,
        // meaning cosine similarity is negative, meaning divergence > 1.0.
        // Exact global centroid B: (10+10-5)/3 = 15/3 = 5. Centroid A: 0. Global traj: +5X.
        // Node 3 traj: -5X.
        // Cosine similarity: -1.0. Divergence: 2.0.
        assert!((score.divergence_score - 2.0).abs() < 0.01);
    }
}
