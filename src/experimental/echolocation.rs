//! Echolocation: Probing the Semantic Substrate.
//!
//! "Ping..."
//!
//! Echolocation emits a semantic "ping" (a vector) to a set of nodes and measures
//! how the graph resonates with it, returning a topological map of the responses.
//! It accounts for "reverberation" - if a node resonates strongly, its neighbors
//! also get a boost.
//!
//! # Concepts
//! - **Ping**: A query vector representing a concept or question.
//! - **Echo**: The similarity response from nodes in the graph.
//! - **Reverberation**: How the echo propagates through structural connections.
//!
//! # Use Cases
//! - **Topic Discovery**: "Where in the graph is the topic of 'AI Safety' most concentrated?"
//! - **Context Isolation**: Extracting an active sub-network that strongly resonates with an idea.
//!
//! # Example
//! ```rust
//! // Requires features = ["nova"]
//! use aletheiadb::AletheiaDB;
//! use aletheiadb::experimental::echolocation::Echolocation;
//! use aletheiadb::core::id::NodeId;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let db = AletheiaDB::new()?;
//! let echolocation = Echolocation::new(&db);
//!
//! let ping_vector = vec![1.0, 0.0];
//! # let candidates = vec![];
//!
//! // Ping the graph
//! let echoes = echolocation.ping(&ping_vector, &candidates, "embedding", 2)?;
//!
//! for echo in echoes {
//!     println!("Node {} resonated with strength {}", echo.node_id, echo.strength);
//! }
//! # Ok(())
//! # }
//! ```

use crate::AletheiaDB;
use crate::api::transaction::ReadOps;
use crate::core::error::{Error, Result};
use crate::core::id::NodeId;
use crate::core::vector::cosine_similarity;
use std::cmp::Ordering;
use std::collections::{HashMap, VecDeque};

/// An Echo response from a node.
#[derive(Debug, Clone, PartialEq)]
pub struct Echo {
    /// The node that responded.
    pub node_id: NodeId,
    /// The final resonance strength (combination of direct similarity and reverberation).
    pub strength: f32,
}

impl Eq for Echo {}

impl Ord for Echo {
    fn cmp(&self, other: &Self) -> Ordering {
        // We want a max-heap, so we reverse the comparison
        self.strength
            .partial_cmp(&other.strength)
            .unwrap_or(Ordering::Equal)
    }
}

impl PartialOrd for Echo {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// The Echolocation Engine.
pub struct Echolocation<'a> {
    db: &'a AletheiaDB,
}

impl<'a> Echolocation<'a> {
    /// Create a new Echolocation instance.
    pub fn new(db: &'a AletheiaDB) -> Self {
        Self { db }
    }

