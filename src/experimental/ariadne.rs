//! Ariadne: Semantic Thread Weaver.
//!
//! "Follow the thread."
//!
//! Ariadne weaves narrative threads through the graph by connecting events
//! based on explicit causality (edges) and implicit semantic similarity (vectors),
//! while respecting the arrow of time.
//!
//! # Concepts
//! - **Thread**: A sequence of nodes (events) connected by causal links.
//! - **Warp (Edges)**: Explicit connections (e.g., `NEXT`, `CAUSED_BY`).
//! - **Weft (Vectors)**: Implicit connections based on semantic similarity.
//! - **Time Arrow**: Threads must move forward in time.

use crate::AletheiaDB;
use crate::core::id::NodeId;
use crate::utils::error::Result;
use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashSet};

/// A step in a narrative thread.
#[derive(Debug, Clone)]
pub struct ThreadStep {
    /// The node at this step.
    pub node_id: NodeId,
    /// The time of this event (from property).
    pub timestamp: i64,
    /// How we reached this node.
    pub connection_type: ConnectionType,
}

/// How a step is connected to the previous one.
#[derive(Debug, Clone, PartialEq)]
pub enum ConnectionType {
    /// Start of the thread.
    Start,
    /// Explicit edge traversal.
    Edge { label: String },
    /// Implicit semantic jump via vector similarity.
    SemanticJump { similarity: f32 },
}

/// Internal state for the priority queue.
#[derive(Debug)]
struct SearchState {
    node_id: NodeId,
    timestamp: i64,
    g_cost: f32, // Actual cost from start
    f_cost: f32, // Total estimated cost (g + h)
    path: Vec<ThreadStep>,
}

impl PartialEq for SearchState {
    fn eq(&self, other: &Self) -> bool {
        self.f_cost == other.f_cost
    }
}

impl Eq for SearchState {}

impl Ord for SearchState {
    fn cmp(&self, other: &Self) -> Ordering {
        // Reverse for min-heap (lowest cost first)
        other
            .f_cost
            .partial_cmp(&self.f_cost)
            .unwrap_or(Ordering::Equal)
    }
}

impl PartialOrd for SearchState {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// The Ariadne Weaver engine.
pub struct Ariadne<'a> {
    db: &'a AletheiaDB,
}

impl<'a> Ariadne<'a> {
    /// Create a new Ariadne instance.
    pub fn new(db: &'a AletheiaDB) -> Self {
        Self { db }
    }

