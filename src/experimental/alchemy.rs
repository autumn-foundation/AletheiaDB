//! Alchemy: Semantic Graph Transformation Engine.
//!
//! "Turn lead into gold."
//!
//! Alchemy allows you to define "Semantic Reactions" that automatically evolve your
//! knowledge graph. It unifies Analysis (finding patterns) with Evolution (modifying the graph).
//!
//! # Features
//! - **Crystallize Wormholes**: Turns potential "wormholes" (semantic gaps) into real edges.
//! - **Fuse Synonyms**: Merges nodes that are semantically identical.
//!
//! # Example
//! ```rust,no_run
//! use aletheiadb::AletheiaDB;
//! use aletheiadb::experimental::alchemy::Alchemist;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let db = AletheiaDB::new()?;
//! let alchemist = Alchemist::new(&db);
//!
//! // Turn semantic gaps into "RELATED" edges
//! let count = alchemist.crystallize_wormholes(
//!     &vec![], // candidates (empty = none)
//!     0.9,     // similarity threshold
//!     2,       // max hops (must be disconnected or > 2 hops)
//!     "RELATED",
//!     10       // k (number of semantic neighbors to check)
//! )?;
//! println!("Created {} new edges.", count);
//! # Ok(())
//! # }
//! ```

use crate::AletheiaDB;
use crate::api::transaction::{ReadOps, WriteOps};
use crate::core::error::Result;
use crate::core::id::NodeId;
use crate::core::interning::GLOBAL_INTERNER;
use crate::core::property::PropertyMapBuilder;
use std::collections::{HashSet, VecDeque};

/// The Alchemist engine for graph transformation.
pub struct Alchemist<'a> {
    db: &'a AletheiaDB,
}

impl<'a> Alchemist<'a> {
    /// Create a new Alchemist instance.
    pub fn new(db: &'a AletheiaDB) -> Self {
        Self { db }
    }

    /// Turn semantic gaps (wormholes) into real edges.
    ///
    /// This method identifies pairs of nodes that are semantically similar but structurally
    /// distant (or disconnected), and creates an edge between them.
    ///
    /// # Arguments
    /// * `candidates` - List of source nodes to check.
    /// * `similarity_threshold` - Minimum cosine similarity (0.0 to 1.0) to justify an edge.
    /// * `max_hops` - Max structural distance to consider "disconnected" (e.g. 2).
    /// * `edge_label` - The label for the new edges (e.g. "RELATED").
    /// * `k` - Number of nearest semantic neighbors to consider.
    ///
    /// # Returns
    /// The number of edges created.
    pub fn crystallize_wormholes(
        &self,
        candidates: &[NodeId],
        similarity_threshold: f32,
        max_hops: usize,
        edge_label: &str,
        k: usize,
    ) -> Result<usize> {
        if candidates.is_empty() {
            return Ok(0);
        }

        if candidates.len() > 1000 {
            return Err(crate::core::error::Error::other(
                "Too many candidates for wormhole crystallization (max 1000)",
            ));
        }

        // 1. Find wormholes (Read Phase)
        // A wormhole is a pair (source, target) that is semantically similar but structurally distant.
        let mut wormholes = Vec::new();

        for &source in candidates {
            let neighbors = self.db.find_similar(source, k)?;
            for (target, similarity) in neighbors {
                if similarity >= similarity_threshold {
                    // Check structural distance
                    // If distance is None, it means no path within max_hops was found.
                    if self.bfs_distance(source, target, max_hops)?.is_none() {
                        wormholes.push((source, target, similarity));
                    }
                }
            }
        }

        if wormholes.is_empty() {
            return Ok(0);
        }

        // 2. Write Phase
        let created_count = self.db.write(|tx| {
            let mut count = 0;
            for (source, target, similarity) in wormholes {
                // Create edge with metadata
                let props = PropertyMapBuilder::new()
                    .insert("similarity", similarity as f64)
                    .insert("crystallized", true)
                    .build();

                tx.create_edge(source, target, edge_label, props)?;
                count += 1;
            }
            Ok::<_, crate::core::error::Error>(count)
        })?;

        Ok(created_count)
    }

