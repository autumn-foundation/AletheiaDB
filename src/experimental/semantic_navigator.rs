//! Semantic Navigator
//!
//! This experimental module implements A* pathfinding on the graph using vector similarity
//! as the heuristic and cost function. It enables finding "semantically smooth" paths
//! between concepts, prioritizing edges that maintain semantic coherence.
//!
//! # How it works
//!
//! The navigator treats the graph as a state space where:
//! - **Nodes** are states.
//! - **Edges** are transitions.
//! - **Cost** of a transition (A -> B) is `1.0 - cosine_similarity(A, B)`.
//! - **Heuristic** (h-score) for a node N to goal G is `1.0 - cosine_similarity(N, G)`.
//!
//! This approach favors paths where adjacent nodes are semantically similar, and where
//! the path moves progressively closer to the goal's semantic meaning.
//!
//! # Example
//!
//! ```rust
//! # use aletheiadb::AletheiaDB;
//! # use aletheiadb::core::property::PropertyMapBuilder;
//! # use aletheiadb::experimental::semantic_navigator::SemanticNavigator;
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! // 1. Setup database
//! let db = AletheiaDB::new()?;
//!
//! // 2. Create nodes with embeddings
//! // A: "King" -> B: "Queen" -> C: "Royalty"
//! // ( Simplified vectors for illustration )
//! let vec_a = vec![1.0, 0.0];
//! let vec_b = vec![0.9, 0.1]; // Similar to A
//! let vec_c = vec![0.8, 0.2]; // Similar to B
//!
//! let a = db.create_node("Word", PropertyMapBuilder::new().insert_vector("vec", &vec_a).build())?;
//! let b = db.create_node("Word", PropertyMapBuilder::new().insert_vector("vec", &vec_b).build())?;
//! let c = db.create_node("Word", PropertyMapBuilder::new().insert_vector("vec", &vec_c).build())?;
//!
//! db.create_edge(a, b, "RELATED", Default::default())?;
//! db.create_edge(b, c, "RELATED", Default::default())?;
//!
//! // 3. Find semantic path
//! let navigator = SemanticNavigator::new(&db);
//! let path = navigator.find_path(a, c, "vec")?;
//!
//! assert_eq!(path, vec![a, b, c]);
//! # Ok(())
//! # }
//! ```

use crate::AletheiaDB;
use crate::core::error::{Error, Result};
use crate::core::hasher::IdentityHasher;
use crate::core::id::NodeId;
use crate::core::vector::cosine_similarity;
use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashMap};
use std::hash::BuildHasherDefault;

/// A navigator that finds semantically meaningful paths through the graph.
///
/// Wraps a reference to `AletheiaDB` to perform graph traversals and property lookups.
pub struct SemanticNavigator<'a> {
    db: &'a AletheiaDB,
}

/// Search state for A* algorithm.
#[derive(Clone, Copy, PartialEq)]
struct State {
    /// Total estimated cost (f-score = g-score + h-score).
    cost: f32,
    /// Current node ID.
    node: NodeId,
}

impl Eq for State {}

// Priority queue depends on `Ord`.
// We flip the ordering on costs because BinaryHeap is a max-heap,
// but A* requires a min-heap (lowest cost first).
impl Ord for State {
    fn cmp(&self, other: &Self) -> Ordering {
        // Compare costs inversely
        other
            .cost
            .partial_cmp(&self.cost)
            .unwrap_or(Ordering::Equal)
    }
}

impl PartialOrd for State {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl<'a> SemanticNavigator<'a> {
    /// Create a new SemanticNavigator.
    pub fn new(db: &'a AletheiaDB) -> Self {
        Self { db }
    }

