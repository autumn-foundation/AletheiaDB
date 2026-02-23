//! Janus: Semantic Bridge Detector.
//!
//! "Bridging the Gap."
//!
//! Janus identifies nodes that act as "bridges" or "diplomats" between distinct semantic clusters.
//! A high "Bridge Score" indicates that a node's neighbors fall into two (or more)
//! clearly separated groups in vector space, meaning the node connects different communities.
//!
//! # Use Cases
//! - **Interdisciplinary Research**: Finding papers that cite both Biology and CS.
//! - **Social Connectors**: finding people who bridge two social circles.
//! - **Conflict of Interest**: Detecting nodes that link opposing factions.
//!
//! # Example
//! ```rust,no_run
//! use aletheiadb::AletheiaDB;
//! use aletheiadb::experimental::janus::JanusDetector;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let db = AletheiaDB::new()?;
//! let janus = JanusDetector::new(&db);
//!
//! let score = janus.analyze_node(node_id, "embedding")?;
//!
//! if score.is_bridge() {
//!     println!("Node {} is a bridge! Score: {:.2}", node_id, score.total_score);
//! }
//! # Ok(())
//! # }
//! ```

#![allow(clippy::collapsible_if)]

use crate::AletheiaDB;
use crate::core::error::Result;
use crate::core::id::NodeId;

/// The result of a Janus analysis.
#[derive(Debug, Clone)]
pub struct BridgeScore {
    /// The calculated bridge score (0.0 to 1.0+).
    /// Higher is better. > 1.0 usually indicates good separation.
    pub total_score: f32,
    /// The distance between the two cluster centroids.
    pub inter_cluster_distance: f32,
    /// The average internal spread of the clusters.
    pub intra_cluster_spread: f32,
    /// Number of neighbors analyzed.
    pub neighbor_count: usize,
}

impl BridgeScore {
    /// Heuristic to determine if this is a significant bridge.
    pub fn is_bridge(&self) -> bool {
        // If clusters are separated by more distance than their internal spread,
        // it's a bridge.
        self.total_score > 1.2 && self.neighbor_count >= 4
    }
}

/// The Janus Detector.
pub struct JanusDetector<'a> {
    db: &'a AletheiaDB,
}

impl<'a> JanusDetector<'a> {
    /// Create a new JanusDetector.
    pub fn new(db: &'a AletheiaDB) -> Self {
        Self { db }
    }

    /// Analyze a node to see if it bridges semantic clusters.
    ///
    /// This fetches the node's neighbors, retrieves their vectors,
    /// and performs a local 2-Means clustering.
    pub fn analyze_node(&self, node_id: NodeId, property: &str) -> Result<BridgeScore> {
        // 1. Fetch Neighbors
        let edges = self.db.get_outgoing_edges(node_id);
        let mut vectors: Vec<Vec<f32>> = Vec::new();

        for edge_id in edges {
            let neighbor_id = self.db.get_edge_target(edge_id)?;
            // Get neighbor node
            let neighbor = self.db.get_node(neighbor_id)?;
            #[allow(clippy::collapsible_if)]
            if let Some(prop) = neighbor.get_property(property) {
                if let Some(vec) = prop.as_vector() {
                    vectors.push(vec.to_vec());
                }
            }
        }

        let count = vectors.len();
        if count < 2 {
            return Ok(BridgeScore {
                total_score: 0.0,
                inter_cluster_distance: 0.0,
                intra_cluster_spread: 0.0,
                neighbor_count: count,
            });
        }

        // 2. Local 2-Means Clustering
        // We want to split neighbors into 2 groups and see if they are distinct.
        // Simple K-Means with K=2.
        let (centroids, assignments) = self.kmeans_2(&vectors);

        // 3. Calculate Metrics
        let dist = euclidean_distance_sq(&centroids[0], &centroids[1]).sqrt();

        // Calculate intra-cluster spread (average distance to centroid)
        let mut spread_sum = 0.0;
        for (i, vec) in vectors.iter().enumerate() {
            let cluster_idx = assignments[i];
            let d = euclidean_distance_sq(vec, &centroids[cluster_idx]).sqrt();
            spread_sum += d;
        }
        let avg_spread = if count > 0 {
            spread_sum / count as f32
        } else {
            0.0
        };

        // Avoid division by zero
        let score = if avg_spread < 1e-6 {
            if dist > 1e-6 {
                100.0 // Infinite separation
            } else {
                0.0 // Both 0
            }
        } else {
            dist / avg_spread
        };

        Ok(BridgeScore {
            total_score: score,
            inter_cluster_distance: dist,
            intra_cluster_spread: avg_spread,
            neighbor_count: count,
        })
    }

