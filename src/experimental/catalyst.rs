#![allow(clippy::collapsible_if)]

//! Catalyst: Synergy Maximization & Team Building 🧪
//!
//! "Who is the missing piece of the puzzle?"
//!
//! Catalyst identifies the nodes that, if added to an existing subgraph (e.g., a team),
//! would yield the highest positive change in its Synergy score.
//!
//! # Concepts
//! - **Catalyst Candidate**: A node outside the current subgraph that is structurally
//!   connected to it.
//! - **Synergy Delta**: The difference between the subgraph's Synergy score before and
//!   after the candidate is added.
//!
//! # Use Cases
//! - **Team Building**: Recommending a new team member who complements the current team's
//!   dynamics and maximizes collective emergence.
//! - **Concept Synthesis**: Suggesting the missing concept that brings disparate ideas together.
//!
//! # Example
//! ```rust
//! // Requires features = ["nova"]
//! use aletheiadb::AletheiaDB;
//! use aletheiadb::experimental::catalyst::Catalyst;
//! use aletheiadb::core::id::NodeId;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let db = AletheiaDB::new()?;
//! // ... setup graph ...
//! # let current_team = vec![];
//!
//! let catalyst = Catalyst::new(&db);
//! let best_candidates = catalyst.find_catalysts(&current_team, "embedding", 5)?;
//!
//! for (node_id, delta) in best_candidates {
//!     println!("Candidate: {}, Synergy Delta: +{:.4}", node_id, delta);
//! }
//! # Ok(())
//! # }
//! ```

use crate::AletheiaDB;
use crate::api::transaction::ReadOps;
use crate::core::error::{Error, Result};
use crate::core::id::NodeId;
use crate::experimental::synergy::Synergy;
use std::collections::HashSet;

/// The Catalyst Engine.
pub struct Catalyst<'a> {
    db: &'a AletheiaDB,
}

impl<'a> Catalyst<'a> {
    /// Create a new Catalyst Engine.
    pub fn new(db: &'a AletheiaDB) -> Self {
        Self { db }
    }

    /// Find the nodes that maximize the synergy of a given subgraph.
    ///
    /// # Arguments
    /// * `current_subgraph` - The list of node IDs currently in the subgraph.
    /// * `property_name` - The vector property to use for semantic analysis.
    /// * `k` - The maximum number of top candidates to return.
    ///
    /// # Returns
    /// A list of `(NodeId, synergy_delta)` tuples sorted by descending synergy delta.
    /// Only candidates with a positive synergy delta are returned.
    pub fn find_catalysts(
        &self,
        current_subgraph: &[NodeId],
        property_name: &str,
        k: usize,
    ) -> Result<Vec<(NodeId, f32)>> {
        if current_subgraph.is_empty() {
            return Err(Error::other("Cannot find catalysts for an empty subgraph"));
        }

        let synergy_engine = Synergy::new(self.db);

        // 1. Calculate current synergy
        // If the current subgraph has no vectors or fails to analyze,
        // we assume its current synergy is 0.0.
        let current_synergy = synergy_engine
            .calculate_synergy(current_subgraph, property_name)
            .unwrap_or(0.0);

        // 2. Find potential candidates
        // For efficiency in this prototype, we only consider direct neighbors of the
        // current subgraph that are not already in it.
        let mut candidates = HashSet::new();
        let current_set: HashSet<NodeId> = current_subgraph.iter().cloned().collect();

        self.db.read(|tx| {
            for &node_id in current_subgraph {
                // Outgoing neighbors
                let outgoing = tx.get_outgoing_edges(node_id);
                for edge_id in outgoing {
                    if let Ok(edge) = tx.get_edge(edge_id) {
                        if !current_set.contains(&edge.target) {
                            candidates.insert(edge.target);
                        }
                    }
                }

                // Incoming neighbors
                let incoming = tx.get_incoming_edges(node_id);
                for edge_id in incoming {
                    if let Ok(edge) = tx.get_edge(edge_id) {
                        if !current_set.contains(&edge.source) {
                            candidates.insert(edge.source);
                        }
                    }
                }
            }
            Ok::<(), Error>(())
        })?;

        // 3. Simulate adding each candidate and measure the delta
        let mut scored_candidates = Vec::new();

        for candidate in candidates {
            // Create a temporary subgraph including the candidate
            let mut temp_subgraph = current_subgraph.to_vec();
            temp_subgraph.push(candidate);

            // Calculate new synergy
            if let Ok(score) = synergy_engine.calculate_synergy(&temp_subgraph, property_name) {
                let delta = score - current_synergy;

                // Only care about positive contributions
                if delta > 0.0 {
                    scored_candidates.push((candidate, delta));
                }
            }
        }

        // 4. Sort by highest delta
        scored_candidates
            .sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

        // 5. Truncate to top K
        scored_candidates.truncate(k);

        Ok(scored_candidates)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::transaction::WriteOps;
    use crate::core::property::PropertyMapBuilder;

    #[test]
    fn test_catalyst_finds_missing_link() {
        let db = AletheiaDB::new().unwrap();

        let mut n1 = NodeId::new(0).unwrap();
        let mut n2 = NodeId::new(0).unwrap();
        let mut n3 = NodeId::new(0).unwrap();
        let mut n4 = NodeId::new(0).unwrap();

        db.write(|tx| {
            // Team member 1 (Tech)
            n1 = tx
                .create_node(
                    "Person",
                    PropertyMapBuilder::new()
                        .insert_vector("embedding", &[1.0, 0.0])
                        .build(),
                )
                .unwrap();

            // Team member 2 (Sales)
            n2 = tx
                .create_node(
                    "Person",
                    PropertyMapBuilder::new()
                        .insert_vector("embedding", &[1.0, 0.0]) // Same vector so they don't have divergence without n3
                        .build(),
                )
                .unwrap();

            // The Catalyst (Product Manager)
            // Structurally connected to both, vector bringing a new dimension
            n3 = tx
                .create_node(
                    "Person",
                    PropertyMapBuilder::new()
                        .insert_vector("embedding", &[0.0, 1.0])
                        .build(),
                )
                .unwrap();

            // Random disconnected node (HR)
            n4 = tx
                .create_node(
                    "Person",
                    PropertyMapBuilder::new()
                        .insert_vector("embedding", &[-1.0, -1.0])
                        .build(),
                )
                .unwrap();

            // Connections
            tx.create_edge(n1, n3, "WORKS_WITH", Default::default())
                .unwrap();
            tx.create_edge(n2, n3, "WORKS_WITH", Default::default())
                .unwrap();
            // N1 and N2 are not connected directly, they only connect through N3

            Ok::<(), Error>(())
        })
        .unwrap();

        let catalyst = Catalyst::new(&db);

        // Find catalysts for [n1, n2]
        // N3 is connected to both n1 and n2
        // In the beginning, n1 and n2 have synergy 0 (same vectors, no internal edges)
        // With n3, they are all connected to n3, adding a new orthogonal vector
        let best_candidates = catalyst.find_catalysts(&[n1, n2], "embedding", 5).unwrap();

        // Should return n3 as the top candidate with a positive synergy delta
        assert!(
            !best_candidates.is_empty(),
            "Should find at least one catalyst"
        );
        assert_eq!(best_candidates[0].0, n3, "N3 should be the best candidate");
        assert!(
            best_candidates[0].1 > 0.0,
            "Synergy delta should be positive"
        );
    }
}
