#![allow(clippy::collapsible_if, clippy::needless_range_loop)]

//! Eclipse: Semantic Shadow Detection 🌑
//!
//! "Who is being silenced in this conversation?"
//!
//! The Eclipse engine analyzes a target node and its neighborhood to find
//! "Eclipsed Nodes" – neighbors whose semantic contribution is entirely
//! overshadowed by another, stronger neighbor.
//!
//! # Concepts
//! - **Target**: The central node (e.g., a "Meeting" or a central "Topic").
//! - **Neighbors**: Nodes connected to the target (e.g., attendees, subtopics).
//! - **Projection**: How much a neighbor's vector aligns with the target's vector.
//! - **Eclipse**: If Neighbor X aligns well with the Target, and Neighbor Y's
//!   projection is completely "covered" (highly similar and shorter) by X's
//!   projection, then Y is "eclipsed" by X.
//!
//! # Use Cases
//! - **Echo Chamber Detection**: Finding nodes that add no new information.
//! - **Meeting Analysis**: "Who didn't get a chance to speak or had their ideas completely subsumed?"
//! - **Query Optimization**: Safely drop eclipsed nodes during summarization without losing semantic breadth.
//!
//! # Example
//! ```rust
//! // Requires features = ["nova"]
//! use aletheiadb::AletheiaDB;
//! use aletheiadb::experimental::eclipse::EclipseEngine;
//! use aletheiadb::core::id::NodeId;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let db = AletheiaDB::new()?;
//! // ... setup graph ...
//! # let target_id = NodeId::new(1)?;
//!
//! let eclipse = EclipseEngine::new(&db);
//! // let result = eclipse.detect_eclipses(target_id, "embedding", 0.9)?;
//! # Ok(())
//! # }
//! ```

use crate::AletheiaDB;
use crate::api::transaction::ReadOps;
use crate::core::error::{Error, Result};
use crate::core::id::NodeId;
use crate::core::vector::ops::{cosine_similarity, dot_product, magnitude};

/// A node that is eclipsed by another node.
#[derive(Debug, Clone, PartialEq)]
pub struct EclipsedNode {
    /// The node that is being eclipsed.
    pub node_id: NodeId,
    /// The node that is overshadowing it.
    pub eclipsed_by: NodeId,
    /// A score representing how completely it is eclipsed (0.0 to 1.0).
    /// Higher means more completely eclipsed.
    pub eclipse_score: f32,
}

/// The result of an eclipse detection.
#[derive(Debug, Clone)]
pub struct EclipseResult {
    /// The target node that was analyzed.
    pub target_id: NodeId,
    /// A list of eclipsed nodes found in the neighborhood.
    pub eclipsed_nodes: Vec<EclipsedNode>,
    /// Nodes that are not eclipsed (they provide unique semantic value).
    pub unique_nodes: Vec<NodeId>,
}

/// The Eclipse Engine for semantic shadow detection.
pub struct EclipseEngine<'a> {
    db: &'a AletheiaDB,
}

impl<'a> EclipseEngine<'a> {
    /// Create a new Eclipse Engine.
    pub fn new(db: &'a AletheiaDB) -> Self {
        Self { db }
    }

