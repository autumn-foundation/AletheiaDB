//! "The Cartographer" - Semantic Graph Clustering
//!
//! This module implements functionality to analyze the vector space of the graph,
//! identify natural clusters, and reify them as explicit "Region" nodes.

use crate::core::NodeId;
use crate::{AletheiaDB, PropertyMapBuilder, WriteOps};
use std::collections::HashMap;

/// The result of a clustering operation.
#[derive(Debug, Clone)]
pub struct ClusteringResult {
    /// The centroids of the clusters.
    pub centroids: Vec<Vec<f32>>,
    /// Mapping of node ID to cluster index.
    pub assignments: HashMap<NodeId, usize>,
}

/// The Cartographer maps the semantic landscape of the graph.
pub struct Cartographer<'a> {
    pub(crate) db: &'a AletheiaDB,
}

impl<'a> Cartographer<'a> {
    /// Create a new Cartographer instance.
    pub fn new(db: &'a AletheiaDB) -> Self {
        Self { db }
    }

    /// Analyzes the graph to find clusters based on the given vector property.
    pub fn analyze(&self, property: &str, k: usize) -> crate::utils::Result<ClusteringResult> {
        // Step 1: Harvest vectors
        // We use the query engine to scan all nodes.
        let results = self
            .db
            .query()
            .scan(None) // Scan all nodes
            .execute(self.db)?;

        let mut data: Vec<(NodeId, Vec<f32>)> = Vec::new();

        for row_result in results {
            let row = row_result?;
            #[allow(clippy::collapsible_if)]
            if let Some(node) = row.entity.as_node() {
                if let Some(prop_val) = node.get_property(property) {
                    if let Some(vector) = prop_val.as_vector() {
                        // We need to own the vector for clustering
                        data.push((node.id, vector.to_vec()));
                    }
                }
            }
        }

        // Step 2: Cluster
        let kmeans = KMeans::new(k);
        let result = kmeans.train(&data);

        Ok(result)
    }

    /// Reifies the clusters as "Region" nodes in the graph.
    ///
    /// This method creates a "Region" node for each cluster and links all
    /// member nodes to it with a "LOCATED_IN" edge.
    ///
    /// Returns the NodeIds of the created Region nodes.
    pub fn reify(&self, clustering: &ClusteringResult) -> crate::utils::Result<Vec<NodeId>> {
        self.db.write(|tx| {
            let mut region_ids = Vec::new();

            // Create Region nodes
            for (cluster_idx, centroid) in clustering.centroids.iter().enumerate() {
                // Create a "Region" node
                // We store the centroid as a property "centroid"
                let props = PropertyMapBuilder::new()
                    .insert("name", format!("Region {}", cluster_idx))
                    .insert("cluster_id", cluster_idx as i64)
                    .insert_vector("centroid", centroid)
                    .build();

                // We use "Region" as the label
                let region_id = tx.create_node("Region", props)?;
                region_ids.push(region_id);
            }

            // Create edges
            for (node_id, cluster_idx) in &clustering.assignments {
                if *cluster_idx < region_ids.len() {
                    let region_id = region_ids[*cluster_idx];

                    // Link node -> region
                    tx.create_edge(
                        *node_id,
                        region_id,
                        "LOCATED_IN",
                        PropertyMapBuilder::new().build(),
                    )?;
                }
            }

            Ok(region_ids)
        })
    }
}

/// Simple K-Means implementation.
pub(crate) struct KMeans {
    k: usize,
    max_iterations: usize,
}

impl KMeans {
    fn new(k: usize) -> Self {
        Self {
            k,
            max_iterations: 100,
        }
    }

    fn train(&self, data: &[(NodeId, Vec<f32>)]) -> ClusteringResult {
        if data.is_empty() {
            return ClusteringResult {
                centroids: Vec::new(),
                assignments: HashMap::new(),
            };
        }

        let dimensions = data[0].1.len();
        // Handle case where k > data.len()
        let effective_k = self.k.min(data.len());
        let mut centroids: Vec<Vec<f32>> = Vec::with_capacity(effective_k);

        // Initialize centroids deterministically to avoid adding `rand` dependency
        // We pick points spaced out in the input list.
        if effective_k > 0 {
            let step = data.len() / effective_k;
            for i in 0..effective_k {
                let idx = (i * step).min(data.len() - 1);
                centroids.push(data[idx].1.clone());
            }
        }

        let mut assignments: HashMap<NodeId, usize> = HashMap::new();

        for _iteration in 0..self.max_iterations {
            let mut changes = 0;
            let mut new_assignments: HashMap<NodeId, usize> = HashMap::new();
            let mut sums = vec![vec![0.0; dimensions]; effective_k];
            let mut counts = vec![0; effective_k];

            // Assign step
            for (node_id, vector) in data {
                let mut best_cluster = 0;
                let mut best_dist = f32::MAX;

                for (i, centroid) in centroids.iter().enumerate() {
                    let dist = euclidean_distance_sq(vector, centroid);
                    if dist < best_dist {
                        best_dist = dist;
                        best_cluster = i;
                    }
                }

                new_assignments.insert(*node_id, best_cluster);
                if let Some(&old_cluster) = assignments.get(node_id) {
                    if old_cluster != best_cluster {
                        changes += 1;
                    }
                } else {
                    changes += 1;
                }

                // Accumulate for update step
                for (d, val) in vector.iter().enumerate() {
                    sums[best_cluster][d] += val;
                }
                counts[best_cluster] += 1;
            }

            assignments = new_assignments;

            // Update step
            for i in 0..effective_k {
                if counts[i] > 0 {
                    for d in 0..dimensions {
                        centroids[i][d] = sums[i][d] / counts[i] as f32;
                    }
                }
                // If a cluster becomes empty, we could re-initialize it, but for simplicity we leave it.
                // In K-Means, empty clusters can happen.
            }

            if changes == 0 {
                break;
            }
        }

        ClusteringResult {
            centroids,
            assignments,
        }
    }
}

