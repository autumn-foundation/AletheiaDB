//! Wormhole: Detecting Semantic-Structural Gaps.
//!
//! "Mind the Gap."
//!
//! The Wormhole Detector identifies pairs of nodes that are **semantically similar** (close in vector space)
//! but **structurally distant** (or disconnected) in the graph.
//! These represent "latent edges" or "missing links" in the knowledge graph.
//!
//! # Use Cases
//! - **Link Prediction**: Suggesting new edges based on semantic similarity.
//! - **Anomaly Detection**: Finding nodes that "should be connected" but aren't.
//! - **Serendipity**: Discovering unexpected relationships.
//!
//! # Example
//! ```rust,no_run
//! use aletheiadb::AletheiaDB;
//! use aletheiadb::experimental::wormhole::WormholeDetector;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let db = AletheiaDB::new()?;
//! let detector = WormholeDetector::new(&db);
//!
//! // Find candidates for wormholes (e.g., all Person nodes)
//! let candidates = vec![node_id_1, node_id_2]; // Populate with actual IDs
//!
//! // Find wormholes: Top 10 similar nodes, max 2 hops structural distance
//! let wormholes = detector.find_wormholes(&candidates, 10, 2)?;
//!
//! for w in wormholes {
//!     println!("Found wormhole from {} to {} (similarity: {}, distance: {:?})",
//!         w.source, w.target, w.similarity, w.structural_distance);
//! }
//! # Ok(())
//! # }
//! ```

use crate::AletheiaDB;
use crate::core::id::NodeId;
use crate::utils::Result;
use std::collections::{HashSet, VecDeque};

/// A detected wormhole (semantic shortcut).
#[derive(Debug, Clone, PartialEq)]
pub struct Wormhole {
    /// The source node of the wormhole.
    pub source: NodeId,
    /// The target node (semantically close to source).
    pub target: NodeId,
    /// The semantic similarity score (e.g., cosine similarity).
    pub similarity: f32,
    /// The shortest path distance in the graph.
    /// `None` if no path exists within `max_hops`.
    pub structural_distance: Option<usize>,
}

/// Detector for finding semantic wormholes.
pub struct WormholeDetector<'a> {
    db: &'a AletheiaDB,
}

impl<'a> WormholeDetector<'a> {
    /// Create a new WormholeDetector.
    pub fn new(db: &'a AletheiaDB) -> Self {
        Self { db }
    }

    /// Find wormholes for a set of candidate nodes.
    ///
    /// For each candidate:
    /// 1. Finds `k` most similar nodes in vector space.
    /// 2. Checks structural distance to each similar node using BFS up to `max_hops`.
    /// 3. Returns pairs where distance is `None` (disconnected or > max_hops).
    ///
    /// # Arguments
    ///
    /// * `candidates` - List of source nodes to check.
    /// * `k` - Number of nearest semantic neighbors to consider.
    /// * `max_hops` - Maximum graph distance to search before considering it a wormhole.
    pub fn find_wormholes(
        &self,
        candidates: &[NodeId],
        k: usize,
        max_hops: usize,
    ) -> Result<Vec<Wormhole>> {
        let mut wormholes = Vec::new();

        for &source in candidates {
            // Find semantic neighbors using the default vector index.
            // Note: find_similar excludes the query node itself.
            let semantic_neighbors = self.db.find_similar(source, k)?;

            for (target, similarity) in semantic_neighbors {
                // Check structural distance
                let distance = self.bfs_distance(source, target, max_hops)?;

                // If distance is None, it means no path within max_hops was found.
                // This is a candidate for a wormhole (strong semantic link, weak/no structural link).
                if distance.is_none() {
                    wormholes.push(Wormhole {
                        source,
                        target,
                        similarity,
                        structural_distance: None,
                    });
                }
            }
        }

        Ok(wormholes)
    }

