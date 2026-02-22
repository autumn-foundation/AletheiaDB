//! The Diplomat: Semantic Bridge Detection.
//!
//! "Who connects the unconnected?"
//!
//! The Diplomat identifies nodes that act as bridges between disparate semantic clusters.
//! These nodes are "interdisciplinary" or "hubs" that have neighbors belonging to multiple
//! different clusters.
//!
//! # Theory
//!
//! A "Diplomatic Score" is calculated based on the Shannon Entropy of the cluster distribution
//! of a node's neighbors.
//!
//! - **High Entropy**: Neighbors are evenly distributed across many clusters (High Diplomacy).
//! - **Low Entropy**: Neighbors are all in the same cluster (Low Diplomacy / Localized).
//!
//! # Use Cases
//! - **Innovation Discovery**: Find researchers publishing across multiple fields.
//! - **Code Quality**: Identify "God Objects" or "Glue Code" that touches many subsystems.
//! - **Social Network Analysis**: Find users who bridge social cliques.
//!
//! # Example
//!
//! ```rust,no_run
//! use aletheiadb::AletheiaDB;
//! use aletheiadb::experimental::cartographer::Cartographer;
//! use aletheiadb::experimental::diplomat::Diplomat;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let db = AletheiaDB::new()?;
//!
//! // 1. Run Cartographer to get clusters
//! let cartographer = Cartographer::new(&db);
//! let clustering = cartographer.analyze("embedding", 5)?;
//!
//! // 2. Run Diplomat to find bridges
//! let diplomat = Diplomat::new(&db);
//! let bridges = diplomat.identify_bridges(&clustering, 10)?;
//!
//! for (node_id, score) in bridges {
//!     println!("Node {} is a diplomat with score {:.4}", node_id, score);
//! }
//! # Ok(())
//! # }
//! ```

use crate::AletheiaDB;
use crate::core::error::Result;
use crate::core::id::NodeId;
use crate::experimental::cartographer::ClusteringResult;
use std::collections::HashMap;

/// The Diplomat Engine.
pub struct Diplomat<'a> {
    db: &'a AletheiaDB,
}

impl<'a> Diplomat<'a> {
    /// Create a new Diplomat instance.
    pub fn new(db: &'a AletheiaDB) -> Self {
        Self { db }
    }

    /// Identify top bridges based on the given clustering.
    ///
    /// # Arguments
    /// * `clustering` - The result from `Cartographer::analyze`.
    /// * `top_k` - Number of top bridges to return.
    ///
    /// # Returns
    /// A list of `(NodeId, Score)` tuples, sorted by score descending.
    pub fn identify_bridges(
        &self,
        clustering: &ClusteringResult,
        top_k: usize,
    ) -> Result<Vec<(NodeId, f32)>> {
        let mut scores = Vec::new();

        // Iterate over all nodes in the clustering
        // We only care about nodes that were clustered (have vectors)
        for &node_id in clustering.assignments.keys() {
            let score = self.calculate_diplomatic_score(node_id, clustering)?;
            if score > 0.0 {
                scores.push((node_id, score));
            }
        }

        // Sort by score descending
        scores.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

        // Take top K
        Ok(scores.into_iter().take(top_k).collect())
    }