    /// Weave a narrative thread from a start node.
    ///
    /// This explores the graph to find a coherent sequence of events starting from `start_node`.
    /// It prefers explicit edges but will "jump" via vector similarity if needed to continue the story.
    ///
    /// # Arguments
    /// * `start_node` - The starting event.
    /// * `goal_node` - Optional target event to steer towards.
    /// * `time_property` - The property key containing the event timestamp (must be integer).
    /// * `vector_property` - The property key containing the vector embedding.
    /// * `max_steps` - Maximum length of the thread.
    /// * `beam_width` - Number of candidates to consider at each step.
    pub fn weave(
        &self,
        start_node: NodeId,
        goal_node: Option<NodeId>,
        time_property: &str,
        vector_property: &str,
        max_steps: usize,
        beam_width: usize,
    ) -> Result<Vec<ThreadStep>> {
        let start_time = self.get_time(start_node, time_property)?;

        let start_step = ThreadStep {
            node_id: start_node,
            timestamp: start_time,
            connection_type: ConnectionType::Start,
        };

        // If goal is provided, get its vector for heuristic
        let goal_vector = if let Some(goal) = goal_node {
            if let Ok(node) = self.db.get_node(goal) {
                node.properties
                    .get(vector_property)
                    .and_then(|v| v.as_vector())
                    .map(|v| v.to_vec())
            } else {
                None
            }
        } else {
            None
        };

        let mut pq = BinaryHeap::new();
        let heuristic = self.calculate_heuristic(start_node, vector_property, &goal_vector);
        pq.push(SearchState {
            node_id: start_node,
            timestamp: start_time,
            g_cost: 0.0,
            f_cost: heuristic,
            path: vec![start_step],
        });

        let mut visited = HashSet::new();
        visited.insert(start_node);

        let mut best_path = Vec::new();
        let mut max_path_len = 0;

        while let Some(state) = pq.pop() {
            // Check if we reached goal
            if let Some(goal) = goal_node {
                if state.node_id == goal {
                    return Ok(state.path);
                }
            }

            // Keep track of the longest valid path found so far
            if state.path.len() > max_path_len {
                max_path_len = state.path.len();
                best_path = state.path.clone();
            }

            if state.path.len() >= max_steps {
                continue;
            }

            // 1. Gather candidates from explicit edges
            // We favor explicit edges (low cost)
            let edges = self.db.current.get_outgoing_edges(state.node_id);
            for edge_id in edges {
                if let Ok(edge) = self.db.get_edge(edge_id) {
                    let target = edge.target;
                    if visited.contains(&target) {
                        continue;
                    }

                    if let Ok(target_time) = self.get_time(target, time_property) {
                        if target_time >= state.timestamp {
                            // Valid explicit step
                            let label = self.resolve_label(edge.label);
                            let mut new_path = state.path.clone();
                            new_path.push(ThreadStep {
                                node_id: target,
                                timestamp: target_time,
                                connection_type: ConnectionType::Edge { label },
                            });

                            let heuristic =
                                self.calculate_heuristic(target, vector_property, &goal_vector);

                            // Edges are cheap (cost += 1.0)
                            let new_g = state.g_cost + 1.0;
                            let f_cost = new_g + heuristic;
                            pq.push(SearchState {
                                node_id: target,
                                timestamp: target_time,
                                g_cost: new_g,
                                f_cost,
                                path: new_path,
                            });
                        }
                    }
                }
            }

            // 2. Gather candidates from semantic jumps
            // Only if we haven't found enough explicit edges, or to explore alternatives.
            // We search for vectors similar to the CURRENT node's vector.
            if let Ok(current_node) = self.db.get_node(state.node_id) {
                if let Some(current_vector) = current_node
                    .properties
                    .get(vector_property)
                    .and_then(|v| v.as_vector())
                {
                    let min_time = state.timestamp;

                    // Use the custom predicate to filter by time!
                    let candidates = self.db.find_similar_with_predicate(
                        vector_property,
                        current_vector,
                        beam_width, // Get top-k candidates
                        |candidate_id| {
                            // Filter: Candidate must be after current time
                            if *candidate_id == state.node_id {
                                return false;
                            } // Exclude self

                            // Check time property
                            if let Ok(node) = self.db.get_node(*candidate_id) {
                                if let Some(val) = node.properties.get(time_property) {
                                    let time = val.as_int().unwrap_or(0);
                                    return time >= min_time;
                                }
                            }
                            false
                        },
                    );

                    if let Ok(results) = candidates {
                        for (target, score) in results {
                            if visited.contains(&target) {
                                continue;
                            }

                            // Calculate timestamp again (or we could have returned it from predicate if signature allowed)
                            if let Ok(target_time) = self.get_time(target, time_property) {
                                let mut new_path = state.path.clone();
                                new_path.push(ThreadStep {
                                    node_id: target,
                                    timestamp: target_time,
                                    connection_type: ConnectionType::SemanticJump {
                                        similarity: score,
                                    },
                                });

                                let heuristic =
                                    self.calculate_heuristic(target, vector_property, &goal_vector);

                                // Jumps are more expensive than edges.
                                // Cost based on dissimilarity (1.0 - score).
                                // Multiplier to discourage jumps if edges exist.
                                // Base cost of 1.5 ensures edges (cost 1.0) are preferred even for identical vectors.
                                let jump_cost = 1.5 + (1.0 - score) * 5.0;

                                let new_g = state.g_cost + jump_cost;
                                let f_cost = new_g + heuristic;
                                pq.push(SearchState {
                                    node_id: target,
                                    timestamp: target_time,
                                    g_cost: new_g,
                                    f_cost,
                                    path: new_path,
                                });
                            }
                        }
                    }
                }
            }
        }

        Ok(best_path)
    }

    fn get_time(&self, node_id: NodeId, property: &str) -> Result<i64> {
        let node = self.db.get_node(node_id)?;
        let val = node.properties.get(property).ok_or_else(|| {
            crate::utils::error::Error::Storage(
                crate::utils::error::StorageError::PropertyNotFound(property.to_string()),
            )
        })?;
        val.as_int().ok_or_else(|| {
            crate::utils::error::Error::Query(crate::utils::error::QueryError::TypeMismatch {
                expected: "Integer".to_string(),
                actual: format!("{:?}", val),
            })
        })
    }

    fn resolve_label(&self, label_id: crate::core::interning::InternedString) -> String {
        use crate::core::interning::GLOBAL_INTERNER;
        GLOBAL_INTERNER
            .resolve_with(label_id, |s| s.to_string())
            .unwrap_or_else(|| "unknown".to_string())
    }

