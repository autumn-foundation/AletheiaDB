#![allow(clippy::collapsible_if)]

//! Phantom: Semantic Residue Detector 👻
//!
//! "Are we haunted by the past?"
//!
//! The Phantom Engine detects "Semantic Ghosts" — lingering semantic influence
//! of a node's past state. It identifies situations where a node's vector has
//! changed significantly over time, but its neighbors remain semantically closer
//! to its *historical* (phantom) state than its *current* state.
//!
//! # Concepts
//! - **Phantom Vector**: A historical version of a node's vector.
//! - **Semantic Residue**: The phenomenon where the neighborhood still reflects
//!   the past. High residue means the graph structure is lagging behind the
//!   semantic evolution of the central node.
//!
//! # Use Cases
//! - **Echo Chamber Detection**: An agent changed its mind (vector moved), but
//!   its friends haven't (neighborhood stuck in the past).
//! - **Delayed Propagation**: Identifying bottlenecks where new information hasn't
//!   diffused to neighbors yet.
//!
//! # Example
//! ```rust
//! // Requires features = ["nova"]
//! use aletheiadb::AletheiaDB;
//! use aletheiadb::experimental::phantom::PhantomDetector;
//! use aletheiadb::core::id::NodeId;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let db = AletheiaDB::new()?;
//! // ... setup graph and temporal evolution ...
//!
//! let detector = PhantomDetector::new(&db);
//! # Ok(())
//! # }
//! ```

use crate::AletheiaDB;
use crate::api::transaction::ReadOps;
use crate::core::error::{Error, Result};
use crate::core::id::NodeId;
use crate::core::vector::ops::cosine_similarity;

/// A detected phantom occurrence.
#[derive(Debug, Clone)]
pub struct PhantomResidue {
    /// The node that evolved.
    pub node_id: NodeId,
    /// The average similarity of the neighborhood to the node's CURRENT vector.
    pub current_similarity: f32,
    /// The average similarity of the neighborhood to the node's HISTORICAL vector.
    pub historical_similarity: f32,
    /// The residue score: historical_similarity - current_similarity.
    /// Higher means the neighborhood is more "haunted" by the past.
    pub residue_score: f32,
}

/// The Phantom Engine for detecting semantic ghosts.
pub struct PhantomDetector<'a> {
    db: &'a AletheiaDB,
}

impl<'a> PhantomDetector<'a> {
    /// Create a new Phantom Detector.
    pub fn new(db: &'a AletheiaDB) -> Self {
        Self { db }
    }

