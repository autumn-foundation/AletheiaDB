//! Tesseract: Spatial Partitioning & Semantic Clustering.
//!
//! "Discrete answers to continuous questions."
//!
//! Cosine similarity and Euclidean distances are O(N) or O(log N) at best when indexed,
//! but sometimes we don't need exact continuous similarity. Sometimes we just want to know:
//! "Which nodes are in the exact same rough neighborhood?"
//!
//! Tesseract solves this by quantizing continuous vectors into discrete N-dimensional grids.
//! This reduces semantic collision detection to an O(1) hash map lookup.
//!
//! # Concepts
//! - **Quantization**: Mapping floats to integers based on a `resolution`.
//! - **Collision**: When two distinct nodes share the exact same discrete coordinates.
//!
//! # Use Cases
//! - **Blazing fast coarse deduplication**: Finding near-twins across the entire graph in O(N) time instead of O(N^2).
//! - **Spatial hashing**: Assigning discrete buckets to concepts.
//!
//! # Example
//! ```rust,no_run
//! use aletheiadb::AletheiaDB;
//! use aletheiadb::experimental::tesseract::Tesseract;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let db = AletheiaDB::new()?;
//! // ... insert nodes with embeddings ...
//!
//! // Resolution 10.0 means vectors like [0.12] and [0.15] both map to [1]
//! let tesseract = Tesseract::new(&db, 10.0);
//!
//! // Find all buckets with multiple nodes (collisions)
//! let buckets = tesseract.investigate("embedding")?;
//!
//! for (coord, nodes) in buckets {
//!     if nodes.len() > 1 {
//!         println!("Collision at {:?}: {} nodes share this space.", coord, nodes.len());
//!     }
//! }
//! # Ok(())
//! # }
//! ```

use crate::AletheiaDB;
use crate::core::error::Result;
use crate::core::id::NodeId;
use std::collections::HashMap;

/// The Tesseract Engine for spatial partitioning.
pub struct Tesseract<'a> {
    db: &'a AletheiaDB,
    /// The multiplier used for quantization. Higher resolution means more precision (smaller buckets).
    resolution: f32,
}

impl<'a> Tesseract<'a> {
    /// Create a new Tesseract instance.
    pub fn new(db: &'a AletheiaDB, resolution: f32) -> Self {
        Self { db, resolution }
    }

    /// Quantize a continuous vector into discrete integer coordinates.
    pub fn quantize(&self, vector: &[f32]) -> Vec<i32> {
        vector
            .iter()
            .map(|&v| (v * self.resolution).round() as i32)
            .collect()
    }

    /// Scan the entire graph and bucket nodes by their quantized vector.
    ///
    /// # Arguments
    /// * `vector_property` - The property name containing the node embeddings.
    #[allow(clippy::collapsible_if)]
    pub fn investigate(&self, vector_property: &str) -> Result<HashMap<Vec<i32>, Vec<NodeId>>> {
        let mut buckets: HashMap<Vec<i32>, Vec<NodeId>> = HashMap::new();

        let scan_results = self.db.query().scan(None).execute(self.db)?;

        for row_result in scan_results {
            let row = row_result?;
            if let Some(node) = row.entity.as_node() {
                if let Some(prop) = node.properties.get(vector_property) {
                    if let Some(vec) = prop.as_vector() {
                        let coord = self.quantize(vec);
                        buckets.entry(coord).or_default().push(node.id);
                    }
                }
            }
        }

        Ok(buckets)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::property::PropertyMapBuilder;

    #[test]
    fn test_quantize() {
        let db = AletheiaDB::new().unwrap();
        // Resolution 10.0 -> multiply by 10, then round
        // 0.12 * 10 = 1.2 -> 1
        // 0.15 * 10 = 1.5 -> 2
        // -0.12 * 10 = -1.2 -> -1
        let tesseract = Tesseract::new(&db, 10.0);

        assert_eq!(tesseract.quantize(&[0.12]), vec![1]);
        assert_eq!(tesseract.quantize(&[0.15]), vec![2]);
        assert_eq!(tesseract.quantize(&[-0.12]), vec![-1]);
        assert_eq!(tesseract.quantize(&[1.0, 0.0]), vec![10, 0]);

        let tesseract_low_res = Tesseract::new(&db, 1.0);
        // 0.12 * 1 = 0.12 -> 0
        // 0.15 * 1 = 0.15 -> 0
        assert_eq!(tesseract_low_res.quantize(&[0.12, 0.15]), vec![0, 0]);
    }

    #[test]
    fn test_investigate_collisions() {
        let db = AletheiaDB::new().unwrap();

        // Node A: [0.12, 0.0]
        let a = db
            .create_node(
                "Person",
                PropertyMapBuilder::new()
                    .insert_vector("vec", &[0.12, 0.0])
                    .build(),
            )
            .unwrap();

        // Node B: [0.11, 0.0] (Should collide with A at res 10: 1.1 -> 1, 1.2 -> 1)
        let b = db
            .create_node(
                "Person",
                PropertyMapBuilder::new()
                    .insert_vector("vec", &[0.11, 0.0])
                    .build(),
            )
            .unwrap();

        // Node C: [0.99, 0.0] (Should NOT collide with A/B at res 10)
        let c = db
            .create_node(
                "Person",
                PropertyMapBuilder::new()
                    .insert_vector("vec", &[0.99, 0.0])
                    .build(),
            )
            .unwrap();

        let tesseract = Tesseract::new(&db, 10.0);
        let buckets = tesseract.investigate("vec").unwrap();

        let coord_ab = vec![1, 0];
        let bucket_ab = buckets.get(&coord_ab).unwrap();
        assert_eq!(bucket_ab.len(), 2);
        assert!(bucket_ab.contains(&a));
        assert!(bucket_ab.contains(&b));

        let coord_c = vec![10, 0];
        let bucket_c = buckets.get(&coord_c).unwrap();
        assert_eq!(bucket_c.len(), 1);
        assert!(bucket_c.contains(&c));
    }
}