    /// Calculate shortest path distance using BFS up to max_depth.
    ///
    /// Returns:
    /// - `Ok(Some(d))` if path found with length `d <= max_depth`.
    /// - `Ok(None)` if no path found within `max_depth`.
    fn bfs_distance(&self, start: NodeId, end: NodeId, max_depth: usize) -> Result<Option<usize>> {
        if max_depth > 10 {
            return Err(crate::core::error::Error::other(
                "max_depth cannot exceed 10",
            ));
        }

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

    /// Merges highly similar nodes into a single node ("Semantic Fusion").
    ///
    /// This method:
    /// 1. Finds pairs of nodes in `candidates` that exceed `similarity_threshold`.
    /// 2. Designates the node with the lower ID as the "Survivor" and the other as "Victim".
    /// 3. Moves all edges from Victim to Survivor.
    /// 4. Deletes the Victim node.
    ///
    /// # Arguments
    /// * `candidates` - List of nodes to check.
    /// * `similarity_threshold` - Minimum similarity to consider nodes as synonyms.
    ///
    /// # Returns
    /// The number of nodes merged (deleted).
    pub fn fuse_synonyms(&self, candidates: &[NodeId], similarity_threshold: f32) -> Result<usize> {
        if candidates.len() < 2 {
            return Ok(0);
        }

        // Optimization: Create a set for fast lookup of valid merge targets
        let candidate_set: HashSet<NodeId> = candidates.iter().cloned().collect();

        // 1. Identify pairs to merge
        // We use a simple greedy scan: for each candidate, find similar nodes.
        // We track processed nodes to avoid duplicates or chains in one pass.
        let mut pairs = Vec::new();
        let mut processed = HashSet::new();

        // Limit search to K=5 neighbors to avoid excessive scanning
        for &node_id in candidates {
            if processed.contains(&node_id) {
                continue;
            }

            // Find similar nodes (excludes self)
            // Note: find_similar might return nodes outside candidates list
            let neighbors = self.db.find_similar(node_id, 5)?;

            for (neighbor, sim) in neighbors {
                if sim >= similarity_threshold
                    && candidate_set.contains(&neighbor)
                    && !processed.contains(&neighbor)
                {
                    // Found a valid pair within candidates
                    pairs.push((node_id, neighbor));
                    processed.insert(node_id);
                    processed.insert(neighbor);
                    // Greedy match: take the first good match and move on
                    break;
                }
            }
        }

        if pairs.is_empty() {
            return Ok(0);
        }

        // 2. Perform merge in transaction
        let merged_count = self.db.write(|tx| {
            let mut count = 0;
            for (n1, n2) in pairs {
                // Determine Survivor (lower ID implies created earlier, usually "canonical")
                // This is a heuristic; real app might use PageRank or degree centrality.
                let (survivor, victim) = if n1 < n2 { (n1, n2) } else { (n2, n1) };

                // 2a. Move Outgoing Edges
                // Collect edge data first to avoid borrow issues while mutating
                // Actually ReadOps allows reading while WriteTx is active
                let outgoing = tx.get_outgoing_edges(victim);
                for edge_id in outgoing {
                    // Safe to unwrap here because get_outgoing_edges returns existing edges
                    if let Ok(edge) = tx.get_edge(edge_id) {
                        // Resolve label to string for creation
                        let label_str = GLOBAL_INTERNER
                            .resolve_with(edge.label, |s| s.to_string())
                            .unwrap_or_else(|| "UNKNOWN".to_string());

                        // Create new edge from Survivor -> Target
                        // Avoid self-loops if target happens to be the survivor (merging adjacent nodes)
                        if edge.target != survivor {
                            tx.create_edge(
                                survivor,
                                edge.target,
                                &label_str,
                                edge.properties.clone(),
                            )?;
                        }
                    }
                }

                // 2b. Move Incoming Edges
                let incoming = tx.get_incoming_edges(victim);
                for edge_id in incoming {
                    if let Ok(edge) = tx.get_edge(edge_id) {
                        let label_str = GLOBAL_INTERNER
                            .resolve_with(edge.label, |s| s.to_string())
                            .unwrap_or_else(|| "UNKNOWN".to_string());

                        // Create new edge from Source -> Survivor
                        if edge.source != survivor {
                            tx.create_edge(
                                edge.source,
                                survivor,
                                &label_str,
                                edge.properties.clone(),
                            )?;
                        }
                    }
                }

                // 2c. Delete Victim
                // delete_node_cascade removes the victim and cleans up its old edges
                tx.delete_node_cascade(victim)?;
                count += 1;
            }
            Ok::<_, crate::core::error::Error>(count)
        })?;

        Ok(merged_count)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::property::PropertyMapBuilder;
    use crate::index::vector::{DistanceMetric, HnswConfig};

    fn create_test_db() -> AletheiaDB {
        let db = AletheiaDB::new().unwrap();
        // Enable vector index
        let config = HnswConfig::new(2, DistanceMetric::Cosine);
        db.enable_vector_index("vec", config).unwrap();
        db
    }

    #[test]
    fn test_crystallize_wormholes_creates_edges() {
        let db = create_test_db();
        let alchemist = Alchemist::new(&db);

        // 1. Create similar nodes A and B (disconnected)
        let props_a = PropertyMapBuilder::new()
            .insert_vector("vec", &[1.0, 0.0])
            .build();
        let a = db.create_node("Node", props_a).unwrap();

        let props_b = PropertyMapBuilder::new()
            .insert_vector("vec", &[0.99, 0.1]) // High similarity
            .build();
        let b = db.create_node("Node", props_b).unwrap();

        // 2. Run Crystallize
        let candidates = vec![a, b];
        let count = alchemist
            .crystallize_wormholes(&candidates, 0.9, 2, "RELATED", 10)
            .unwrap();

        assert!(
            count >= 1,
            "Should create at least one edge (may be bidirectional)"
        );

        // 3. Verify Edge Exists
        let found = db
            .read(|tx| {
                let outgoing = tx.get_outgoing_edges(a);
                let mut found_edge = false;
                for eid in outgoing {
                    if let Ok(edge) = tx.get_edge(eid) {
                        if edge.target == b && edge.has_label_str("RELATED") {
                            found_edge = true;
                            // Verify metadata
                            if let Some(sim) = edge.properties.get("similarity") {
                                assert!(sim.as_float().unwrap() > 0.9);
                            }
                        }
                    }
                }
                Ok::<_, crate::core::error::Error>(found_edge)
            })
            .unwrap();

        assert!(found, "Edge A->B with label RELATED should exist");
    }

    #[test]
    fn test_fuse_synonyms_merges_nodes() {
        let db = create_test_db();
        let alchemist = Alchemist::new(&db);

        // 1. Create Synonym Nodes (Survivor A, Victim B)
        // IDs are sequential, so first created has lower ID (usually)
        let props_a = PropertyMapBuilder::new()
            .insert("name", "Survivor")
            .insert_vector("vec", &[1.0, 0.0])
            .build();
        let a = db.create_node("Node", props_a).unwrap();

        let props_b = PropertyMapBuilder::new()
            .insert("name", "Victim")
            .insert_vector("vec", &[0.99, 0.01]) // Very similar
            .build();
        let b = db.create_node("Node", props_b).unwrap();

        // 2. Create Connections
        let c = db
            .create_node("Other", PropertyMapBuilder::new().build())
            .unwrap();
        let d = db
            .create_node("Other", PropertyMapBuilder::new().build())
            .unwrap();

        // C -> A (should stay C -> A)
        db.create_edge(c, a, "LINKS", Default::default()).unwrap();

        // C -> B (should become C -> A)
        db.create_edge(c, b, "LINKS_TO_B", Default::default())
            .unwrap();

        // B -> D (should become A -> D)
        db.create_edge(b, d, "LINKS_FROM_B", Default::default())
            .unwrap();

        // 3. Run Fuse
        let candidates = vec![a, b];
        let merged = alchemist.fuse_synonyms(&candidates, 0.95).unwrap();

        assert_eq!(merged, 1, "Should merge 1 pair (delete 1 node)");

        // 4. Verify B is deleted
        assert!(db.get_node(b).is_err(), "Node B should be deleted");

        // 5. Verify A has inherited edges
        // Check Outgoing: A -> D should exist (inherited from B -> D)
        let has_a_to_d = db
            .read(|tx| {
                let edges = tx.get_outgoing_edges(a);
                let mut found = false;
                for eid in edges {
                    if let Ok(edge) = tx.get_edge(eid) {
                        if edge.target == d && edge.has_label_str("LINKS_FROM_B") {
                            found = true;
                        }
                    }
                }
                Ok::<_, crate::core::error::Error>(found)
            })
            .unwrap();
        assert!(has_a_to_d, "Survivor A should inherit outgoing edge to D");

        // Check Incoming: C -> A should exist twice (original + inherited)
        // One with label LINKS, one with label LINKS_TO_B
        let has_c_to_a_inherited = db
            .read(|tx| {
                let edges = tx.get_outgoing_edges(c);
                let mut found = false;
                for eid in edges {
                    if let Ok(edge) = tx.get_edge(eid) {
                        if edge.target == a && edge.has_label_str("LINKS_TO_B") {
                            found = true;
                        }
                    }
                }
                Ok::<_, crate::core::error::Error>(found)
            })
            .unwrap();
        assert!(
            has_c_to_a_inherited,
            "Survivor A should inherit incoming edge from C"
        );
    }

    #[test]
    fn test_bfs_distance_logic() {
        let db = create_test_db();
        let alchemist = Alchemist::new(&db);

        // 1. Create Nodes
        let props = PropertyMapBuilder::new().build();
        let a = db.create_node("Node", props.clone()).unwrap();
        let b = db.create_node("Node", props.clone()).unwrap();
        let c = db.create_node("Node", props.clone()).unwrap();

        // 2. Create Edges A -> B -> C
        db.create_edge(a, b, "NEXT", props.clone()).unwrap();
        db.create_edge(b, c, "NEXT", props.clone()).unwrap();

        // A -> C distance is 2.

        // Case 1: Max Hops = 1. Should return None.
        let dist1 = alchemist.bfs_distance(a, c, 1).unwrap();
        assert!(dist1.is_none(), "Should not find path within 1 hop");

        // Case 2: Max Hops = 2. Should return Some(2).
        let dist2 = alchemist.bfs_distance(a, c, 2).unwrap();
        assert_eq!(dist2, Some(2), "Should find path of length 2");

        // Case 3: Max Hops = 3. Should return Some(2).
        let dist3 = alchemist.bfs_distance(a, c, 3).unwrap();
        assert_eq!(dist3, Some(2), "Should find path of length 2");
    }

    #[test]
    fn test_bfs_distance_disconnected() {
        let db = create_test_db();
        let alchemist = Alchemist::new(&db);

        let props = PropertyMapBuilder::new().build();
        let a = db.create_node("Node", props.clone()).unwrap();
        let d = db.create_node("Node", props.clone()).unwrap();

        let dist = alchemist.bfs_distance(a, d, 5).unwrap();
        assert!(dist.is_none(), "Should be disconnected");
    }
}
