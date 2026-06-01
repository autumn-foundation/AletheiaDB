//! Doppelganger: The Disconnected Twin Detector.
//!
//! "Are there nodes that mean exactly the same thing but have never met?"
//!
//! Doppelganger identifies pairs of nodes that have exceptionally high semantic
//! similarity (their vectors are almost identical) but are completely disconnected
//! in the graph structure (no path between them, or path length > threshold).
//!
//! # Example
//! ```rust,no_run
//! use aletheiadb::AletheiaDB;
//! use aletheiadb::experimental::doppelganger::Doppelganger;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let db = AletheiaDB::new()?;
//! let mut detector = Doppelganger::new(&db);
//!
//! // Find pairs with > 0.95 similarity that have no path between them
//! let twins = detector.find_doppelgangers("embedding", 0.95)?;
//! for twin in twins {
//!     println!("Found doppelgangers: {} and {}", twin.node_a, twin.node_b);
//! }
//! # Ok(())
//! # }
//! ```

use crate::AletheiaDB;
use crate::core::error::Result;
use crate::core::id::NodeId;
use crate::core::vector::ops::cosine_similarity;
use std::collections::{HashSet, VecDeque};

/// A detected pair of disconnected twins.
#[derive(Debug, Clone, PartialEq)]
pub struct DoppelgangerPair {
    /// The first node in the pair.
    pub node_a: NodeId,
    /// The second node in the pair.
    pub node_b: NodeId,
    /// The actual cosine similarity between them.
    pub similarity: f32,
}

/// The Doppelganger engine for detecting disconnected twins.
pub struct Doppelganger<'a> {
    db: &'a AletheiaDB,
}

impl<'a> Doppelganger<'a> {
    /// Create a new Doppelganger instance.
    pub fn new(db: &'a AletheiaDB) -> Self {
        Self { db }
    }

    /// Find pairs of nodes that have high similarity but are disconnected.
    ///
    /// Two nodes are considered disconnected if there is no path between them
    /// of length 3 or less.
    ///
    /// # Arguments
    /// * `vector_property` - The property name containing the node embeddings.
    /// * `similarity_threshold` - Minimum cosine similarity to be considered a twin.
    #[allow(clippy::collapsible_if)]
    pub fn find_doppelgangers(
        &self,
        vector_property: &str,
        similarity_threshold: f32,
    ) -> Result<Vec<DoppelgangerPair>> {
        let mut doppelgangers = Vec::new();

        // 1. Gather all nodes with vectors
        let mut nodes_with_vectors = Vec::new();
        let scan_results = self.db.query().scan(None).execute(self.db)?;
        for row_result in scan_results {
            let row = row_result?;
            if let Some(node) = row.entity.as_node() {
                if let Some(vec_prop) = node
                    .get_property(vector_property)
                    .and_then(|p| p.as_vector())
                {
                    nodes_with_vectors.push((node.id, vec_prop.to_vec()));
                }
            }
        }

        // 2. Compare every pair O(N^2) - suitable for diagnostics/R&D, not OLTP.
        // In a real optimized system, we'd use the vector index's HNSW graph.
        for i in 0..nodes_with_vectors.len() {
            let (node_a, vec_a) = &nodes_with_vectors[i];

            for (node_b, vec_b) in nodes_with_vectors.iter().skip(i + 1) {
                let sim = cosine_similarity(vec_a, vec_b)?;

                if sim >= similarity_threshold {
                    // Check if they are connected
                    if !self.are_connected(*node_a, *node_b, 3) {
                        doppelgangers.push(DoppelgangerPair {
                            node_a: *node_a,
                            node_b: *node_b,
                            similarity: sim,
                        });
                    }
                }
            }
        }

        Ok(doppelgangers)
    }

    /// Performs a simple BFS to see if target is reachable from start within max_depth.
    fn are_connected(&self, start: NodeId, target: NodeId, max_depth: usize) -> bool {
        if start == target {
            return true;
        }

        let mut queue = VecDeque::new();
        queue.push_back((start, 0));
        let mut visited = HashSet::new();
        visited.insert(start);

        while let Some((current_node, depth)) = queue.pop_front() {
            if depth >= max_depth {
                continue;
            }

            // Get all neighbors (undirected traversal for "connection")
            let outgoing = self.db.get_outgoing_edges(current_node);
            for edge_id in outgoing {
                if let Ok(edge) = self.db.get_edge(edge_id) {
                    if edge.target == target {
                        return true;
                    }
                    if visited.insert(edge.target) {
                        queue.push_back((edge.target, depth + 1));
                    }
                }
            }

            let incoming = self.db.get_incoming_edges(current_node);
            for edge_id in incoming {
                if let Ok(edge) = self.db.get_edge(edge_id) {
                    if edge.source == target {
                        return true;
                    }
                    if visited.insert(edge.source) {
                        queue.push_back((edge.source, depth + 1));
                    }
                }
            }
        }

        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::property::PropertyMapBuilder;

    #[test]
    fn test_doppelganger_detection() {
        let db = AletheiaDB::new().unwrap();

        // A and B are identical vectors but disconnected
        let a = db
            .create_node(
                "User",
                PropertyMapBuilder::new()
                    .insert_vector("vec", &[1.0, 0.0, 0.0])
                    .build(),
            )
            .unwrap();

        let b = db
            .create_node(
                "User",
                PropertyMapBuilder::new()
                    .insert_vector("vec", &[1.0, 0.0, 0.0])
                    .build(),
            )
            .unwrap();

        // C is identical but connected to A
        let c = db
            .create_node(
                "User",
                PropertyMapBuilder::new()
                    .insert_vector("vec", &[1.0, 0.0, 0.0])
                    .build(),
            )
            .unwrap();

        db.create_edge(a, c, "KNOWS", Default::default()).unwrap();

        // D is connected to C, meaning it is depth=2 from A.
        let d = db
            .create_node(
                "User",
                PropertyMapBuilder::new()
                    .insert_vector("vec", &[1.0, 0.0, 0.0])
                    .build(),
            )
            .unwrap();

        db.create_edge(c, d, "KNOWS", Default::default()).unwrap();

        // E is different vector
        let _e = db
            .create_node(
                "User",
                PropertyMapBuilder::new()
                    .insert_vector("vec", &[0.0, 1.0, 0.0])
                    .build(),
            )
            .unwrap();

        let detector = Doppelganger::new(&db);
        let doppelgangers = detector.find_doppelgangers("vec", 0.99).unwrap();

        // Expected pairs (order of A and B doesn't strictly matter as long as combination is found)
        // A <-> B (Disconnected)
        // C <-> B (Disconnected)
        // D <-> B (Disconnected)

        // A <-> C (Connected, depth 1 - NOT included)
        // A <-> D (Connected, depth 2 - NOT included)
        // C <-> D (Connected, depth 1 - NOT included)

        assert_eq!(doppelgangers.len(), 3);

        let has_ab = doppelgangers
            .iter()
            .any(|p| (p.node_a == a && p.node_b == b) || (p.node_a == b && p.node_b == a));
        let has_cb = doppelgangers
            .iter()
            .any(|p| (p.node_a == c && p.node_b == b) || (p.node_a == b && p.node_b == c));
        let has_db = doppelgangers
            .iter()
            .any(|p| (p.node_a == d && p.node_b == b) || (p.node_a == b && p.node_b == d));

        assert!(has_ab);
        assert!(has_cb);
        assert!(has_db);
    }
}
