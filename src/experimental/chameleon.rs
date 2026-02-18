//! Chameleon 🦎: Context-Adaptive Vector Search.
//!
//! "Does 'Bank' mean river side or financial institution? Ask its neighbors."
//!
//! Chameleon modifies a search query vector by blending it with the "semantic centroid"
//! of a context node's neighborhood. This grounds the query in the local graph topology,
//! resolving polysemy and improving relevance in Graph RAG pipelines.
//!
//! # Concepts
//! - **Context Node**: A node that provides the semantic frame of reference.
//! - **Semantic Centroid**: The average vector of the context node and its neighbors.
//! - **Alpha Blending**: `query = alpha * original_query + (1 - alpha) * centroid`.
//!
//! # Example
//! ```rust,no_run
//! use aletheiadb::AletheiaDB;
//! use aletheiadb::experimental::chameleon::Chameleon;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let db = AletheiaDB::new()?;
//! # let context_node_id = db.create_node("Context", Default::default())?;
//! let chameleon = Chameleon::new(&db);
//!
//! // Search for "Bank" relative to a "River" node
//! let query_vector = vec![0.1f32, 0.2f32]; // Placeholder for "Bank" embedding
//! let results = chameleon.search_with_context(
//!     &query_vector,
//!     context_node_id,
//!     "embedding",
//!     0.7, // 70% query, 30% context
//!     10
//! )?;
//! # Ok(())
//! # }
//! ```

use crate::AletheiaDB;
use crate::core::id::NodeId;
use crate::core::vector::ops::normalize;
use crate::utils::Result;

/// Chameleon Context-Adaptive Search Engine.
pub struct Chameleon<'a> {
    db: &'a AletheiaDB,
}

impl<'a> Chameleon<'a> {
    /// Create a new Chameleon instance.
    pub fn new(db: &'a AletheiaDB) -> Self {
        Self { db }
    }

    /// Search for similar vectors, biasing the query towards the context node's neighborhood.
    ///
    /// # Arguments
    /// * `query_vector` - The original query embedding.
    /// * `context_node_id` - The node providing the semantic context.
    /// * `property_name` - The vector property to use for context (must be indexed).
    /// * `alpha` - Blending factor [0.0, 1.0].
    ///     - `1.0`: Pure query (standard search).
    ///     - `0.0`: Pure context (search for neighbors of neighbors).
    ///     - `0.5`: Equal weight.
    /// * `k` - Number of results to return.
    pub fn search_with_context(
        &self,
        query_vector: &[f32],
        context_node_id: NodeId,
        property_name: &str,
        alpha: f32,
        k: usize,
    ) -> Result<Vec<(NodeId, f32)>> {
        // 1. Validate Alpha
        let alpha = alpha.clamp(0.0, 1.0);
        if (alpha - 1.0).abs() < f32::EPSILON {
            // Optimization: If alpha is 1.0, just do a normal search
            return self.db.search_vectors_in(property_name, query_vector, k);
        }

        // 2. Compute Context Vector (Centroid)
        let context_vector = self.compute_context_centroid(context_node_id, property_name)?;

        // 3. Blend Vectors
        // If context vector is empty (no context found), fall back to original query
        let blended_vector = if context_vector.is_empty() {
            query_vector.to_vec()
        } else {
            self.blend_vectors(query_vector, &context_vector, alpha)
        };

        // 4. Normalize (Important for Cosine Similarity)
        // Most vector indexes (HNSW with Cosine) expect normalized inputs if they optimize for dot product
        // or just to keep magnitude consistent.
        let final_query = normalize(&blended_vector);

        // 5. Execute Search
        self.db.search_vectors_in(property_name, &final_query, k)
    }

    /// Compute the centroid of the context node and its outgoing neighbors.
    fn compute_context_centroid(&self, node_id: NodeId, property_name: &str) -> Result<Vec<f32>> {
        let mut vectors: Vec<Vec<f32>> = Vec::new();

        // Helper to try fetch vector from a node
        let fetch_vector = |nid: NodeId| -> Option<Vec<f32>> {
            // We use get_node to access properties.
            if let Ok(node) = self.db.get_node(nid) {
                if let Some(val) = node.properties.get(property_name) {
                    if let Some(vec) = val.as_vector() {
                        return Some(vec.to_vec());
                    }
                }
            }
            None
        };

        // 1. Add Context Node itself
        if let Some(v) = fetch_vector(node_id) {
            vectors.push(v);
        }

        // 2. Add Neighbors (Outgoing)
        // Accessing CurrentStorage directly for efficiency
        let neighbors = self.db.current.get_outgoing_edges(node_id);
        for edge_id in neighbors {
            if let Ok(target) = self.db.current.get_edge_target(edge_id) {
                if let Some(v) = fetch_vector(target) {
                    vectors.push(v);
                }
            }
        }

        if vectors.is_empty() {
            return Ok(Vec::new());
        }

        // 3. Compute Centroid
        let dim = vectors[0].len();
        let mut centroid = vec![0.0; dim];
        let count = vectors.len() as f32;

        for v in &vectors {
            if v.len() != dim {
                // Skip mismatched dimensions (shouldn't happen if properly indexed but safety first)
                continue;
            }
            for (i, val) in v.iter().enumerate() {
                centroid[i] += val;
            }
        }

        for val in centroid.iter_mut() {
            *val /= count;
        }

        Ok(centroid)
    }