    /// Calculate shortest path distance using BFS up to max_depth.
    ///
    /// Returns:
    /// - `Ok(Some(d))` if path found with length `d <= max_depth`.
    /// - `Ok(None)` if no path found within `max_depth`.
    fn bfs_distance(&self, start: NodeId, end: NodeId, max_depth: usize) -> Result<Option<usize>> {
        if start == end {
            return Ok(Some(0));
        }

        let mut queue = VecDeque::new();
        queue.push_back((start, 0));

        let mut visited = HashSet::new();
        visited.insert(start);

        while let Some((current, depth)) = queue.pop_front() {
            if depth >= max_depth {
                // If we reached max depth and haven't found target,
                // we stop this branch. If queue empties, we return None.
                continue;
            }

            // Iterate over outgoing edges
            let edges_iter = self.db.current.get_outgoing_edges_iter(current);
            for edge_id in edges_iter {
                // Resolve target node (zero-copy)
                let target = self.db.current.get_edge_target(edge_id)?;

                if target == end {
                    return Ok(Some(depth + 1));
                }

                if visited.insert(target) {
                    queue.push_back((target, depth + 1));
                }
            }
        }

        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::property::PropertyMapBuilder;
    use crate::index::vector::{DistanceMetric, HnswConfig};

    #[test]
    fn test_wormhole_detection() {
        let db = AletheiaDB::new().unwrap();

        // 1. Setup Vector Index
        let config = HnswConfig::new(2, DistanceMetric::Cosine);
        db.enable_vector_index("vec", config).unwrap();

        // 2. Create Nodes
        // A: [1.0, 0.0]
        // B: [0.0, 1.0] (Orthogonal to A)
        // C: [0.9, 0.1] (Close to A)

        let props_a = PropertyMapBuilder::new()
            .insert_vector("vec", &[1.0, 0.0])
            .build();
        let a = db.create_node("Node", props_a).unwrap();

        let props_b = PropertyMapBuilder::new()
            .insert_vector("vec", &[0.0, 1.0])
            .build();
        let b = db.create_node("Node", props_b).unwrap();

        let props_c = PropertyMapBuilder::new()
            .insert_vector("vec", &[0.9, 0.1])
            .build();
        let c = db.create_node("Node", props_c).unwrap();

        // 3. Create Edges
        // A -> B -> C
        // Path A -> C exists (dist 2), but no direct edge A -> C.
        db.create_edge(a, b, "NEXT", PropertyMapBuilder::new().build())
            .unwrap();
        db.create_edge(b, c, "NEXT", PropertyMapBuilder::new().build())
            .unwrap();

        let detector = WormholeDetector::new(&db);

        // Case 1: Max Hops = 1
        // BFS checks distance <= 1.
        // A -> C distance is 2.
        // So BFS returns None.
        // A and C are semantically close.
        // Expect Wormhole A -> C.
        let wormholes = detector.find_wormholes(&[a], 10, 1).unwrap();

        // Should find A -> C
        let ac = wormholes.iter().find(|w| w.target == c);
        assert!(ac.is_some(), "Should find wormhole A -> C with max_hops=1");
        let w = ac.unwrap();
        assert_eq!(w.source, a);
        assert_eq!(w.target, c);
        assert!(w.structural_distance.is_none());
        assert!(w.similarity > 0.8); // High similarity

        // Case 2: Max Hops = 2
        // BFS checks distance <= 2.
        // A -> C distance is 2.
        // So BFS returns Some(2).
        // Since distance found, it is NOT a wormhole (structurally connected enough).
        let wormholes_2 = detector.find_wormholes(&[a], 10, 2).unwrap();
        let ac_2 = wormholes_2.iter().find(|w| w.target == c);
        assert!(
            ac_2.is_none(),
            "Should NOT find wormhole A -> C with max_hops=2 (path exists)"
        );
    }

    #[test]
    fn test_wormhole_disconnected() {
        let db = AletheiaDB::new().unwrap();
        let config = HnswConfig::new(2, DistanceMetric::Cosine);
        db.enable_vector_index("vec", config).unwrap();

        // A and D are disconnected but similar
        let props_a = PropertyMapBuilder::new()
            .insert_vector("vec", &[1.0, 0.0])
            .build();
        let a = db.create_node("Node", props_a).unwrap();

        let props_d = PropertyMapBuilder::new()
            .insert_vector("vec", &[0.95, 0.0])
            .build();
        let d = db.create_node("Node", props_d).unwrap();

        let detector = WormholeDetector::new(&db);
        let wormholes = detector.find_wormholes(&[a], 10, 5).unwrap();

        assert!(!wormholes.is_empty());
        assert_eq!(wormholes[0].target, d);
        assert!(wormholes[0].structural_distance.is_none());
    }
}