    /// Find a path from `start` to `end` using vector similarity on `vector_prop`.
    ///
    /// # Algorithm
    ///
    /// Implements A* search where:
    /// - **g_score(n)**: Cumulative cost from start to n. Cost of edge (u, v) is `1.0 - similarity(u, v)`.
    /// - **h_score(n)**: Estimated cost from n to end. Heuristic is `1.0 - similarity(n, end)`.
    /// - **f_score(n)**: `g_score(n) + h_score(n)`.
    ///
    /// # Constraints
    ///
    /// - Both `start` and `end` nodes must have the specified `vector_prop`.
    /// - Nodes missing the `vector_prop` encountered during traversal are penalized with high cost (1.0).
    ///
    /// # Arguments
    ///
    /// * `start` - Starting node ID.
    /// * `end` - Target node ID.
    /// * `vector_prop` - Name of the property containing the vector embedding.
    ///
    /// # Returns
    ///
    /// - `Ok(Vec<NodeId>)`: The path from start to end (inclusive).
    /// - `Err`: If start/end nodes are invalid, missing vectors, or no path exists.
    pub fn find_path(&self, start: NodeId, end: NodeId, vector_prop: &str) -> Result<Vec<NodeId>> {
        // 1. Validate Start/End and get Goal Vector
        let start_node = self.db.get_node(start)?;
        let end_node = self.db.get_node(end)?;

        // Ensure start and end have vectors (we need goal vector for heuristic)
        let _start_vec = start_node
            .properties
            .get(vector_prop)
            .and_then(|v| v.as_arc_vector())
            .ok_or_else(|| {
                Error::other(format!(
                    "Start node {} missing vector property '{}'",
                    start, vector_prop
                ))
            })?;

        let end_vec = end_node
            .properties
            .get(vector_prop)
            .and_then(|v| v.as_arc_vector())
            .ok_or_else(|| {
                Error::other(format!(
                    "End node {} missing vector property '{}'",
                    end, vector_prop
                ))
            })?;

        // 2. Initialize A*
        let mut open_set = BinaryHeap::new();
        open_set.push(State {
            cost: 0.0,
            node: start,
        });

        // ⚡ Bolt: Use `IdentityHasher` instead of the default SipHash.
        // `NodeId` is already a unique wrapper over `u64`. In performance-critical pathfinding (like A*),
        // bypassing SipHash eliminates unnecessary hashing overhead on already-unique integer keys.
        let mut came_from: HashMap<NodeId, NodeId, BuildHasherDefault<IdentityHasher>> =
            HashMap::default();
        let mut g_score: HashMap<NodeId, f32, BuildHasherDefault<IdentityHasher>> =
            HashMap::default();
        g_score.insert(start, 0.0);

        let mut f_score: HashMap<NodeId, f32, BuildHasherDefault<IdentityHasher>> =
            HashMap::default();
        // h(start) = 1.0 - sim(start, end)
        // We know start has a vector.
        let h_start = 1.0 - cosine_similarity(&_start_vec, &end_vec)?;
        f_score.insert(start, h_start);

        // Track visited nodes to prevent cycles and redundant processing
        // While A* with consistent heuristic doesn't strictly need a closed set for correctness,
        // it helps performance on general graphs.
        // However, g_score check implicitly handles this.

        while let Some(State {
            cost: _current_f,
            node: current,
        }) = open_set.pop()
        {
            if current == end {
                return Ok(self.reconstruct_path(came_from, current));
            }

            // Get current vector for cost calculation
            // Optimization Note: We fetch it repeatedly. In a production version,
            // we might want to cache these or batch-fetch.
            let current_node = if current == start {
                start_node.clone() // We have it already (cheap clone of Arc properties)
            } else {
                self.db.get_node(current)?
            };

            let current_vec = current_node
                .properties
                .get(vector_prop)
                .and_then(|v| v.as_arc_vector());

            for edge_id in self.db.get_outgoing_edges(current) {
                let neighbor = self.db.get_edge_target(edge_id)?;

                // Calculate tentative_g_score
                let neighbor_node = self.db.get_node(neighbor)?;
                let neighbor_vec = neighbor_node
                    .properties
                    .get(vector_prop)
                    .and_then(|v| v.as_arc_vector());

                // Cost(current, neighbor)
                let distance_cost = match (&current_vec, &neighbor_vec) {
                    (Some(a), Some(b)) => 1.0 - cosine_similarity(a, b)?,
                    _ => 1.0, // Penalize missing vectors (max distance)
                };

                let tentative_g = g_score.get(&current).unwrap_or(&f32::INFINITY) + distance_cost;

                if tentative_g < *g_score.get(&neighbor).unwrap_or(&f32::INFINITY) {
                    came_from.insert(neighbor, current);
                    g_score.insert(neighbor, tentative_g);

                    // h(neighbor) = 1.0 - sim(neighbor, goal)
                    let h_score = match &neighbor_vec {
                        Some(vec) => 1.0 - cosine_similarity(vec, &end_vec)?,
                        None => 1.0, // High heuristic if missing vector
                    };

                    let f = tentative_g + h_score;
                    f_score.insert(neighbor, f);
                    open_set.push(State {
                        cost: f,
                        node: neighbor,
                    });
                }
            }
        }

        Err(Error::other("No path found"))
    }