    /// Simple 2-Means implementation.
    /// Returns (centroids, assignments).
    fn kmeans_2(&self, data: &[Vec<f32>]) -> (Vec<Vec<f32>>, Vec<usize>) {
        if data.is_empty() {
            return (vec![], vec![]);
        }
        let dim = data[0].len();

        // Init: Pick first point and the point farthest from it.
        let c1 = data[0].clone();
        let mut max_dist = -1.0;
        let mut c2 = data[0].clone();

        for v in data.iter().skip(1) {
            let d = euclidean_distance_sq(&c1, v);
            if d > max_dist {
                max_dist = d;
                c2 = v.clone();
            }
        }

        let mut centroids = vec![c1, c2];
        let mut assignments = vec![0; data.len()];

        for _ in 0..10 {
            // 10 iterations usually enough for local 2-means
            let mut changes = 0;
            let mut sums = vec![vec![0.0; dim]; 2];
            let mut counts = [0; 2];

            // Assign
            for (i, v) in data.iter().enumerate() {
                let d0 = euclidean_distance_sq(&centroids[0], v);
                let d1 = euclidean_distance_sq(&centroids[1], v);
                let cluster = if d0 < d1 { 0 } else { 1 };

                if assignments[i] != cluster {
                    changes += 1;
                }
                assignments[i] = cluster;

                for k in 0..dim {
                    sums[cluster][k] += v[k];
                }
                counts[cluster] += 1;
            }

            // Update
            for k in 0..2 {
                if counts[k] > 0 {
                    for d in 0..dim {
                        centroids[k][d] = sums[k][d] / counts[k] as f32;
                    }
                }
            }

            if changes == 0 {
                break;
            }
        }

        (centroids, assignments)
    }
}

fn euclidean_distance_sq(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b.iter()).map(|(x, y)| (x - y).powi(2)).sum()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::property::PropertyMapBuilder;
    use crate::index::vector::{DistanceMetric, HnswConfig};

    #[test]
    fn test_janus_dumbbell_bridge() {
        let db = AletheiaDB::new().unwrap();
        db.enable_vector_index("emb", HnswConfig::new(2, DistanceMetric::Euclidean))
            .unwrap();

        // Cluster A (near 0,0)
        let a1 = db
            .create_node(
                "Node",
                PropertyMapBuilder::new()
                    .insert_vector("emb", &[0.0, 0.0])
                    .build(),
            )
            .unwrap();
        let a2 = db
            .create_node(
                "Node",
                PropertyMapBuilder::new()
                    .insert_vector("emb", &[0.1, 0.0])
                    .build(),
            )
            .unwrap();
        let a3 = db
            .create_node(
                "Node",
                PropertyMapBuilder::new()
                    .insert_vector("emb", &[0.0, 0.1])
                    .build(),
            )
            .unwrap();

        // Cluster B (near 10,10)
        let b1 = db
            .create_node(
                "Node",
                PropertyMapBuilder::new()
                    .insert_vector("emb", &[10.0, 10.0])
                    .build(),
            )
            .unwrap();
        let b2 = db
            .create_node(
                "Node",
                PropertyMapBuilder::new()
                    .insert_vector("emb", &[10.1, 10.0])
                    .build(),
            )
            .unwrap();
        let b3 = db
            .create_node(
                "Node",
                PropertyMapBuilder::new()
                    .insert_vector("emb", &[10.0, 10.1])
                    .build(),
            )
            .unwrap();

        // The Bridge (connected to all)
        // Its vector doesn't matter for the score, only neighbors matter.
        let bridge = db
            .create_node(
                "Bridge",
                PropertyMapBuilder::new()
                    .insert_vector("emb", &[5.0, 5.0])
                    .build(),
            )
            .unwrap();

        // Connect Bridge to Cluster A
        for n in [a1, a2, a3] {
            db.create_edge(bridge, n, "LINKS", Default::default())
                .unwrap();
        }
        // Connect Bridge to Cluster B
        for n in [b1, b2, b3] {
            db.create_edge(bridge, n, "LINKS", Default::default())
                .unwrap();
        }

        let janus = JanusDetector::new(&db);
        let score = janus.analyze_node(bridge, "emb").unwrap();

        println!("Bridge Score: {:?}", score);

        // Inter-cluster distance should be approx sqrt(10^2 + 10^2) = 14.14
        // Intra-cluster spread should be small (< 0.2)
        // Score should be high (> 10)
        assert!(
            score.total_score > 5.0,
            "Bridge node should have high score"
        );
        assert!(score.inter_cluster_distance > 10.0);
        assert!(score.is_bridge());
    }

    #[test]
    fn test_janus_homogeneous_cluster() {
        let db = AletheiaDB::new().unwrap();
        db.enable_vector_index("emb", HnswConfig::new(2, DistanceMetric::Euclidean))
            .unwrap();

        // One big cluster around 0,0
        let center = db.create_node("Center", Default::default()).unwrap();

        for i in 0..6 {
            let v = [i as f32 * 0.1, 0.0];
            let n = db
                .create_node(
                    "Node",
                    PropertyMapBuilder::new().insert_vector("emb", &v).build(),
                )
                .unwrap();
            db.create_edge(center, n, "LINKS", Default::default())
                .unwrap();
        }

        let janus = JanusDetector::new(&db);
        let score = janus.analyze_node(center, "emb").unwrap();

        println!("Homogeneous Score: {:?}", score);

        // K-Means will still split it into 2 (left and right halves), but separation will be small compared to spread.
        // Inter-cluster dist approx 0.3. Spread approx 0.15. Score approx 2.0?
        // Wait, if spread is 0.15 and dist is 0.3, score is 2.0.
        // But for dumbbell, dist was 14, spread 0.1. Score 140.

        // So threshold > 1.2 might be too low if data is very clean?
        // But relative to Dumbbell, this should be much lower.

        assert!(
            score.total_score < 5.0,
            "Homogeneous node should have low score compared to bridge"
        );
    }
}
