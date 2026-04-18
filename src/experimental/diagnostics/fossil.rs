//! Fossil: Semantic Stagnation Detector.
//!
//! "Why is everyone moving on without me?"
//!
//! The Fossil Detector identifies nodes that remain semantically stagnant (their vectors don't change)
//! while their surrounding structural context (their neighbors) experiences significant semantic shift.
//!
//! # Concepts
//! - **Node Displacement**: The Euclidean distance a node's vector moves over a time window.
//! - **Context Displacement**: The average Euclidean distance the node's neighbors move over the same window.
//! - **Fossil Index**: A ratio of Context Displacement to Node Displacement.
//!   A high index means the node is a "Fossil"—stuck in the past while the world moves on.
//!
//! # Example
//! ```rust,no_run
//! use aletheiadb::AletheiaDB;
//! use aletheiadb::experimental::fossil::FossilDetector;
//! use aletheiadb::core::temporal::{TimeRange, time};
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let db = AletheiaDB::new()?;
//! # let node_id = db.create_node("Node", Default::default())?;
//! let detector = FossilDetector::new(&db);
//!
//! let start = time::now() - 3600 * 1_000_000 * 24 * 7; // Last week
//! let end = time::now(); // Now
//! let window = TimeRange::new(start, end).unwrap();
//!
//! let result = detector.detect_fossil(node_id, window, "embedding")?;
//!
//! if result.fossil_index > 2.0 {
//!     println!("Node {} is a Fossil! Neighbors moved {}x more.", node_id, result.fossil_index);
//! }
//! # Ok(())
//! # }
//! ```

use crate::AletheiaDB;
use crate::core::error::Result;
use crate::core::id::NodeId;
use crate::core::temporal::TimeRange;
use std::collections::HashSet;

/// The result of a Fossil analysis.
#[derive(Debug, Clone, PartialEq)]
pub struct FossilResult {
    /// How much the node itself moved semantically.
    pub node_displacement: f32,
    /// How much its neighbors moved on average.
    pub context_displacement: f32,
    /// The ratio of context_displacement to node_displacement.
    /// High = Fossil.
    pub fossil_index: f32,
}

/// The Fossil Detector Engine.
pub struct FossilDetector<'a> {
    db: &'a AletheiaDB,
}

impl<'a> FossilDetector<'a> {
    /// Create a new FossilDetector instance.
    pub fn new(db: &'a AletheiaDB) -> Self {
        Self { db }
    }

    /// Detect if a node is a semantic fossil over a given time window.
    pub fn detect_fossil(
        &self,
        node_id: NodeId,
        window: TimeRange,
        property_name: &str,
    ) -> Result<FossilResult> {
        // 1. Calculate node displacement
        let node_displacement = self.calculate_displacement(node_id, window, property_name)?;

        // 2. Find neighbors
        let mut neighbors = HashSet::new();
        let outgoing = self.db.get_outgoing_edges(node_id);
        for edge_id in outgoing {
            if let Ok(target) = self.db.get_edge_target(edge_id) {
                neighbors.insert(target);
            }
        }
        let incoming = self.db.get_incoming_edges(node_id);
        for edge_id in incoming {
            if let Ok(source) = self.db.get_edge_source(edge_id) {
                neighbors.insert(source);
            }
        }

        // 3. Calculate context displacement
        let mut context_displacement_sum = 0.0;
        let mut valid_neighbors = 0;

        for neighbor in neighbors {
            if let Ok(disp) = self.calculate_displacement(neighbor, window, property_name) {
                context_displacement_sum += disp;
                valid_neighbors += 1;
            }
        }

        let context_displacement = if valid_neighbors > 0 {
            context_displacement_sum / valid_neighbors as f32
        } else {
            0.0
        };

        // 4. Calculate fossil index
        let epsilon = 1e-6_f32; // Prevent division by zero
        let fossil_index = context_displacement / (node_displacement + epsilon);

        Ok(FossilResult {
            node_displacement,
            context_displacement,
            fossil_index,
        })
    }