    /// Detect eclipsed nodes among a target node's immediate neighbors.
    ///
    /// # Arguments
    /// * `target_id` - The node representing the central topic/meeting.
    /// * `property_name` - The vector property to analyze (e.g., "embedding").
    /// * `similarity_threshold` - Minimum cosine similarity between two neighbors'
    ///   projections for one to eclipse another.
    pub fn detect_eclipses(
        &self,
        target_id: NodeId,
        property_name: &str,
        similarity_threshold: f32,
    ) -> Result<EclipseResult> {
        let mut target_vector = None;
        let mut neighbors = Vec::new();

        self.db.read(|tx| {
            // Get target node
            let target_node = tx.get_node(target_id)?;
            if let Some(prop) = target_node.properties.get(property_name) {
                if let Some(vec) = prop.as_vector() {
                    target_vector = Some(vec.to_vec());
                }
            }

            // Get all neighbors
            for edge_id in tx.get_outgoing_edges(target_id) {
                if let Ok(edge) = tx.get_edge(edge_id) {
                    let neighbor_node = tx.get_node(edge.target)?;
                    if let Some(prop) = neighbor_node.properties.get(property_name) {
                        if let Some(vec) = prop.as_vector() {
                            neighbors.push((edge.target, vec.to_vec()));
                        }
                    }
                }
            }
            for edge_id in tx.get_incoming_edges(target_id) {
                if let Ok(edge) = tx.get_edge(edge_id) {
                    let neighbor_node = tx.get_node(edge.source)?;
                    if let Some(prop) = neighbor_node.properties.get(property_name) {
                        if let Some(vec) = prop.as_vector() {
                            neighbors.push((edge.source, vec.to_vec()));
                        }
                    }
                }
            }

            Ok::<(), Error>(())
        })?;

        let target_vec = target_vector.ok_or_else(|| {
            Error::other(format!(
                "Target node {} lacks vector property {}",
                target_id, property_name
            ))
        })?;

        if neighbors.is_empty() {
            return Ok(EclipseResult {
                target_id,
                eclipsed_nodes: Vec::new(),
                unique_nodes: Vec::new(),
            });
        }

        let target_mag = magnitude(&target_vec);
        if target_mag == 0.0 {
            return Err(Error::other("Target vector magnitude is 0"));
        }

        // Calculate scalar projections onto the target vector
        // projection = (A . B) / |B| where B is target
        let mut projections = Vec::new();
        for (node_id, vec) in &neighbors {
            let dot = dot_product(vec, &target_vec)?;
            let proj_scalar = dot / target_mag; // length of projection
            // If the neighbor is completely orthogonal or opposite, it's not projecting onto the target topic
            if proj_scalar > 0.0 {
                projections.push((*node_id, vec, proj_scalar));
            }
        }

        // Find eclipsed nodes
        // X eclipses Y if:
        // 1. They are highly similar (cosine_sim(X, Y) >= threshold)
        // 2. X's projection length > Y's projection length

        let mut eclipsed = Vec::new();
        let mut unique = Vec::new();

        // We use an O(N^2) comparison for neighbors. For large neighborhoods, this might need an index.
        let mut is_eclipsed_by = std::collections::HashMap::new();

        for i in 0..projections.len() {
            let (node_y, vec_y, proj_y) = projections[i];
            let mut y_eclipsed_by = None;
            let mut max_score = 0.0;

            for j in 0..projections.len() {
                if i == j {
                    continue;
                }
                let (node_x, vec_x, proj_x) = projections[j];

                // Check condition 2: X must cast a larger shadow (have a longer projection)
                if proj_x > proj_y {
                    // Check condition 1: X and Y must be highly similar
                    let sim = cosine_similarity(vec_x, vec_y)?;
                    if sim >= similarity_threshold {
                        // Node Y is eclipsed by Node X
                        if sim > max_score {
                            max_score = sim;
                            y_eclipsed_by = Some(node_x);
                        }
                    }
                }
            }

            if let Some(eclipsing_node) = y_eclipsed_by {
                is_eclipsed_by.insert(node_y, (eclipsing_node, max_score));
            }
        }

        for (node_id, _, _) in &projections {
            if let Some((eclipsing_node, score)) = is_eclipsed_by.get(node_id) {
                eclipsed.push(EclipsedNode {
                    node_id: *node_id,
                    eclipsed_by: *eclipsing_node,
                    eclipse_score: *score,
                });
            } else {
                unique.push(*node_id);
            }
        }

        // Also add any neighbors that had <= 0.0 projection to unique (they are totally off-topic, but not eclipsed by others on-topic)
        let projected_nodes: std::collections::HashSet<_> =
            projections.iter().map(|(id, _, _)| *id).collect();
        for (node_id, _) in &neighbors {
            if !projected_nodes.contains(node_id) {
                unique.push(*node_id);
            }
        }

        Ok(EclipseResult {
            target_id,
            eclipsed_nodes: eclipsed,
            unique_nodes: unique,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::property::PropertyMapBuilder;

    #[test]
    fn test_eclipse_detection() {
        let db = AletheiaDB::new().unwrap();

        // Target (e.g., The Meeting Topic: "AI in Healthcare")
        // Vector: [1.0, 1.0, 0.0]
        let target_props = PropertyMapBuilder::new()
            .insert_vector("embedding", &[1.0, 1.0, 0.0])
            .build();
        let target = db.create_node("Meeting", target_props).unwrap();

        // Node A: Highly aligned, strong magnitude ("Loud Expert")
        // Vector: [0.9, 0.9, 0.1]
        let a_props = PropertyMapBuilder::new()
            .insert_vector("embedding", &[0.9, 0.9, 0.1])
            .build();
        let node_a = db.create_node("Person", a_props).unwrap();
        db.create_edge(target, node_a, "ATTENDED", Default::default())
            .unwrap();

        // Node B: Highly aligned with A, but weaker magnitude ("Quiet Junior")
        // Vector: [0.4, 0.4, 0.05]
        let b_props = PropertyMapBuilder::new()
            .insert_vector("embedding", &[0.4, 0.4, 0.05])
            .build();
        let node_b = db.create_node("Person", b_props).unwrap();
        db.create_edge(target, node_b, "ATTENDED", Default::default())
            .unwrap();

        // Node C: Orthogonal / Unique perspective ("Design Lead")
        // Vector: [0.0, 0.0, 1.0]
        let c_props = PropertyMapBuilder::new()
            .insert_vector("embedding", &[0.0, 0.0, 1.0])
            .build();
        let node_c = db.create_node("Person", c_props).unwrap();
        db.create_edge(target, node_c, "ATTENDED", Default::default())
            .unwrap();

        let engine = EclipseEngine::new(&db);
        let result = engine.detect_eclipses(target, "embedding", 0.9).unwrap();

        // Node B should be eclipsed by Node A
        assert_eq!(result.eclipsed_nodes.len(), 1);
        let eclipsed = &result.eclipsed_nodes[0];
        assert_eq!(eclipsed.node_id, node_b);
        assert_eq!(eclipsed.eclipsed_by, node_a);

        // Nodes A and C should be unique
        assert_eq!(result.unique_nodes.len(), 2);
        assert!(result.unique_nodes.contains(&node_a));
        assert!(result.unique_nodes.contains(&node_c));
    }
}