    fn blend_vectors(&self, v1: &[f32], v2: &[f32], alpha: f32) -> Vec<f32> {
        let dim = v1.len();
        // If v2 dimension mismatch, just return v1
        if v2.len() != dim {
            return v1.to_vec();
        }

        let mut result = Vec::with_capacity(dim);
        let beta = 1.0 - alpha;

        for i in 0..dim {
            result.push(v1[i] * alpha + v2[i] * beta);
        }

        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{AletheiaDBConfig, WalConfigBuilder};
    use crate::core::property::PropertyMapBuilder;
    use crate::index::vector::{DistanceMetric, HnswConfig};
    use tempfile::tempdir;

    fn create_test_db() -> (AletheiaDB, tempfile::TempDir) {
        let dir = tempdir().unwrap();
        let config = AletheiaDBConfig::builder()
            .wal(
                WalConfigBuilder::new()
                    .wal_dir(dir.path().to_path_buf())
                    .build(),
            )
            .persistence(crate::storage::index_persistence::PersistenceConfig {
                enabled: false,
                data_dir: dir.path().join("data"),
                ..Default::default()
            })
            .build();
        let db = AletheiaDB::with_unified_config(config).unwrap();
        (db, dir)
    }

    #[test]
    fn test_chameleon_blending() {
        let (db, _dir) = create_test_db();

        // 1. Setup Vector Index
        let config = HnswConfig::new(2, DistanceMetric::Cosine);
        db.enable_vector_index("vec", config).unwrap();

        // 2. Create Context Node (C) at [1.0, 0.0]
        let props_c = PropertyMapBuilder::new()
            .insert_vector("vec", &[1.0f32, 0.0])
            .build();
        let c = db.create_node("Context", props_c).unwrap();

        // 3. Create Target Node (T) at [0.707, 0.707] (45 degrees)
        let props_t = PropertyMapBuilder::new()
            .insert_vector("vec", &[0.707f32, 0.707])
            .build();
        let t = db.create_node("Target", props_t).unwrap();

        // 4. Create Distractor Node (D) at [0.0, 1.0] (90 degrees)
        let props_d = PropertyMapBuilder::new()
            .insert_vector("vec", &[0.0f32, 1.0])
            .build();
        let d = db.create_node("Distractor", props_d).unwrap();

        // 5. Query
        // Original Query: [0.0, 1.0] (matches Distractor perfectly)
        // Context: [1.0, 0.0]
        // Blended (alpha=0.5): [0.5, 0.5] -> normalized -> [0.707, 0.707] (matches Target!)

        let chameleon = Chameleon::new(&db);
        let query = vec![0.0f32, 1.0];

        // Case A: Alpha = 1.0 (Pure Query) -> Should find Distractor
        let results_pure = chameleon
            .search_with_context(&query, c, "vec", 1.0, 1)
            .unwrap();
        assert!(!results_pure.is_empty(), "Should return results");
        assert_eq!(results_pure[0].0, d, "Pure query should find Distractor");

        // Case B: Alpha = 0.5 (Blended) -> Should find Target
        let results_blend = chameleon
            .search_with_context(&query, c, "vec", 0.5, 1)
            .unwrap();
        assert!(!results_blend.is_empty(), "Should return results");
        assert_eq!(results_blend[0].0, t, "Blended query should find Target");
    }

    #[test]
    fn test_chameleon_neighbor_context() {
        let (db, _dir) = create_test_db();

        let config = HnswConfig::new(2, DistanceMetric::Cosine);
        db.enable_vector_index("vec", config).unwrap();

        // Context Node (C) - No vector itself
        let props_c = PropertyMapBuilder::new().build();
        let c = db.create_node("Context", props_c).unwrap();

        // Neighbor 1 (N1) - [1.0, 0.0]
        let props_n1 = PropertyMapBuilder::new()
            .insert_vector("vec", &[1.0f32, 0.0])
            .build();
        let n1 = db.create_node("Neighbor", props_n1).unwrap();

        // Connect C -> N1
        db.create_edge(c, n1, "LINK", PropertyMapBuilder::new().build())
            .unwrap();

        // Target (T) - [0.707, 0.707]
        let props_t = PropertyMapBuilder::new()
            .insert_vector("vec", &[0.707f32, 0.707])
            .build();
        let t = db.create_node("Target", props_t).unwrap();

        // Distractor (D) - [0.0, 1.0]
        let props_d = PropertyMapBuilder::new()
            .insert_vector("vec", &[0.0f32, 1.0])
            .build();
        let _d = db.create_node("Distractor", props_d).unwrap();

        let chameleon = Chameleon::new(&db);
        let query = vec![0.0f32, 1.0]; // Points to D

        // With context from neighbor N1, centroid becomes [1.0, 0.0]
        // Blend 0.5 -> [0.5, 0.5] -> T
        let results = chameleon
            .search_with_context(&query, c, "vec", 0.5, 1)
            .unwrap();
        assert!(!results.is_empty(), "Should return results");
        assert_eq!(
            results[0].0, t,
            "Neighbor context should steer query to Target"
        );
    }

    #[test]
    fn test_chameleon_no_context_fallback() {
        let (db, _dir) = create_test_db();
        let config = HnswConfig::new(2, DistanceMetric::Cosine);
        db.enable_vector_index("vec", config).unwrap();

        // Node with no vector and no neighbors
        let props_c = PropertyMapBuilder::new().build();
        let c = db.create_node("Context", props_c).unwrap();

        // Target (matches query [1.0, 0.0])
        let props_t = PropertyMapBuilder::new()
            .insert_vector("vec", &[1.0f32, 0.0])
            .build();
        let t = db.create_node("Target", props_t).unwrap();

        let chameleon = Chameleon::new(&db);
        let query = vec![1.0f32, 0.0];

        // Even with alpha=0.0 (pure context), if no context exists, it should fallback to query
        // and find the target.
        let results = chameleon
            .search_with_context(&query, c, "vec", 0.0, 1)
            .unwrap();

        assert!(!results.is_empty());
        assert_eq!(
            results[0].0, t,
            "Should fallback to original query when no context available"
        );
    }
}