    /// Calculate the semantic displacement of a single node over a time window.
    fn calculate_displacement(
        &self,
        node_id: NodeId,
        window: TimeRange,
        property_name: &str,
    ) -> Result<f32> {
        // Get state at start of window
        let start_state = self
            .db
            .get_nodes_at_time(&[node_id], window.start(), window.start())?;

        // Get state at end of window
        let end_state = self
            .db
            .get_nodes_at_time(&[node_id], window.end(), window.end())?;

        let start_vec = start_state
            .first()
            .and_then(|(_, opt)| opt.as_ref())
            .and_then(|node| node.get_property(property_name))
            .and_then(|prop| prop.as_vector());

        let end_vec = end_state
            .first()
            .and_then(|(_, opt)| opt.as_ref())
            .and_then(|node| node.get_property(property_name))
            .and_then(|prop| prop.as_vector());

        match (start_vec, end_vec) {
            (Some(s_vec), Some(e_vec)) => {
                if s_vec.len() != e_vec.len() {
                    return Err(crate::core::error::Error::other(
                        "Dimension mismatch between start and end vectors",
                    ));
                }

                let mut dist_sq = 0.0;
                for i in 0..s_vec.len() {
                    let diff = e_vec[i] - s_vec[i];
                    dist_sq += diff * diff;
                }
                Ok(dist_sq.sqrt())
            }
            _ => {
                // Node might not have existed at start or end, or missing property.
                // For simplicity, we can treat missing -> existing as a total change,
                // or just return 0.0. Let's return an error so the caller skips it.
                Err(crate::core::error::Error::other(
                    "Node missing vector property at start or end of window",
                ))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::transaction::WriteOps;
    use crate::core::property::PropertyMapBuilder;
    use crate::core::temporal::time;
    use std::time::Duration;

    #[test]
    fn test_fossil_detection() {
        let db = AletheiaDB::new().unwrap();

        // 1. Setup initial state at T0
        let mut fossil_node = NodeId::new(0).unwrap();
        let mut neighbor_a = NodeId::new(0).unwrap();
        let mut neighbor_b = NodeId::new(0).unwrap();

        db.write(|tx| {
            fossil_node = tx
                .create_node(
                    "Concept",
                    PropertyMapBuilder::new()
                        .insert_vector("vec", &[0.0, 0.0])
                        .build(),
                )
                .unwrap();

            neighbor_a = tx
                .create_node(
                    "Concept",
                    PropertyMapBuilder::new()
                        .insert_vector("vec", &[1.0, 0.0])
                        .build(),
                )
                .unwrap();

            neighbor_b = tx
                .create_node(
                    "Concept",
                    PropertyMapBuilder::new()
                        .insert_vector("vec", &[-1.0, 0.0])
                        .build(),
                )
                .unwrap();

            tx.create_edge(fossil_node, neighbor_a, "LINK", Default::default())
                .unwrap();
            tx.create_edge(fossil_node, neighbor_b, "LINK", Default::default())
                .unwrap();

            Ok::<(), crate::core::error::Error>(())
        })
        .unwrap();

        std::thread::sleep(Duration::from_millis(10));
        let time_start = time::now();
        std::thread::sleep(Duration::from_millis(50));

        // 2. Setup final state at T1
        db.write(|tx| {
            // Fossil stays the same (we update a non-vector property just to make a new version, or don't even update it)
            // Wait, if it's a true fossil, it might not be updated at all.
            // But let's say it's updated with no vector change just to be safe, or just leave it.

            // Neighbors move significantly
            tx.update_node(
                neighbor_a,
                PropertyMapBuilder::new()
                    .insert_vector("vec", &[10.0, 0.0]) // Moved 9 units
                    .build(),
            )
            .unwrap();

            tx.update_node(
                neighbor_b,
                PropertyMapBuilder::new()
                    .insert_vector("vec", &[-10.0, 0.0]) // Moved 9 units
                    .build(),
            )
            .unwrap();

            Ok::<(), crate::core::error::Error>(())
        })
        .unwrap();

        let time_end = time::now();
        let window = TimeRange::new(time_start, time_end).unwrap();

        let detector = FossilDetector::new(&db);
        let result = detector.detect_fossil(fossil_node, window, "vec").unwrap();

        // The neighbors moved ~9 units each. Context displacement should be ~9.
        // Fossil moved 0 units. Node displacement should be 0.
        // Fossil index should be 9 / (0 + eps) = very large.

        assert!(
            result.node_displacement < 0.1,
            "Node displacement should be ~0"
        );
        assert!(
            result.context_displacement > 8.0,
            "Context displacement should be ~9"
        );
        assert!(result.fossil_index > 1.0, "Fossil index should be high");
    }
}
