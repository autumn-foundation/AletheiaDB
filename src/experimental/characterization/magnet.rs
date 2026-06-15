//! Magnet: Semantic Center of Mass Calculator 🧲
//!
//! "Where is the center of gravity for this community?"
//!
//! Magnet takes a set of nodes and calculates their "center of mass" in the semantic
//! vector space. This allows you to find the consensus vector, the average opinion,
//! or the focal point of a given subgraph.
//!
//! # Example
//!
//! ```rust,no_run
//! use aletheiadb::AletheiaDB;
//! use aletheiadb::experimental::characterization::magnet::Magnet;
//! use aletheiadb::core::id::NodeId;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let db = AletheiaDB::new()?;
//! let magnet = Magnet::new(&db);
//!
//! let nodes = vec![NodeId::new(1)?, NodeId::new(2)?, NodeId::new(3)?];
//!
//! // Calculate the center of mass for their "embedding" vectors
//! if let Some(center) = magnet.center_of_mass(&nodes, "embedding")? {
//!     println!("Consensus vector: {:?}", center);
//! }
//! # Ok(())
//! # }
//! ```

use crate::AletheiaDB;
use crate::core::error::Result;
use crate::core::id::NodeId;

/// The Magnet engine for computing semantic center of mass.
pub struct Magnet<'a> {
    db: &'a AletheiaDB,
}

impl<'a> Magnet<'a> {
    /// Create a new Magnet engine.
    pub fn new(db: &'a AletheiaDB) -> Self {
        Self { db }
    }

    /// Calculates the normalized center of mass for a set of nodes based on a specific vector property.
    ///
    /// The center of mass is the element-wise average of all valid vectors, normalized
    /// to remain on the unit hypersphere.
    ///
    /// # Arguments
    ///
    /// * `nodes` - The list of node IDs to aggregate.
    /// * `property` - The name of the vector property to use (e.g., "embedding").
    ///
    /// # Returns
    ///
    /// The normalized center of mass vector, or `None` if no nodes had valid vectors.
    pub fn center_of_mass(&self, nodes: &[NodeId], property: &str) -> Result<Option<Vec<f32>>> {
        let mut sum_vec: Option<Vec<f32>> = None;
        let mut count = 0;

        for &node_id in nodes {
            if let Ok(node) = self.db.get_node(node_id) {
                if let Some(val) = node.properties.get(property) {
                    if let Some(vec) = val.as_vector() {
                        match sum_vec.as_mut() {
                            Some(sum) => {
                                if sum.len() == vec.len() {
                                    for i in 0..sum.len() {
                                        sum[i] += vec[i];
                                    }
                                    count += 1;
                                }
                            }
                            None => {
                                sum_vec = Some(vec.to_vec());
                                count += 1;
                            }
                        }
                    }
                }
            }
        }

        if count == 0 {
            return Ok(None);
        }

        // Apply average and normalize
        if let Some(mut sum) = sum_vec {
            let mut sq_mag = 0.0;
            for val in sum.iter_mut() {
                *val /= count as f32;
                sq_mag += (*val) * (*val);
            }
            if sq_mag > 0.0 {
                let inv_mag = 1.0 / sq_mag.sqrt();
                for val in sum.iter_mut() {
                    *val *= inv_mag;
                }
            } else {
                for val in sum.iter_mut() {
                    *val = 0.0;
                }
            }
            Ok(Some(sum))
        } else {
            Ok(None)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::PropertyMapBuilder;

    #[test]
    fn test_magnet_center_of_mass() {
        let db = AletheiaDB::new().unwrap();

        let props1 = PropertyMapBuilder::new()
            .insert("vec", vec![1.0f32, 0.0])
            .build();
        let node1 = db.create_node("Node", props1).unwrap();

        let props2 = PropertyMapBuilder::new()
            .insert("vec", vec![0.0f32, 1.0])
            .build();
        let node2 = db.create_node("Node", props2).unwrap();

        let magnet = Magnet::new(&db);

        let center = magnet
            .center_of_mass(&[node1, node2], "vec")
            .unwrap()
            .unwrap();

        // Average of [1.0, 0.0] and [0.0, 1.0] is [0.5, 0.5]
        // Normalized: [0.70710677, 0.70710677]
        let expected_val = 0.5f32.sqrt();
        assert!((center[0] - expected_val).abs() < 1e-5);
        assert!((center[1] - expected_val).abs() < 1e-5);
    }

    #[test]
    fn test_magnet_missing_properties_ignored() {
        let db = AletheiaDB::new().unwrap();

        let props1 = PropertyMapBuilder::new()
            .insert("vec", vec![1.0f32, 0.0])
            .build();
        let node1 = db.create_node("Node", props1).unwrap();

        let props2 = PropertyMapBuilder::new().insert("vec", "not a vector").build();
        let node2 = db.create_node("Node", props2).unwrap();

        let magnet = Magnet::new(&db);

        let center = magnet
            .center_of_mass(&[node1, node2], "vec")
            .unwrap()
            .unwrap();

        // Node2 is ignored, center should just be Node1's normalized vector
        assert!((center[0] - 1.0).abs() < 1e-5);
        assert!((center[1] - 0.0).abs() < 1e-5);
    }
}