    /// Calculate the Diplomatic Score for a single node.
    ///
    /// The score is the Shannon Entropy of the cluster assignments of the node's neighbors.
    ///
    /// Formula: H(X) = -sum(p_i * log2(p_i))
    /// Where p_i is the proportion of neighbors belonging to cluster i.
    pub fn calculate_diplomatic_score(
        &self,
        node_id: NodeId,
        clustering: &ClusteringResult,
    ) -> Result<f32> {
        // 1. Get neighbors
        // We use both incoming and outgoing edges for full context
        let mut neighbors = std::collections::HashSet::new();

        // Outgoing
        for edge_id in self.db.get_outgoing_edges(node_id) {
            if let Ok(target) = self.db.get_edge_target(edge_id) {
                neighbors.insert(target);
            }
        }

        // Incoming
        for edge_id in self.db.get_incoming_edges(node_id) {
            if let Ok(source) = self.db.get_edge_source(edge_id) {
                neighbors.insert(source);
            }
        }

        // Remove self-loops
        neighbors.remove(&node_id);

        if neighbors.is_empty() {
            return Ok(0.0);
        }

        // 2. Count cluster occurrences
        let mut cluster_counts: HashMap<usize, usize> = HashMap::new();
        let mut total_classified_neighbors = 0;

        for neighbor_id in neighbors {
            if let Some(&cluster_idx) = clustering.assignments.get(&neighbor_id) {
                *cluster_counts.entry(cluster_idx).or_insert(0) += 1;
                total_classified_neighbors += 1;
            }
        }

        if total_classified_neighbors == 0 {
            return Ok(0.0);
        }

        // 3. Calculate Entropy
        let mut entropy = 0.0;
        let total = total_classified_neighbors as f32;

        for &count in cluster_counts.values() {
            let p = count as f32 / total;
            if p > 0.0 {
                entropy -= p * p.log2();
            }
        }

        Ok(entropy)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::property::PropertyMapBuilder;
    use crate::index::vector::{DistanceMetric, HnswConfig};

    #[test]
    fn test_diplomat_finds_bridge() {
        let db = AletheiaDB::new().unwrap();
        // Enable vector index for Cartographer/Diplomat simulation
        let config = HnswConfig::new(2, DistanceMetric::Euclidean);
        db.enable_vector_index("embedding", config).unwrap();

        // Scenario: Two Clusters (Left and Right)
        // Cluster 0: Nodes L1, L2 (Vector [0,0])
        // Cluster 1: Nodes R1, R2 (Vector [10,10])
        // Bridge Node B (Vector [5,5])
        //
        // Connections:
        // B -> L1
        // B -> R1
        //
        // L1 -> L2 (Internal)
        // R1 -> R2 (Internal)
        //
        // B should have neighbors in Cluster 0 and Cluster 1 -> High Entropy.
        // L1 should have neighbor L2 (Cluster 0) and B (Cluster ?) -> Lower Entropy?
        // Wait, B will be assigned to *some* cluster. Let's say Cluster 2 or one of them.
        // If B is in Cluster 0, then L1 sees neighbors in Cluster 0 (L2) and Cluster 0 (B) -> Entropy 0.
        // B sees neighbors in Cluster 0 (L1) and Cluster 1 (R1) -> Entropy > 0.

        // Create Nodes
        let props_l = PropertyMapBuilder::new()
            .insert_vector("embedding", &[0.0, 0.0])
            .build();
        let l1 = db.create_node("L", props_l.clone()).unwrap();
        let l2 = db.create_node("L", props_l.clone()).unwrap();

        let props_r = PropertyMapBuilder::new()
            .insert_vector("embedding", &[10.0, 10.0])
            .build();
        let r1 = db.create_node("R", props_r.clone()).unwrap();
        let r2 = db.create_node("R", props_r.clone()).unwrap();

        let props_b = PropertyMapBuilder::new()
            .insert_vector("embedding", &[5.0, 5.0])
            .build();
        let b = db.create_node("Bridge", props_b).unwrap();

        // Create Edges
        // B connects to both sides
        db.create_edge(b, l1, "LINK", Default::default()).unwrap();
        db.create_edge(b, r1, "LINK", Default::default()).unwrap();

        // Internal connections
        db.create_edge(l1, l2, "LINK", Default::default()).unwrap();
        db.create_edge(r1, r2, "LINK", Default::default()).unwrap();

        // Mock Clustering Result
        // We manually construct assignments because running Cartographer (KMeans) is non-deterministic
        // or overkill for unit test logic verification.
        let mut assignments = HashMap::new();
        assignments.insert(l1, 0);
        assignments.insert(l2, 0);
        assignments.insert(r1, 1);
        assignments.insert(r2, 1);
        assignments.insert(b, 2); // B is in its own cluster or one of them. Let's say its own.

        let clustering = ClusteringResult {
            centroids: vec![], // Not used by Diplomat
            assignments,
        };

        let diplomat = Diplomat::new(&db);

        // 1. Check Bridge B
        // Neighbors: L1 (Cluster 0), R1 (Cluster 1)
        // Counts: {0: 1, 1: 1}
        // Total: 2
        // p0 = 0.5, p1 = 0.5
        // Entropy = -(0.5 * log2(0.5) + 0.5 * log2(0.5)) = -(-0.5 + -0.5) = 1.0
        let score_b = diplomat.calculate_diplomatic_score(b, &clustering).unwrap();
        assert!((score_b - 1.0).abs() < 1e-5, "Bridge score should be 1.0");

        // 2. Check L1
        // Neighbors: L2 (Cluster 0), B (Cluster 2)
        // Counts: {0: 1, 2: 1}
        // Entropy = 1.0 (It also bridges to the bridge!)
        let score_l1 = diplomat
            .calculate_diplomatic_score(l1, &clustering)
            .unwrap();
        assert!(
            (score_l1 - 1.0).abs() < 1e-5,
            "L1 score should be 1.0 as it connects to bridge"
        );

        // 3. Check L2
        // Neighbors: L1 (Cluster 0) [From incoming edge L1->L2]
        // Counts: {0: 1}
        // Entropy = 0.0
        let score_l2 = diplomat
            .calculate_diplomatic_score(l2, &clustering)
            .unwrap();
        assert_eq!(score_l2, 0.0, "L2 score should be 0.0 (pure internal)");
    }

    #[test]
    fn test_diplomat_identify_bridges() {
        let db = AletheiaDB::new().unwrap();
        let config = HnswConfig::new(2, DistanceMetric::Euclidean);
        db.enable_vector_index("embedding", config).unwrap();

        let props = PropertyMapBuilder::new()
            .insert_vector("embedding", &[0.0, 0.0])
            .build();
        let n1 = db.create_node("N", props.clone()).unwrap(); // Cluster 0
        let n2 = db.create_node("N", props.clone()).unwrap(); // Cluster 1
        let n3 = db.create_node("N", props.clone()).unwrap(); // Cluster 2
        let hub = db.create_node("Hub", props.clone()).unwrap(); // Cluster 3

        // Hub connects to all 3
        db.create_edge(hub, n1, "LINK", Default::default()).unwrap();
        db.create_edge(hub, n2, "LINK", Default::default()).unwrap();
        db.create_edge(hub, n3, "LINK", Default::default()).unwrap();

        let mut assignments = HashMap::new();
        assignments.insert(n1, 0);
        assignments.insert(n2, 1);
        assignments.insert(n3, 2);
        assignments.insert(hub, 3);

        let clustering = ClusteringResult {
            centroids: vec![],
            assignments,
        };

        let diplomat = Diplomat::new(&db);
        let bridges = diplomat.identify_bridges(&clustering, 10).unwrap();

        assert!(!bridges.is_empty());
        assert_eq!(bridges[0].0, hub);
        // Entropy for 3 distinct clusters (1/3 each)
        // -3 * (1/3 * log2(1/3)) = -log2(1/3) = log2(3) ≈ 1.585
        assert!(bridges[0].1 > 1.5);
    }
}