fn euclidean_distance_sq(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b.iter()).map(|(x, y)| (x - y).powi(2)).sum()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::vector::DistanceMetric;
    use crate::index::vector::hnsw::HnswConfig;

    #[test]
    fn test_kmeans_simple() {
        let kmeans = KMeans::new(2);
        let n1 = NodeId::new(1).unwrap();
        let n2 = NodeId::new(2).unwrap();
        let n3 = NodeId::new(3).unwrap();
        let n4 = NodeId::new(4).unwrap();

        // Cluster 1 around (0,0)
        let v1 = vec![0.0, 0.0];
        let v2 = vec![0.1, 0.1];

        // Cluster 2 around (10,10)
        let v3 = vec![10.0, 10.0];
        let v4 = vec![10.1, 10.1];

        let data = vec![(n1, v1), (n2, v2), (n3, v3), (n4, v4)];

        let result = kmeans.train(&data);

        assert_eq!(result.centroids.len(), 2);

        // n1 and n2 should be in same cluster
        let c1 = result.assignments.get(&n1).unwrap();
        let c2 = result.assignments.get(&n2).unwrap();
        assert_eq!(c1, c2, "n1 and n2 should be in the same cluster");

        // n3 and n4 should be in same cluster
        let c3 = result.assignments.get(&n3).unwrap();
        let c4 = result.assignments.get(&n4).unwrap();
        assert_eq!(c3, c4, "n3 and n4 should be in the same cluster");

        // n1 and n3 should be in different clusters
        assert_ne!(c1, c3, "n1 and n3 should be in different clusters");
    }

    #[test]
    fn test_cartographer_workflow() {
        // Need to set up persistence config to avoid writing to disk
        let db = AletheiaDB::new().unwrap();

        // Setup vector index
        let config = HnswConfig::new(2, DistanceMetric::Euclidean);
        db.enable_vector_index("embedding", config).unwrap();

        // Create nodes with vectors
        // Cluster 1
        let n1 = db
            .create_node(
                "Point",
                PropertyMapBuilder::new()
                    .insert_vector("embedding", &[0.0, 0.0])
                    .build(),
            )
            .unwrap();
        let n2 = db
            .create_node(
                "Point",
                PropertyMapBuilder::new()
                    .insert_vector("embedding", &[0.1, 0.1])
                    .build(),
            )
            .unwrap();

        // Cluster 2
        let n3 = db
            .create_node(
                "Point",
                PropertyMapBuilder::new()
                    .insert_vector("embedding", &[10.0, 10.0])
                    .build(),
            )
            .unwrap();
        let n4 = db
            .create_node(
                "Point",
                PropertyMapBuilder::new()
                    .insert_vector("embedding", &[10.1, 10.1])
                    .build(),
            )
            .unwrap();

        let cartographer = Cartographer::new(&db);

        // Analyze
        let result = cartographer.analyze("embedding", 2).unwrap();
        assert_eq!(result.centroids.len(), 2);

        // Reify
        let region_ids = cartographer.reify(&result).unwrap();
        assert_eq!(region_ids.len(), 2);

        // Verify graph structure
        let r1 = region_ids[0];
        let r1_node = db.get_node(r1).unwrap();
        assert!(r1_node.has_label_str("Region"));

        // Check centroid property exists
        assert!(r1_node.get_property("centroid").is_some());

        // Verify edges exist
        // Note: We don't know exactly which region is which (ordering depends on K-Means init),
        // but we can check that edges were created.
        let edges1 = db.get_outgoing_edges_with_label(n1, "LOCATED_IN");
        assert_eq!(edges1.len(), 1);

        let edges3 = db.get_outgoing_edges_with_label(n3, "LOCATED_IN");
        assert_eq!(edges3.len(), 1);

        // Verify n1 and n2 share the same region
        let region1 = db.get_edge_target(edges1[0]).unwrap();
        let edges2 = db.get_outgoing_edges_with_label(n2, "LOCATED_IN");
        let region2 = db.get_edge_target(edges2[0]).unwrap();
        assert_eq!(region1, region2);

        // Verify n3 and n4 share the same region
        let region3 = db.get_edge_target(edges3[0]).unwrap();
        let edges4 = db.get_outgoing_edges_with_label(n4, "LOCATED_IN");
        let region4 = db.get_edge_target(edges4[0]).unwrap();
        assert_eq!(region3, region4);
    }
}
