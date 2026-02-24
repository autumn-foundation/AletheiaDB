//! Chameleon: Context-Aware Faceted Search.
//!
//! "Discover the hidden dimensions of your nodes."
//!
//! Chameleon analyzes a node's local context (neighbors) to identify distinct semantic "Aspects".
//! It then allows you to search the global graph using these aspects as query vectors.
//!
//! # Concept
//!
//! A node often has multiple meanings depending on context.
//! "Apple" (Node) might be connected to "iPhone" (Tech context) and "Banana" (Fruit context).
//! Standard vector search uses the average "Apple" vector, which might be a muddled middle-ground.
//!
//! Chameleon decomposes the neighborhood into clusters (Aspects), effectively saying:
//! "This node has a 'Tech' aspect and a 'Fruit' aspect."
//! You can then search for "Other nodes like Apple's Tech aspect" (returns Microsoft, Google)
//! or "Other nodes like Apple's Fruit aspect" (returns Orange, Pear).
//!
//! # Use Cases
//! - **Disentangled Recommendations**: "Because you liked 'The Matrix' (Action Aspect)..."
//! - **Exploratory Search**: "Show me the different 'flavors' of this concept."
//! - **Contextual Disambiguation**: clarifying polysemous nodes.
//!
//! # Example
//!
//! ```rust,no_run
//! use aletheiadb::AletheiaDB;
//! use aletheiadb::experimental::chameleon::Chameleon;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let db = AletheiaDB::new()?;
//! let chameleon = Chameleon::new(&db);
//!
//! // 1. Analyze Context
//! // Find 2 distinct aspects of Node 123
//! let aspects = chameleon.analyze_context(123.into(), "embedding", 2)?;
//!
//! for (i, aspect) in aspects.iter().enumerate() {
//!     println!("Aspect {}: Weight {:.2}", i, aspect.weight);
//!     println!("  Exemplars: {:?}", aspect.exemplars);
//! }
//!
//! // 2. Faceted Search
//! // Search for nodes similar to Aspect 0 (e.g., the 'Tech' aspect)
//! let results = chameleon.facet_search(123.into(), "embedding", 0, 10)?;
//!
//! for (node, score) in results {
//!     println!("Found similar node: {} (score: {})", node, score);
//! }
//! # Ok(())
//! # }
//! ```

use crate::AletheiaDB;
use crate::core::error::{Error, Result, VectorError};
use crate::core::id::NodeId;
use crate::core::vector::ops::normalize;

/// A distinct semantic aspect of a node's context.
#[derive(Debug, Clone)]
pub struct Aspect {
    /// The centroid vector of this aspect (normalized).
    pub centroid: Vec<f32>,
    /// The weight of this aspect (fraction of neighbors in this cluster).
    pub weight: f32,
    /// Representative neighbors for this aspect (closest to centroid).
    pub exemplars: Vec<NodeId>,
}

/// The Chameleon Engine.
pub struct Chameleon<'a> {
    db: &'a AletheiaDB,
}

impl<'a> Chameleon<'a> {
    /// Create a new Chameleon instance.
    pub fn new(db: &'a AletheiaDB) -> Self {
        Self { db }
    }

    /// Analyze the semantic context of a node and decompose it into aspects.
    ///
    /// # Arguments
    /// * `node_id` - The node to analyze.
    /// * `property` - The vector property to use.
    /// * `k` - The number of aspects to find.
    pub fn analyze_context(
        &self,
        node_id: NodeId,
        property: &str,
        k: usize,
    ) -> Result<Vec<Aspect>> {
        const MAX_CONTEXT_NEIGHBORS: usize = 100;

        // 1. Gather Neighbors
        let neighbor_ids = self.get_neighbors(node_id)?;
        if neighbor_ids.is_empty() {
            return Ok(Vec::new());
        }

        // Limit to prevent DoS
        let neighbor_iter = neighbor_ids.iter().take(MAX_CONTEXT_NEIGHBORS);

        // 2. Extract Vectors
        let mut data = Vec::with_capacity(neighbor_ids.len().min(MAX_CONTEXT_NEIGHBORS));
        let mut expected_dim = None;

        for &nid in neighbor_iter {
            if let Ok(vec) = self.get_node_vector(nid, property) {
                match expected_dim {
                    None => {
                        // First valid vector establishes the dimension
                        expected_dim = Some(vec.len());
                        data.push((nid, vec));
                    }
                    Some(dim) => {
                        // Subsequent vectors must match
                        if vec.len() == dim {
                            data.push((nid, vec));
                        }
                        // Else: ignore mismatching vectors to prevent panic/garbage
                    }
                }
            }
        }

        if data.is_empty() {
            return Ok(Vec::new());
        }

        // 3. Cluster (K-Means)
        // If k > data.len(), we clamp it.
        let effective_k = k.min(data.len());
        if effective_k == 0 {
            return Ok(Vec::new());
        }

        let clusters = MiniKMeans::cluster(&data, effective_k);

        // 4. Build Aspects
        let mut aspects = Vec::with_capacity(effective_k);
        let total_points = data.len() as f32;

        for cluster in clusters {
            if cluster.points.is_empty() {
                continue;
            }

            let weight = cluster.points.len() as f32 / total_points;

            // Find exemplars (closest to centroid)
            let mut points_with_dist: Vec<(NodeId, f32)> = cluster
                .points
                .iter()
                .map(|&(nid, ref vec)| {
                    let dist = dist_sq(vec, &cluster.centroid);
                    (nid, dist)
                })
                .collect();

            // Sort by distance (ascending)
            points_with_dist
                .sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));

