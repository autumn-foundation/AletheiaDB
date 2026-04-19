//! Wayfarer: Semantic Pathfinding.
//!
//! "Find the semantic path of least resistance."
//!
//! Wayfarer uses A* search combined with vector cosine similarity as a distance heuristic
//! to find the most "semantically aligned" path between two nodes in the knowledge graph.
//!
//! # Use Cases
//! - **Concept Interpolation**: Finding a chain of ideas connecting two distant concepts.
//! - **Recommendation Routing**: "How do I gently transition a user from Topic A to Topic B?"
//! - **Narrative Generation**: Plotting a sequence of events that semantically bridge a gap.

use crate::AletheiaDB;
use crate::core::error::{Error, Result};
use crate::core::id::NodeId;
use crate::core::vector::ops;
use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashMap};

/// A path found by the Wayfarer.
#[derive(Debug, Clone, PartialEq)]
pub struct SemanticPath {
    /// The sequence of nodes from start to end.
    pub nodes: Vec<NodeId>,
    /// The total semantic distance (lower is more aligned).
    pub total_distance: f32,
}

/// The Wayfarer Engine.
pub struct Wayfarer<'a> {
    db: &'a AletheiaDB,
}

impl<'a> Wayfarer<'a> {
    /// Create a new Wayfarer instance.
    pub fn new(db: &'a AletheiaDB) -> Self {
        Self { db }
    }

    /// Find the shortest semantic path between `start` and `target`.
    ///
    /// # Arguments
    /// * `start`: The starting node.
    /// * `target`: The target node.
    /// * `property_name`: The vector property to use for semantic distance.
    /// * `max_depth`: Maximum number of hops to explore.
    ///
    /// # Returns
    /// The optimal `SemanticPath`, or `None` if no path exists within `max_depth` or if nodes lack the required property.
    pub fn navigate(
        &self,
        start: NodeId,
        target: NodeId,
        property_name: &str,
        max_depth: usize,
    ) -> Result<Option<SemanticPath>> {
        #[derive(PartialEq)]
        struct State {
            cost: f32,
            position: NodeId,
            depth: usize,
        }

        impl Eq for State {}

        impl Ord for State {
            fn cmp(&self, other: &Self) -> Ordering {
                // Notice that we flip the ordering on costs.
                // In case of a tie we compare positions - this step is necessary
                // to make implementations of `PartialEq` and `Ord` consistent.
                other
                    .cost
                    .partial_cmp(&self.cost)
                    .unwrap_or(Ordering::Equal)
                    .then_with(|| self.position.cmp(&other.position))
            }
        }

        impl PartialOrd for State {
            fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
                Some(self.cmp(other))
            }
        }

        let target_node = self
            .db
            .get_node(target)
            .map_err(|_| Error::other("Target node not found"))?;
        let target_vec_prop = target_node
            .get_property(property_name)
            .ok_or_else(|| Error::other("Target node missing property"))?;
        let target_vec = target_vec_prop
            .as_vector()
            .ok_or_else(|| Error::other("Target property is not a vector"))?;

        let mut target_normalized = target_vec.to_vec();
        ops::normalize_in_place(&mut target_normalized);

        let mut heap = BinaryHeap::new();
        let mut came_from: HashMap<NodeId, NodeId> = HashMap::new();
        let mut cost_so_far: HashMap<NodeId, f32> = HashMap::new();

        heap.push(State {
            cost: 0.0,
            position: start,
            depth: 0,
        });
        cost_so_far.insert(start, 0.0);

        while let Some(State {
            cost: _,
            position,
            depth,
        }) = heap.pop()
        {
            if position == target {
                let mut path = vec![target];
                let mut current = target;
                while let Some(&prev) = came_from.get(&current) {
                    path.push(prev);
                    current = prev;
                }
                path.reverse();

                let total_dist = *cost_so_far.get(&target).unwrap_or(&0.0);
                return Ok(Some(SemanticPath {
                    nodes: path,
                    total_distance: total_dist,
                }));
            }

            if depth >= max_depth {
                continue;
            }

            for edge_id in self.db.get_outgoing_edges(position) {
                if let Ok(target_node_id) = self.db.get_edge_target(edge_id) {
                    if let Ok(next_node) = self.db.get_node(target_node_id) {
                        let prop_vec = next_node
                            .get_property(property_name)
                            .and_then(|p| p.as_vector());

                        if let Some(vec) = prop_vec {
                            let mut next_normalized = vec.to_vec();
                            ops::normalize_in_place(&mut next_normalized);

                            // Cost is distance: 1.0 - similarity
                            let similarity =
                                ops::cosine_similarity(&next_normalized, &target_normalized)
                                    .unwrap_or(0.0);
                            let distance = (1.0 - similarity).max(0.0);

                            let new_cost = cost_so_far.get(&position).unwrap_or(&0.0) + distance;

                            if !cost_so_far.contains_key(&target_node_id)
                                || new_cost < *cost_so_far.get(&target_node_id).unwrap()
                            {
                                cost_so_far.insert(target_node_id, new_cost);
                                // A* heuristic: we use the distance itself as the heuristic here,
                                // which is simplistic but fits the semantic pathfinding theme.
                                let priority = new_cost + distance;
                                heap.push(State {
                                    cost: priority,
                                    position: target_node_id,
                                    depth: depth + 1,
                                });
                                came_from.insert(target_node_id, position);
                            }
                        }
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

    use crate::core::property::PropertyMapBuilder;

    #[test]
    fn test_wayfarer_basic_path() {
        let db = AletheiaDB::new().unwrap();

        // Target vector at [1.0, 0.0]
        let props_target = PropertyMapBuilder::new()
            .insert_vector("vec", &[1.0, 0.0])
            .build();
        let target = db.create_node("Target", props_target).unwrap();

        // Start vector at [0.0, 1.0]
        let props_start = PropertyMapBuilder::new()
            .insert_vector("vec", &[0.0, 1.0])
            .build();
        let start = db.create_node("Start", props_start).unwrap();

        // Intermediate A: Closer to target semantically [0.8, 0.2]
        let props_a = PropertyMapBuilder::new()
            .insert_vector("vec", &[0.8, 0.2])
            .build();
        let a = db.create_node("A", props_a).unwrap();

        // Intermediate B: Farther from target semantically [-1.0, 0.0]
        let props_b = PropertyMapBuilder::new()
            .insert_vector("vec", &[-1.0, 0.0])
            .build();
        let b = db.create_node("B", props_b).unwrap();

        // Connect graph
        // Start -> A -> Target
        // Start -> B -> Target
        db.create_edge(start, a, "NEXT", Default::default())
            .unwrap();
        db.create_edge(a, target, "NEXT", Default::default())
            .unwrap();
        db.create_edge(start, b, "NEXT", Default::default())
            .unwrap();
        db.create_edge(b, target, "NEXT", Default::default())
            .unwrap();

        let wayfarer = Wayfarer::new(&db);

        let path = wayfarer
            .navigate(start, target, "vec", 5)
            .unwrap()
            .expect("Should find a path");

        // The path should prefer A over B because A is semantically closer to the target
        assert_eq!(path.nodes, vec![start, a, target]);
        assert!(path.total_distance > 0.0);
    }
}