    fn reconstruct_path(
        &self,
        came_from: HashMap<NodeId, NodeId, BuildHasherDefault<IdentityHasher>>,
        mut current: NodeId,
    ) -> Vec<NodeId> {
        let mut total_path = vec![current];
        while let Some(&prev) = came_from.get(&current) {
            current = prev;
            total_path.push(current);
        }
        total_path.reverse();
        total_path
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AletheiaDBConfig;
    use crate::config::WalConfigBuilder;
    use crate::core::property::PropertyMapBuilder;
    use tempfile::tempdir;

    fn create_test_db() -> (AletheiaDB, tempfile::TempDir) {
        let dir = tempdir().unwrap();
        let config = AletheiaDBConfig::builder()
            .wal(
                WalConfigBuilder::new()
                    .wal_dir(dir.path().join("wal"))
                    .build(),
            )
            .build();
        let db = AletheiaDB::with_unified_config(config).unwrap();
        (db, dir)
    }

    #[test]
    fn test_semantic_path_linear() {
        let (db, _dir) = create_test_db();

        // A -> B -> C
        // Vectors:
        // A: [1.0, 0.0]
        // B: [0.707, 0.707] (45 degrees, "Halfway" semantically)
        // C: [0.0, 1.0] (90 degrees)

        // Graph Topology:
        // A -> B -> C
        // A -> D -> C (Alternative path)
        // D: [0.0, -1.0] (Opposite direction, semantically distant from C)

        // Semantic Navigator should prefer A -> B -> C because B is semantically closer to C than D is.

        let props_a = PropertyMapBuilder::new()
            .insert_vector("vec", &[1.0, 0.0])
            .build();
        let a = db.create_node("Node", props_a).unwrap();

        let props_b = PropertyMapBuilder::new()
            .insert_vector("vec", &[0.707, 0.707])
            .build();
        let b = db.create_node("Node", props_b).unwrap();

        let props_c = PropertyMapBuilder::new()
            .insert_vector("vec", &[0.0, 1.0])
            .build();
        let c = db.create_node("Node", props_c).unwrap();

        let props_d = PropertyMapBuilder::new()
            .insert_vector("vec", &[0.0, -1.0])
            .build();
        let d = db.create_node("Node", props_d).unwrap();

        db.create_edge(a, b, "NEXT", PropertyMapBuilder::new().build())
            .unwrap();
        db.create_edge(b, c, "NEXT", PropertyMapBuilder::new().build())
            .unwrap();

        db.create_edge(a, d, "NEXT", PropertyMapBuilder::new().build())
            .unwrap();
        db.create_edge(d, c, "NEXT", PropertyMapBuilder::new().build())
            .unwrap();

        let nav = SemanticNavigator::new(&db);
        let path = nav.find_path(a, c, "vec").unwrap();

        assert_eq!(
            path,
            vec![a, b, c],
            "Should choose the semantically closer path via B"
        );
    }

    #[test]
    fn test_missing_vector_fail() {
        let (db, _dir) = create_test_db();
        let a = db
            .create_node("Node", PropertyMapBuilder::new().build())
            .unwrap();
        let b = db
            .create_node("Node", PropertyMapBuilder::new().build())
            .unwrap();

        let nav = SemanticNavigator::new(&db);
        let result = nav.find_path(a, b, "vec");
        assert!(
            result.is_err(),
            "Should fail if start/end nodes lack vectors"
        );
    }
}