    /// Emit a semantic ping to a set of candidates and measure the graph's resonance.
    ///
    /// # Arguments
    /// * `ping_vector` - The vector to ping with.
    /// * `candidates` - The nodes to consider in the analysis.
    /// * `property_name` - The name of the vector property to compare against.
    /// * `reverberation_depth` - How many structural hops the echo should propagate.
    pub fn ping(
        &self,
        ping_vector: &[f32],
        candidates: &[NodeId],
        property_name: &str,
        reverberation_depth: usize,
    ) -> Result<Vec<Echo>> {
        if ping_vector.is_empty() {
            return Err(Error::other("Ping vector cannot be empty"));
        }

        if candidates.is_empty() {
            return Ok(Vec::new());
        }

        let mut direct_echoes: HashMap<NodeId, f32> = HashMap::new();
        let mut final_echoes: HashMap<NodeId, f32> = HashMap::new();

        self.db.read(|tx| {
            // 1. Initial Ping (Direct Similarity)
            for &node_id in candidates {
                if let Ok(node) = tx.get_node(node_id) {
                    #[allow(clippy::collapsible_if)]
                    if let Some(prop) = node.get_property(property_name).and_then(|p| p.as_vector())
                    {
                        let sim = cosine_similarity(ping_vector, prop).unwrap_or(0.0);
                        // Normalize similarity to 0.0 - 1.0 range (cosine is -1 to 1)
                        let normalized_sim = (sim + 1.0) / 2.0;
                        direct_echoes.insert(node_id, normalized_sim);
                        final_echoes.insert(node_id, normalized_sim); // Base strength
                    }
                }
            }

            // 2. Reverberation (Structural Propagation)
            // If a node resonated strongly, it "lights up" its neighbors.
            let decay_factor = 0.5; // How much signal is lost per hop

            for &start_node in candidates {
                let initial_strength = *direct_echoes.get(&start_node).unwrap_or(&0.0);

                if initial_strength <= 0.0 {
                    continue;
                }

                let mut queue = VecDeque::new();
                queue.push_back((start_node, 0, initial_strength));

                let mut visited = HashMap::new();
                visited.insert(start_node, 0);

                while let Some((current, depth, current_strength)) = queue.pop_front() {
                    if depth >= reverberation_depth {
                        continue;
                    }

                    // Propagate to neighbors
                    let outgoing = tx.get_outgoing_edges(current);
                    for edge_id in outgoing {
                        if let Ok(edge) = tx.get_edge(edge_id) {
                            let target = edge.target;

                            // Only reverberate to nodes in our candidate set to avoid exploding the search
                            if direct_echoes.contains_key(&target) {
                                let new_depth = depth + 1;

                                // Check if we already visited this node at a better or equal depth from this start_node
                                #[allow(clippy::collapsible_if)]
                                if let Some(&best_depth) = visited.get(&target) {
                                    if new_depth >= best_depth {
                                        continue;
                                    }
                                }

                                visited.insert(target, new_depth);

                                let reverberated_strength = current_strength * decay_factor;

                                // Add reverberated strength to the target's final echo
                                final_echoes
                                    .entry(target)
                                    .and_modify(|s| *s += reverberated_strength);

                                queue.push_back((target, new_depth, reverberated_strength));
                            }
                        }
                    }
                }
            }

            Ok::<(), Error>(())
        })?;

        // 3. Format and Sort Results
        let mut results: Vec<Echo> = final_echoes
            .into_iter()
            .map(|(node_id, strength)| Echo { node_id, strength })
            .collect();

        // Sort descending by strength
        results.sort_by(|a, b| {
            b.strength
                .partial_cmp(&a.strength)
                .unwrap_or(Ordering::Equal)
        });

        Ok(results)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::transaction::WriteOps;
    use crate::core::property::PropertyMapBuilder;

    #[test]
    fn test_echolocation_direct_hit() {
        let db = AletheiaDB::new().unwrap();

        let mut n1 = NodeId::new(0).unwrap();
        let mut n2 = NodeId::new(0).unwrap();

        db.write(|tx| {
            n1 = tx
                .create_node(
                    "Node",
                    PropertyMapBuilder::new()
                        .insert_vector("vec", &[1.0, 0.0])
                        .build(),
                )
                .unwrap();

            n2 = tx
                .create_node(
                    "Node",
                    PropertyMapBuilder::new()
                        .insert_vector("vec", &[-1.0, 0.0])
                        .build(),
                )
                .unwrap();
            Ok::<(), Error>(())
        })
        .unwrap();

        let echo = Echolocation::new(&db);
        let candidates = vec![n1, n2];

        // Ping with [1.0, 0.0], n1 should resonate highly, n2 should not
        let results = echo.ping(&[1.0, 0.0], &candidates, "vec", 0).unwrap();

        assert_eq!(results.len(), 2);
        assert_eq!(results[0].node_id, n1);
        assert!(results[0].strength > results[1].strength);
        assert!(results[0].strength > 0.9); // Cosine ~ 1.0 -> Normalized ~ 1.0
        assert!(results[1].strength < 0.1); // Cosine ~ -1.0 -> Normalized ~ 0.0
    }

    #[test]
    fn test_echolocation_reverberation() {
        let db = AletheiaDB::new().unwrap();

        let mut n1 = NodeId::new(0).unwrap(); // Direct hit
        let mut n2 = NodeId::new(0).unwrap(); // Orthogonal, but connected
        let mut n3 = NodeId::new(0).unwrap(); // Orthogonal, disconnected

        db.write(|tx| {
            n1 = tx
                .create_node(
                    "Node",
                    PropertyMapBuilder::new()
                        .insert_vector("vec", &[1.0, 0.0])
                        .build(),
                )
                .unwrap();

            n2 = tx
                .create_node(
                    "Node",
                    PropertyMapBuilder::new()
                        .insert_vector("vec", &[0.0, 1.0])
                        .build(),
                )
                .unwrap();

            n3 = tx
                .create_node(
                    "Node",
                    PropertyMapBuilder::new()
                        .insert_vector("vec", &[0.0, 1.0])
                        .build(),
                )
                .unwrap();

            // Connect n1 to n2
            tx.create_edge(n1, n2, "LINK", Default::default()).unwrap();

            Ok::<(), Error>(())
        })
        .unwrap();

        let echo = Echolocation::new(&db);
        let candidates = vec![n1, n2, n3];

        // Ping with 0 reverberation (direct only)
        let direct_results = echo.ping(&[1.0, 0.0], &candidates, "vec", 0).unwrap();

        let n2_direct_strength = direct_results
            .iter()
            .find(|e| e.node_id == n2)
            .unwrap()
            .strength;
        let n3_direct_strength = direct_results
            .iter()
            .find(|e| e.node_id == n3)
            .unwrap()
            .strength;

        // Both n2 and n3 should have identical base strength (cosine sim 0 -> normalized 0.5)
        assert!((n2_direct_strength - n3_direct_strength).abs() < 0.001);

        // Ping with 1 hop reverberation
        let reverb_results = echo.ping(&[1.0, 0.0], &candidates, "vec", 1).unwrap();

        let n2_reverb_strength = reverb_results
            .iter()
            .find(|e| e.node_id == n2)
            .unwrap()
            .strength;
        let n3_reverb_strength = reverb_results
            .iter()
            .find(|e| e.node_id == n3)
            .unwrap()
            .strength;

        // n2 should now have a higher strength than n3 because it received a reverberation from n1
        assert!(n2_reverb_strength > n3_reverb_strength);
    }
}
