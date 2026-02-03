//! Semantic Navigator
//!
//! This experimental module implements A* pathfinding on the graph using vector similarity
//! as the heuristic and cost function. It enables finding "semantically smooth" paths
//! between concepts.

use crate::GallifreyDB;
use crate::core::id::NodeId;
use crate::core::vector::cosine_similarity;
use crate::utils::{Error, Result};
use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashMap};

/// A navigator that finds semantically meaningful paths through the graph.
pub struct SemanticNavigator<'a> {
    db: &'a GallifreyDB,
}

#[derive(Clone, Copy, PartialEq)]
struct State {
    cost: f32,
    node: NodeId,
}

impl Eq for State {}

// Priority queue depends on `Ord`.
// We flip the ordering on costs because BinaryHeap is a max-heap.
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
    pub fn new(db: &'a GallifreyDB) -> Self {
        Self { db }
    }

    /// Find a path from `start` to `end` using vector similarity on `vector_prop`.
    ///
    /// The cost function is `1.0 - similarity(current, next)`.
    /// The heuristic is `1.0 - similarity(next, goal)`.
    ///
    /// If a node is missing the vector property, the transition cost is penalized (1.0).
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

        let mut came_from: HashMap<NodeId, NodeId> = HashMap::new();
        let mut g_score: HashMap<NodeId, f32> = HashMap::new();
        g_score.insert(start, 0.0);

        let mut f_score: HashMap<NodeId, f32> = HashMap::new();
        // h(start) = 1.0 - sim(start, end)
        // We know start has a vector.
        let h_start = 1.0 - cosine_similarity(&_start_vec, &end_vec)?;
        f_score.insert(start, h_start);

        while let Some(State {
            cost: _current_f,
            node: current,
        }) = open_set.pop()
        {
            if current == end {
                return Ok(self.reconstruct_path(came_from, current));
            }

            // Get current vector for cost calculation
            // We fetch it again here. Optimization: Cache vectors?
            // For now, rely on hot path speed.
            let current_node = if current == start {
                start_node.clone() // We have it already
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
                    _ => 1.0, // Penalize missing vectors
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
    use crate::core::property::PropertyMapBuilder;

    #[test]
    fn test_semantic_path_linear() {
        let db = GallifreyDB::new().unwrap();

        // A -> B -> C
        // Vectors:
        // A: [1.0, 0.0]
        // B: [0.7, 0.7] (Halfway)
        // C: [0.0, 1.0]

        // Edge A->B, B->C.
        // Also add A->D->C where D is [0.0, -1.0] (Opposite)
        // Semantic path should prefer B.

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

        assert_eq!(path, vec![a, b, c]);
    }

    #[test]
    fn test_missing_vector_fail() {
        let db = GallifreyDB::new().unwrap();
        let a = db
            .create_node("Node", PropertyMapBuilder::new().build())
            .unwrap();
        let b = db
            .create_node("Node", PropertyMapBuilder::new().build())
            .unwrap();

        let nav = SemanticNavigator::new(&db);
        let result = nav.find_path(a, b, "vec");
        assert!(result.is_err());
    }
}