            let exemplars: Vec<NodeId> = points_with_dist
                .into_iter()
                .take(3)
                .map(|(nid, _)| nid)
                .collect();

            aspects.push(Aspect {
                centroid: normalize(&cluster.centroid), // Ensure aspect vector is unit length
                weight,
                exemplars,
            });
        }

        // Sort aspects by weight (descending)
        aspects.sort_by(|a, b| {
            b.weight
                .partial_cmp(&a.weight)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        Ok(aspects)
    }

    /// Perform a faceted search using a specific aspect of a node.
    ///
    /// # Arguments
    /// * `node_id` - The source node.
    /// * `property` - The vector property.
    /// * `aspect_index` - The index of the aspect to use (from `analyze_context`).
    /// * `limit` - Max results.
    pub fn facet_search(
        &self,
        node_id: NodeId,
        property: &str,
        aspect_index: usize,
        limit: usize,
    ) -> Result<Vec<(NodeId, f32)>> {
        // We assume a standard k=5 context analysis for ad-hoc search
        let k = 5;
        let aspects = self.analyze_context(node_id, property, k)?;

        if aspect_index >= aspects.len() {
            return Err(Error::Vector(VectorError::IndexError(format!(
                "Aspect index {} out of bounds (found {} aspects)",
                aspect_index,
                aspects.len()
            ))));
        }

        let aspect = &aspects[aspect_index];
        self.db.search_vectors_in(property, &aspect.centroid, limit)
    }

    // --- Helpers ---

    fn get_neighbors(&self, node_id: NodeId) -> Result<Vec<NodeId>> {
        let outgoing = self.db.get_outgoing_edges(node_id);
        let mut neighbors = Vec::with_capacity(outgoing.len());
        for edge_id in outgoing {
            let edge = self.db.get_edge(edge_id)?;
            neighbors.push(edge.target);
        }
        Ok(neighbors)
    }

    fn get_node_vector(&self, node_id: NodeId, property: &str) -> Result<Vec<f32>> {
        let node = self.db.get_node(node_id)?;
        let val = node.properties.get(property).ok_or_else(|| {
            Error::Vector(VectorError::IndexError(format!(
                "Property '{}' missing",
                property
            )))
        })?;
        let vec = val.as_vector().ok_or_else(|| {
            Error::Vector(VectorError::IndexError(format!(
                "Property '{}' not a vector",
                property
            )))
        })?;
        Ok(vec.to_vec())
    }
}

// --- Internal Clustering Logic ---

struct Cluster {
    centroid: Vec<f32>,
    points: Vec<(NodeId, Vec<f32>)>,
}

struct MiniKMeans;

impl MiniKMeans {
    fn cluster(data: &[(NodeId, Vec<f32>)], k: usize) -> Vec<Cluster> {
        if data.is_empty() || k == 0 {
            return Vec::new();
        }

        let dim = data[0].1.len();

        // 1. Initialize Centroids (Deterministically: First k points)
        let mut centroids: Vec<Vec<f32>> = data.iter().take(k).map(|(_, v)| v.clone()).collect();

        let mut assignments = vec![0; data.len()];
        let max_iters = 20;

        for _ in 0..max_iters {
            let mut changes = 0;
            let mut sums = vec![vec![0.0; dim]; k];
            let mut counts = vec![0; k];

            // Assign
            for (i, (_, vec)) in data.iter().enumerate() {
                let mut best_cluster = 0;
                let mut best_dist = f32::MAX;

                for (c_idx, centroid) in centroids.iter().enumerate() {
                    let dist = dist_sq(vec, centroid);
                    if dist < best_dist {
                        best_dist = dist;
                        best_cluster = c_idx;
                    }
                }

                if assignments[i] != best_cluster {
                    assignments[i] = best_cluster;
                    changes += 1;
                }

                // Accumulate
                for (d, val) in vec.iter().enumerate() {
                    sums[best_cluster][d] += val;
                }
                counts[best_cluster] += 1;
            }

            // Update
            for c_idx in 0..k {
                if counts[c_idx] > 0 {
                    for d in 0..dim {
                        centroids[c_idx][d] = sums[c_idx][d] / counts[c_idx] as f32;
                    }
                }
            }

            if changes == 0 {
                break;
            }
        }

        // Build result
        let mut clusters = Vec::with_capacity(k);
        for centroid in centroids {
            clusters.push(Cluster {
                centroid,
                points: Vec::new(),
            });
        }

        for (i, (nid, vec)) in data.iter().enumerate() {
            let c_idx = assignments[i];
            clusters[c_idx].points.push((*nid, vec.clone()));
        }

        clusters
    }
}

