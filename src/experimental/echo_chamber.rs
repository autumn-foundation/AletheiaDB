//! Echo Chamber: Semantic Orthodoxy Detector.
//!
//! "Are you trapped in a filter bubble?"
//!
//! The Echo Chamber detector calculates an "Echo Score" for a node.
//! It compares a node's semantic vector to the semantic vectors of its structural neighbors.
//!
//! - **Score near 1.0**: The node is surrounded by neighbors that are semantically identical. (Echo Chamber)
//! - **Score near 0.0**: The node is surrounded by orthogonal or diverse neighbors. (Bridge / Diverse)
//! - **Score near -1.0**: The node is surrounded by polar opposites. (Contrarian)
//!
//! # Potential Uses
//! - Detecting filter bubbles in social networks.
//! - Identifying outlier nodes in knowledge graphs that connect disparate domains.

use crate::AletheiaDB;
use crate::core::error::Result;
use crate::core::id::NodeId;

/// A detector for measuring how semantically isolated a node is from its structural neighbors.
pub struct EchoChamber<'a> {
    db: &'a AletheiaDB,
}

impl<'a> EchoChamber<'a> {
    /// Create a new EchoChamber detector.
    pub fn new(db: &'a AletheiaDB) -> Self {
        Self { db }
    }

    /// Calculate the Echo Score for a given node and vector property.
    ///
    /// The score is the average cosine similarity between the node and its outgoing edge targets.
    /// If the node has no outgoing edges or the vectors are missing, it returns `None`.
    pub fn calculate_score(&self, node_id: NodeId, property_name: &str) -> Result<Option<f32>> {
        let center_vec = match self.get_node_vector(node_id, property_name)? {
            Some(v) => v,
            None => return Ok(None),
        };

        let outgoing = self.db.get_outgoing_edges(node_id);
        if outgoing.is_empty() {
            return Ok(None);
        }

        let mut total_sim = 0.0;
        let mut count = 0;

        for edge_id in outgoing {
            let edge = match self.db.get_edge(edge_id) {
                Ok(e) => e,
                Err(_) => continue,
            };
            let neighbor_vec_res = self.get_node_vector(edge.target, property_name);
            let neighbor_vec = match neighbor_vec_res {
                Ok(Some(v)) => v,
                _ => continue,
            };
            if center_vec.len() == neighbor_vec.len() {
                let sim = cosine_similarity(&center_vec, &neighbor_vec);
                total_sim += sim;
                count += 1;
            }
        }

        if count == 0 {
            Ok(None)
        } else {
            Ok(Some(total_sim / (count as f32)))
        }
    }

    fn get_node_vector(&self, node_id: NodeId, property_name: &str) -> Result<Option<Vec<f32>>> {
        let node = self.db.get_node(node_id)?;
        let prop_opt = node.properties.get(property_name);
        let prop = match prop_opt {
            Some(p) => p,
            None => return Ok(None),
        };
        let vec_opt = prop.as_vector();
        match vec_opt {
            Some(vec) => Ok(Some(vec.to_vec())),
            None => Ok(None),
        }
    }
}

fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.is_empty() || a.len() != b.len() {
        return 0.0;
    }
    let mut dot_product = 0.0;
    let mut norm_a = 0.0;
    let mut norm_b = 0.0;
    for i in 0..a.len() {
        dot_product += a[i] * b[i];
        norm_a += a[i] * a[i];
        norm_b += b[i] * b[i];
    }
    if norm_a == 0.0 || norm_b == 0.0 {
        return 0.0;
    }
    dot_product / (norm_a.sqrt() * norm_b.sqrt())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::property::PropertyMapBuilder;
    use crate::index::vector::{DistanceMetric, HnswConfig};

    #[test]
    fn test_echo_chamber_score() {
        let db = AletheiaDB::new().unwrap();
        db.enable_vector_index("vec", HnswConfig::new(2, DistanceMetric::Cosine))
            .unwrap();

        // Node in an echo chamber (all neighbors have similar vectors)
        let n1_props = PropertyMapBuilder::new()
            .insert_vector("vec", &[1.0, 0.0])
            .build();
        let center = db.create_node("User", n1_props).unwrap();

        let n2_props = PropertyMapBuilder::new()
            .insert_vector("vec", &[0.99, 0.1])
            .build();
        let n2 = db.create_node("User", n2_props).unwrap();

        let n3_props = PropertyMapBuilder::new()
            .insert_vector("vec", &[1.0, 0.05])
            .build();
        let n3 = db.create_node("User", n3_props).unwrap();

        db.create_edge(center, n2, "FOLLOWS", PropertyMapBuilder::new().build())
            .unwrap();
        db.create_edge(center, n3, "FOLLOWS", PropertyMapBuilder::new().build())
            .unwrap();

        let echo = EchoChamber::new(&db);
        let score = echo.calculate_score(center, "vec").unwrap().unwrap();
        assert!(score > 0.95, "Score should be close to 1.0, got {}", score);

        // Orthogonal node
        let n4_props = PropertyMapBuilder::new()
            .insert_vector("vec", &[0.0, 1.0])
            .build();
        let center2 = db.create_node("User", n4_props).unwrap();

        db.create_edge(center2, n2, "FOLLOWS", PropertyMapBuilder::new().build())
            .unwrap();
        db.create_edge(center2, n3, "FOLLOWS", PropertyMapBuilder::new().build())
            .unwrap();

        let score2 = echo.calculate_score(center2, "vec").unwrap().unwrap();
        assert!(
            score2 < 0.15,
            "Score should be close to 0.0, got {}",
            score2
        );
    }
}