    /// Detect semantic residue for a specific node and vector property.
    ///
    /// # Arguments
    /// * `node_id` - The node to investigate.
    /// * `property_name` - The vector property to use for semantic analysis.
    ///
    /// # Returns
    /// An `Option<PhantomResidue>` if the node has a history and neighbors to compare against.
    pub fn detect_residue(
        &self,
        node_id: NodeId,
        property_name: &str,
    ) -> Result<Option<PhantomResidue>> {
        let history = self.db.get_node_history(node_id)?;

        // We need at least 2 versions to have a "past" and a "present"
        if history.versions.len() < 2 {
            return Ok(None);
        }

        // Get the current vector
        let current_version = history.versions.last().unwrap();
        let current_vec = match current_version.properties.get(property_name) {
            Some(p) => p
                .as_vector()
                .ok_or_else(|| Error::other("Property is not a vector"))?,
            None => return Ok(None),
        };

        // Get the oldest historical vector (the "phantom")
        let oldest_version = history.versions.first().unwrap();
        let oldest_vec = match oldest_version.properties.get(property_name) {
            Some(p) => p
                .as_vector()
                .ok_or_else(|| Error::other("Historical property is not a vector"))?,
            None => return Ok(None),
        };

        let mut neighbor_vectors = Vec::new();

        self.db.read(|tx| {
            // Collect neighbors (using both incoming and outgoing for a complete neighborhood)
            let mut neighbors = Vec::new();

            for edge_id in tx.get_outgoing_edges(node_id) {
                if let Ok(edge) = tx.get_edge(edge_id) {
                    neighbors.push(edge.target);
                }
            }

            for edge_id in tx.get_incoming_edges(node_id) {
                if let Ok(edge) = tx.get_edge(edge_id) {
                    neighbors.push(edge.source);
                }
            }

            neighbors.sort();
            neighbors.dedup();

            for n_id in neighbors {
                if let Ok(node) = tx.get_node(n_id) {
                    if let Some(prop) = node.get_property(property_name) {
                        if let Some(vec) = prop.as_vector() {
                            neighbor_vectors.push(vec.to_vec());
                        }
                    }
                }
            }

            Ok::<_, Error>(())
        })?;

        if neighbor_vectors.is_empty() {
            return Ok(None);
        }

        let mut total_current_sim = 0.0;
        let mut total_hist_sim = 0.0;

        for n_vec in &neighbor_vectors {
            total_current_sim += cosine_similarity(current_vec, n_vec).unwrap_or(0.0);
            total_hist_sim += cosine_similarity(oldest_vec, n_vec).unwrap_or(0.0);
        }

        let current_similarity = total_current_sim / neighbor_vectors.len() as f32;
        let historical_similarity = total_hist_sim / neighbor_vectors.len() as f32;
        let residue_score = historical_similarity - current_similarity;

        Ok(Some(PhantomResidue {
            node_id,
            current_similarity,
            historical_similarity,
            residue_score,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::transaction::WriteOps;
    use crate::core::property::PropertyMapBuilder;

    #[test]
    fn test_phantom_detection() {
        let db = AletheiaDB::new().unwrap();

        // Target node starts at [1.0, 0.0] (The Phantom State)
        let n1_props = PropertyMapBuilder::new()
            .insert_vector("vec", &[1.0, 0.0])
            .build();

        // Neighbor node is at [0.9, 0.1] (Close to Phantom State)
        let n2_props = PropertyMapBuilder::new()
            .insert_vector("vec", &[0.9, 0.1])
            .build();

        let mut target_id = NodeId::new(0).unwrap();
        let mut neighbor_id = NodeId::new(0).unwrap();

        db.write(|tx| {
            target_id = tx.create_node("Node", n1_props).unwrap();
            neighbor_id = tx.create_node("Node", n2_props).unwrap();

            // Connect them
            tx.create_edge(
                target_id,
                neighbor_id,
                "KNOWS",
                PropertyMapBuilder::new().build(),
            )
            .unwrap();
            Ok::<_, Error>(())
        })
        .unwrap();

        // Move target node far away to [0.0, 1.0] (Current State)
        let n1_updated = PropertyMapBuilder::new()
            .insert_vector("vec", &[0.0, 1.0])
            .build();

        db.write(|tx| {
            tx.update_node(target_id, n1_updated).unwrap();
            Ok::<_, Error>(())
        })
        .unwrap();

        let detector = PhantomDetector::new(&db);
        let residue = detector.detect_residue(target_id, "vec").unwrap().unwrap();

        // The neighbor is [0.9, 0.1]
        // Historical target is [1.0, 0.0] -> Similarity is high
        // Current target is [0.0, 1.0] -> Similarity is low
        // Thus residue_score should be highly positive (> 0.5)

        assert!(residue.historical_similarity > residue.current_similarity);
        assert!(residue.residue_score > 0.5);
    }

    #[test]
    fn test_no_phantom_when_no_history() {
        let db = AletheiaDB::new().unwrap();

        let n1_props = PropertyMapBuilder::new()
            .insert_vector("vec", &[1.0, 0.0])
            .build();

        let target_id = db
            .write(|tx| Ok::<_, Error>(tx.create_node("Node", n1_props).unwrap()))
            .unwrap();

        let detector = PhantomDetector::new(&db);
        let residue = detector.detect_residue(target_id, "vec").unwrap();

        assert!(residue.is_none());
    }
}