fn dist_sq(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b.iter()).map(|(x, y)| (x - y).powi(2)).sum()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::property::PropertyMapBuilder;
    use crate::index::vector::{DistanceMetric, HnswConfig};

    #[test]
    fn test_chameleon_clustering() {
        let db = AletheiaDB::new().unwrap();
        let config = HnswConfig::new(2, DistanceMetric::Euclidean);
        db.enable_vector_index("vec", config).unwrap();

        // Create Central Node
        let center = db
            .create_node("Center", PropertyMapBuilder::new().build())
            .unwrap();

        // Create Neighbors: 2 Clusters
        // Cluster A: [0, 0], [0.1, 0.1]
        // Cluster B: [10, 10], [10.1, 10.1]

        let n1 = db
            .create_node(
                "A",
                PropertyMapBuilder::new()
                    .insert_vector("vec", &[0.0, 0.0])
                    .build(),
            )
            .unwrap();
        let n2 = db
            .create_node(
                "A",
                PropertyMapBuilder::new()
                    .insert_vector("vec", &[0.1, 0.1])
                    .build(),
            )
            .unwrap();
        let n3 = db
            .create_node(
                "B",
                PropertyMapBuilder::new()
                    .insert_vector("vec", &[10.0, 10.0])
                    .build(),
            )
            .unwrap();
        let n4 = db
            .create_node(
                "B",
                PropertyMapBuilder::new()
                    .insert_vector("vec", &[10.1, 10.1])
                    .build(),
            )
            .unwrap();

        // Connect
        for n in [n1, n2, n3, n4] {
            db.create_edge(center, n, "LINK", PropertyMapBuilder::new().build())
                .unwrap();
        }

        let chameleon = Chameleon::new(&db);
        let aspects = chameleon.analyze_context(center, "vec", 2).unwrap();

        assert_eq!(aspects.len(), 2);
    }

    #[test]
    fn test_chameleon_clustering_orthogonal() {
        let db = AletheiaDB::new().unwrap();
        let config = HnswConfig::new(2, DistanceMetric::Euclidean);
        db.enable_vector_index("vec", config).unwrap();

        let center = db
            .create_node("Center", PropertyMapBuilder::new().build())
            .unwrap();

        // Cluster A: Around X-axis [1, 0]
        let n1 = db
            .create_node(
                "A",
                PropertyMapBuilder::new()
                    .insert_vector("vec", &[1.0, 0.0])
                    .build(),
            )
            .unwrap();
        let n2 = db
            .create_node(
                "A",
                PropertyMapBuilder::new()
                    .insert_vector("vec", &[0.9, 0.1])
                    .build(),
            )
            .unwrap();

        // Cluster B: Around Y-axis [0, 1]
        let n3 = db
            .create_node(
                "B",
                PropertyMapBuilder::new()
                    .insert_vector("vec", &[0.0, 1.0])
                    .build(),
            )
            .unwrap();
        let n4 = db
            .create_node(
                "B",
                PropertyMapBuilder::new()
                    .insert_vector("vec", &[0.1, 0.9])
                    .build(),
            )
            .unwrap();

        for n in [n1, n2, n3, n4] {
            db.create_edge(center, n, "LINK", PropertyMapBuilder::new().build())
                .unwrap();
        }

        let chameleon = Chameleon::new(&db);
        let aspects = chameleon.analyze_context(center, "vec", 2).unwrap();

        assert_eq!(aspects.len(), 2);

        // Verify weights (should be 0.5 each)
        assert!((aspects[0].weight - 0.5).abs() < 0.1);
        assert!((aspects[1].weight - 0.5).abs() < 0.1);

        // Verify centroids
        // Aspect 0 centroid should be close to either [1,0] or [0,1]
        let c0 = &aspects[0].centroid;
        let is_x = (c0[0] - 1.0).abs() < 0.2;
        let is_y = (c0[1] - 1.0).abs() < 0.2;

        assert!(
            is_x || is_y,
            "Centroid 0 {:?} should be near X or Y axis",
            c0
        );
    }
}
