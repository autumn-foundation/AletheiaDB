//! Compass: Semantic Pathfinding.
//!
//! "Find the path that stays on topic."
//!
//! Standard pathfinding (like BFS or Dijkstra) finds the shortest structural path.
//! Compass finds a path between two nodes that maximizes semantic relevance to a target concept.
//!
//! # The Hook
//! Imagine you want to find a connection between "Alice" and "Bob", but you want the path
//! to be relevant to "Security". Compass weights the edges based on the semantic similarity
//! of the traversed nodes to the "Security" vector.
//!
//! # Example
//! ```rust,no_run
//! use aletheiadb::AletheiaDB;
//! use aletheiadb::experimental::reasoning::compass::Compass;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let db = AletheiaDB::new()?;
//! let compass = Compass::new(&db);
//!
//! # let start = aletheiadb::core::id::NodeId::new(1).unwrap();
//! # let end = aletheiadb::core::id::NodeId::new(2).unwrap();
//! # let target_concept = vec![1.0, 0.0, 0.0];
//! let path = compass.find_semantic_path(start, end, &target_concept, "embedding")?;
//! # Ok(())
//! # }
//! ```

use crate::AletheiaDB;
use crate::core::error::Result;
use crate::core::id::NodeId;
use crate::core::vector::ops::cosine_similarity;

use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashMap};

/// The Compass Engine for semantic pathfinding.
#[derive(Debug, Clone)]
pub struct Compass<'a> {
    db: &'a AletheiaDB,
}

#[derive(Debug, Clone, PartialEq)]
struct State {
    cost: f32,
    node: NodeId,
}

impl Eq for State {}

impl Ord for State {
    fn cmp(&self, other: &Self) -> Ordering {
        // Note the reverse order for min-heap
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

#[allow(clippy::collapsible_if)]
impl<'a> Compass<'a> {
    /// Creates a new Compass instance.
    pub fn new(db: &'a AletheiaDB) -> Self {
        Self { db }
    }

    /// Finds a path from `start` to `end` that minimizes semantic distance to `target_vector`.
    /// Nodes that are more similar to `target_vector` cost less to traverse.
    pub fn find_semantic_path(
        &self,
        start: NodeId,
        end: NodeId,
        target_vector: &[f32],
        vector_property: &str,
    ) -> Result<Option<Vec<NodeId>>> {
        let mut dist: HashMap<NodeId, f32> = HashMap::new();
        let mut prev: HashMap<NodeId, NodeId> = HashMap::new();
        let mut heap = BinaryHeap::new();

        dist.insert(start, 0.0);
        heap.push(State {
            cost: 0.0,
            node: start,
        });

        while let Some(State { cost, node }) = heap.pop() {
            if node == end {
                let mut path = Vec::new();
                let mut curr = end;
                while curr != start {
                    path.push(curr);
                    curr = prev[&curr];
                }
                path.push(start);
                path.reverse();
                return Ok(Some(path));
            }

            if cost > *dist.get(&node).unwrap_or(&f32::INFINITY) {
                continue;
            }

            // Using ReadOps directly on db
            let edges = self.db.get_outgoing_edges(node);
            for edge_id in edges {
                if let Ok(edge) = self.db.get_edge(edge_id) {
                    let neighbor = edge.target;

                    let mut similarity = 0.0;
                    if let Ok(neighbor_node) = self.db.get_node(neighbor) {
                        if let Some(v) = neighbor_node
                            .get_property(vector_property)
                            .and_then(|p| p.as_vector())
                        {
                            similarity = cosine_similarity(target_vector, v).unwrap_or(0.0);
                        }
                    }

                    let step_cost = 0.1 + (1.0 - similarity).clamp(0.0, 2.0);
                    let next_cost = cost + step_cost;

                    if next_cost < *dist.get(&neighbor).unwrap_or(&f32::INFINITY) {
                        heap.push(State {
                            cost: next_cost,
                            node: neighbor,
                        });
                        dist.insert(neighbor, next_cost);
                        prev.insert(neighbor, node);
                    }
                }
            }
        }

        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::AletheiaDB;
    use crate::core::property::PropertyMapBuilder;

    // ensures traits are in scope for .create_node etc if needed, or AletheiaDB provides it

    #[test]
    fn test_compass_pathfinding() {
        let db = AletheiaDB::new().unwrap();

        let target_concept = vec![1.0, 0.0, 0.0];

        let start = db
            .create_node(
                "Start",
                PropertyMapBuilder::new()
                    .insert_vector("vec", &[0.0, 0.0, 0.0])
                    .build(),
            )
            .unwrap();
        let good_node = db
            .create_node(
                "Good",
                PropertyMapBuilder::new()
                    .insert_vector("vec", &[0.9, 0.1, 0.0])
                    .build(),
            )
            .unwrap();
        let bad_node = db
            .create_node(
                "Bad",
                PropertyMapBuilder::new()
                    .insert_vector("vec", &[0.0, 1.0, 0.0])
                    .build(),
            )
            .unwrap();
        let end = db
            .create_node(
                "End",
                PropertyMapBuilder::new()
                    .insert_vector("vec", &[0.0, 0.0, 0.0])
                    .build(),
            )
            .unwrap();

        db.create_edge(start, good_node, "PATH", Default::default())
            .unwrap();
        db.create_edge(good_node, end, "PATH", Default::default())
            .unwrap();

        db.create_edge(start, bad_node, "PATH", Default::default())
            .unwrap();
        db.create_edge(bad_node, end, "PATH", Default::default())
            .unwrap();

        let compass = Compass::new(&db);
        let path = compass
            .find_semantic_path(start, end, &target_concept, "vec")
            .unwrap()
            .unwrap();

        assert_eq!(path, vec![start, good_node, end]);
    }
}
