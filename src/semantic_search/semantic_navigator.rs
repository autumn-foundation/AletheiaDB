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
//! # use aletheiadb::semantic_search::semantic_navigator::SemanticNavigator;
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
use crate::core::id::NodeId;
use crate::core::vector::cosine_similarity;
use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashMap};

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

/// Outcome of a bounded semantic path search.
///
/// The bounded search has three terminal states that callers frequently need
/// to distinguish. Two of them are perfectly **normal** outcomes (not server
/// faults): a genuinely disconnected pair of nodes, and a search that hit its
/// DoS-protection expansion budget before finding the goal. Conflating these
/// with hard errors (invalid node id, missing vector property) — as an
/// `Err(Error::other(..))` for both would — misleads a caller into treating a
/// normal "no route exists" answer as an internal bug. This enum makes the
/// three cases explicit so the MCP surface can map them to the right
/// structured error code.
///
/// Hard errors (start/end invalid, missing vector) are still surfaced as
/// `Err` from the search; only the three *reachability* outcomes are modeled
/// here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PathSearchOutcome {
    /// A path from `start` to `end` (inclusive) was found.
    Found(Vec<NodeId>),
    /// The two nodes are not connected: the search exhausted the reachable
    /// component (within budget) without reaching `end`.
    NoPath,
    /// The expansion budget was exhausted before `end` was reached. The graph
    /// may still contain a path; the bounded search simply refused to keep
    /// exploring. Carries the budget that was hit so the caller can report it.
    BudgetExhausted {
        /// The `max_expansions` budget that was reached.
        max_expansions: usize,
    },
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
        // The unbounded variant is a thin wrapper over the bounded search with an
        // effectively-infinite expansion budget, so the two share one code path.
        // With `usize::MAX` the budget can never be exhausted, so the only
        // non-success reachability outcome is a genuinely disconnected pair,
        // which this variant reports (unchanged external behavior) as the
        // historical `Err(Error::other("No path found"))`.
        match self.search(start, end, vector_prop, usize::MAX)? {
            PathSearchOutcome::Found(path) => Ok(path),
            PathSearchOutcome::NoPath | PathSearchOutcome::BudgetExhausted { .. } => {
                Err(Error::other("No path found"))
            }
        }
    }

    /// Bounded variant of [`find_path`](Self::find_path) for untrusted callers
    /// (e.g. the MCP surface).
    ///
    /// The plain A* search is **unbounded**: on a large or adversarial graph it
    /// can expand an arbitrary number of nodes before concluding, a denial-of-
    /// service risk when the endpoints are attacker-controlled. This variant caps
    /// the number of node expansions (pops from the open set) at
    /// `max_expansions`; once the budget is exhausted without reaching `end`, it
    /// returns an error instead of continuing to explore. All other semantics are
    /// identical to [`find_path`](Self::find_path).
    ///
    /// # Arguments
    ///
    /// * `start` - Starting node ID.
    /// * `end` - Target node ID.
    /// * `vector_prop` - Name of the property containing the vector embedding.
    /// * `max_expansions` - Maximum number of node expansions before giving up.
    ///
    /// # Returns
    ///
    /// - `Ok(PathSearchOutcome::Found(path))`: The path from start to end
    ///   (inclusive).
    /// - `Ok(PathSearchOutcome::NoPath)`: The two nodes are not connected —
    ///   a normal outcome, not an error.
    /// - `Ok(PathSearchOutcome::BudgetExhausted { .. })`: The expansion budget
    ///   was exhausted before `end` was reached — a DoS-protection refusal, not
    ///   an error.
    /// - `Err`: Only for hard faults — start/end invalid or missing the vector
    ///   property. Reachability outcomes are reported via [`PathSearchOutcome`],
    ///   so callers (e.g. the MCP surface) can map "no path" and "budget
    ///   exhausted" to distinct, non-`INTERNAL` error codes.
    pub fn find_path_bounded(
        &self,
        start: NodeId,
        end: NodeId,
        vector_prop: &str,
        max_expansions: usize,
    ) -> Result<PathSearchOutcome> {
        self.search(start, end, vector_prop, max_expansions)
    }

    /// Core bounded A* search shared by [`find_path`](Self::find_path) and
    /// [`find_path_bounded`](Self::find_path_bounded).
    ///
    /// Returns a [`PathSearchOutcome`] for the three reachability outcomes;
    /// only hard faults (invalid nodes, missing vectors) are surfaced as `Err`.
    fn search(
        &self,
        start: NodeId,
        end: NodeId,
        vector_prop: &str,
        max_expansions: usize,
    ) -> Result<PathSearchOutcome> {
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

        let mut came_from: HashMap<NodeId, NodeId> = HashMap::new();
        let mut g_score: HashMap<NodeId, f32> = HashMap::new();
        g_score.insert(start, 0.0);

        let mut f_score: HashMap<NodeId, f32> = HashMap::new();
        // h(start) = 1.0 - sim(start, end)
        // We know start has a vector.
        let h_start = 1.0 - cosine_similarity(&_start_vec, &end_vec)?;
        f_score.insert(start, h_start);

        // Track visited nodes to prevent cycles and redundant processing
        // While A* with consistent heuristic doesn't strictly need a closed set for correctness,
        // it helps performance on general graphs.
        // However, g_score check implicitly handles this.

        let mut expansions: usize = 0;
        while let Some(State {
            cost: _current_f,
            node: current,
        }) = open_set.pop()
        {
            if current == end {
                return Ok(PathSearchOutcome::Found(
                    self.reconstruct_path(came_from, current),
                ));
            }

            // Enforce the expansion budget (DoS protection for untrusted
            // callers). We count a node the moment it is popped for expansion.
            expansions = expansions.saturating_add(1);
            if expansions > max_expansions {
                return Ok(PathSearchOutcome::BudgetExhausted { max_expansions });
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

        Ok(PathSearchOutcome::NoPath)
    }

    fn reconstruct_path(
        &self,
        came_from: HashMap<NodeId, NodeId>,
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
        let wal_path = dir.path().join("wal");
        let data_path = dir.path().join("data");
        std::fs::create_dir_all(&wal_path).unwrap();
        std::fs::create_dir_all(&data_path).unwrap();

        let persistence_config = crate::storage::index_persistence::PersistenceConfig {
            data_dir: data_path,
            enabled: false,
            ..Default::default()
        };

        let config = AletheiaDBConfig::builder()
            .wal(WalConfigBuilder::new().wal_dir(wal_path).build())
            .persistence(persistence_config)
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

    #[test]
    fn bounded_no_path_reports_no_path_outcome() {
        let (db, _dir) = create_test_db();
        // Two disconnected nodes, both with vectors, no edge between them.
        let a = db
            .create_node(
                "Node",
                PropertyMapBuilder::new()
                    .insert_vector("vec", &[1.0, 0.0])
                    .build(),
            )
            .unwrap();
        let b = db
            .create_node(
                "Node",
                PropertyMapBuilder::new()
                    .insert_vector("vec", &[0.0, 1.0])
                    .build(),
            )
            .unwrap();

        let nav = SemanticNavigator::new(&db);
        let outcome = nav.find_path_bounded(a, b, "vec", 1000).unwrap();
        assert_eq!(
            outcome,
            PathSearchOutcome::NoPath,
            "disconnected pair must report NoPath, not a hard error"
        );

        // The unbounded convenience wrapper preserves its historical contract:
        // NoPath is surfaced as an Err.
        assert!(nav.find_path(a, b, "vec").is_err());
    }

    #[test]
    fn bounded_budget_exhausted_reports_budget_outcome() {
        let (db, _dir) = create_test_db();
        // Build a long chain A -> B -> C -> ... so a tiny budget is exhausted
        // before the far end is reached.
        let mut ids = Vec::new();
        for i in 0..50u64 {
            let x = (i as f32) / 50.0;
            let id = db
                .create_node(
                    "Node",
                    PropertyMapBuilder::new()
                        .insert_vector("vec", &[1.0 - x, x])
                        .build(),
                )
                .unwrap();
            ids.push(id);
        }
        for w in ids.windows(2) {
            db.create_edge(w[0], w[1], "NEXT", PropertyMapBuilder::new().build())
                .unwrap();
        }

        let nav = SemanticNavigator::new(&db);
        let outcome = nav
            .find_path_bounded(ids[0], ids[ids.len() - 1], "vec", 2)
            .unwrap();
        assert_eq!(
            outcome,
            PathSearchOutcome::BudgetExhausted { max_expansions: 2 },
            "a tiny budget on a long chain must report BudgetExhausted"
        );

        // A generous budget still finds the path.
        let found = nav
            .find_path_bounded(ids[0], ids[ids.len() - 1], "vec", usize::MAX)
            .unwrap();
        assert!(matches!(found, PathSearchOutcome::Found(_)));
    }
}