    fn calculate_heuristic(
        &self,
        node_id: NodeId,
        vector_prop: &str,
        goal_vector: &Option<Vec<f32>>,
    ) -> f32 {
        if let Some(goal) = goal_vector {
            if let Ok(node) = self.db.get_node(node_id) {
                if let Some(vec) = node.properties.get(vector_prop).and_then(|v| v.as_vector()) {
                    // Simple Euclidean distance or 1-Cosine
                    // Let's assume Cosine for now (normalized vectors)
                    // HNSW usually returns similarity.
                    // We need a cost.
                    // Let's calculate manual cosine distance.
                    // AletheiaDB doesn't expose a raw math util easily here, so implement basic dot product
                    let dot: f32 = vec.iter().zip(goal.iter()).map(|(a, b)| a * b).sum();
                    // Assuming normalized vectors, cosine dist = 1 - dot
                    return (1.0 - dot).max(0.0);
                }
            }
        }
        0.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::transaction::WriteOps;
    use crate::core::property::PropertyMapBuilder;
    use crate::index::vector::{DistanceMetric, HnswConfig};

    #[test]
    fn test_ariadne_weaving() {
        let db = AletheiaDB::new().unwrap();

        // Setup vector index
        let config = HnswConfig::new(2, DistanceMetric::Cosine);
        db.enable_vector_index("embedding", config).unwrap();

        // Story: A -> B -> (Jump) -> C -> D
        // Times: 10 -> 20 -> (Jump) -> 30 -> 40

        // Event A: Start (Time 10)
        let props_a = PropertyMapBuilder::new()
            .insert("time", 10i64)
            .insert("name", "A")
            .insert_vector("embedding", &[1.0, 0.0])
            .build();
        let a = db.create_node("Event", props_a).unwrap();

        // Event B: Connected to A (Time 20)
        // Vector is close to A but distinct enough that Edge is cheaper than Jump to C
        let props_b = PropertyMapBuilder::new()
            .insert("time", 20i64)
            .insert("name", "B")
            .insert_vector("embedding", &[0.9, 0.4]) // Sim to A: 0.9 (approx)
            .build();
        let b = db.create_node("Event", props_b).unwrap();
        db.create_edge(a, b, "NEXT", PropertyMapBuilder::new().build())
            .unwrap();

        // Event C: Disconnected but semantically close to B (Time 30)
        // Vector is far from A, close to B
        let props_c = PropertyMapBuilder::new()
            .insert("time", 30i64)
            .insert("name", "C")
            .insert_vector("embedding", &[0.5, 0.8])
            .build();
        let c = db.create_node("Event", props_c).unwrap();

        // Event D: Connected to C (Time 40)
        let props_d = PropertyMapBuilder::new()
            .insert("time", 40i64)
            .insert("name", "D")
            .insert_vector("embedding", &[0.0, 1.0])
            .build();
        let d = db.create_node("Event", props_d).unwrap();
        db.create_edge(c, d, "NEXT", PropertyMapBuilder::new().build())
            .unwrap();

        // Event E: Past event (Time 5), semantically close to B. Should NOT be picked.
        let props_e = PropertyMapBuilder::new()
            .insert("time", 5i64)
            .insert("name", "E")
            .insert_vector("embedding", &[0.9, 0.4])
            .build();
        let _e = db.create_node("Event", props_e).unwrap();

        let ariadne = Ariadne::new(&db);

        // Weave from A to D
        let path = ariadne
            .weave(
                a,
                Some(d),
                "time",
                "embedding",
                10,
                100, // High beam width to ensure we find candidates
            )
            .unwrap();

        // Expected Path: A -> (Edge) -> B -> (Jump) -> C -> (Edge) -> D
        if path.len() != 4 {
            println!(
                "Path found: {:?}",
                path.iter().map(|s| s.node_id).collect::<Vec<_>>()
            );
        }

        assert_eq!(path.len(), 4);
        assert_eq!(path[0].node_id, a);
        assert_eq!(path[1].node_id, b);
        assert_eq!(path[2].node_id, c);
        assert_eq!(path[3].node_id, d);

        // Verify connection types
        assert!(matches!(
            path[1].connection_type,
            ConnectionType::Edge { .. }
        ));
        assert!(matches!(
            path[2].connection_type,
            ConnectionType::SemanticJump { .. }
        ));
        assert!(matches!(
            path[3].connection_type,
            ConnectionType::Edge { .. }
        ));
    }
}
