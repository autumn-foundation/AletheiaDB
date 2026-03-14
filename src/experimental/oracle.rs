//! Oracle: Probabilistic Graph Reasoning.
//!
//! "Stop guessing. Ask the Oracle."
//!
//! The Oracle uses Monte Carlo simulations to answer probabilistic questions about the graph.
//! - "Who is the most relevant node to X?" (Personalized PageRank)
//! - "What is the probability of reaching Y from X?" (Reachability)
//!
//! # Use Cases
//! - **Influence Analysis**: Identify key influencers in a local context.
//! - **Risk Assessment**: "If this server fails, what is the probability it cascades to the database?"
//! - **Recommendation**: "Users who viewed X are 80% likely to view Y."

use crate::AletheiaDB;
use crate::core::error::{Result, StorageError};
use crate::core::hasher::IdentityHasher;
use crate::core::id::NodeId;
use rand::prelude::*;
use std::collections::HashMap;
use std::hash::BuildHasherDefault;

/// The Oracle Engine.
pub struct Oracle<'a> {
    db: &'a AletheiaDB,
}

impl<'a> Oracle<'a> {
    /// Create a new Oracle instance.
    pub fn new(db: &'a AletheiaDB) -> Self {
        Self { db }
    }

    /// Calculate Personalized PageRank (PPR) starting from a seed node.
    ///
    /// This simulates random walks starting from the seed node.
    /// At each step, the walk either:
    /// - Terminates with probability `alpha` (restart probability).
    /// - Moves to a random neighbor.
    ///
    /// # Arguments
    /// * `seed`: The starting node.
    /// * `alpha`: Teleport probability (probability of restart/termination). Typical value 0.15.
    /// * `num_walks`: Number of random walks to perform.
    /// * `max_steps`: Maximum steps per walk (to prevent infinite loops in cycles).
    ///
    /// # Returns
    /// A map of NodeId -> Visit Count (normalized probability).
    pub fn personalized_page_rank(
        &self,
        seed: NodeId,
        alpha: f32,
        num_walks: usize,
        max_steps: usize,
    ) -> Result<HashMap<NodeId, f32>> {
        // PERFORMANCE: Use IdentityHasher instead of the default SipHash for the `visits` map.
        // `NodeId` is a wrapper around a unique `u64`. In a Monte Carlo simulation (random walks),
        // we perform millions of map insertions and lookups. Bypassing SipHash eliminates
        // unnecessary hashing overhead on already-unique integer keys, significantly improving throughput.
        let mut visits: HashMap<NodeId, usize, BuildHasherDefault<IdentityHasher>> =
            HashMap::default();
        let mut rng = rand::thread_rng();

        // Ensure seed exists
        if self.db.get_node(seed).is_err() {
            return Err(crate::core::error::Error::Storage(
                StorageError::NodeNotFound(seed),
            ));
        }

        for _ in 0..num_walks {
            let mut current = seed;
            // Count start node
            *visits.entry(current).or_default() += 1;

            for _step in 0..max_steps {
                // Check for restart/termination
                if rng.r#gen::<f32>() < alpha {
                    break;
                }

                // Get outgoing edges
                let edges = self.db.get_outgoing_edges(current);
                if edges.is_empty() {
                    break; // Stuck at sink
                }

                // Pick random neighbor
                let edge_idx = rng.gen_range(0..edges.len());
                let edge_id = edges[edge_idx];

                // Move
                // If edge target is invalid (shouldn't happen in consistent DB), we stop.
                match self.db.get_edge_target(edge_id) {
                    Ok(target) => {
                        current = target;
                        *visits.entry(current).or_default() += 1;
                    }
                    Err(_) => break,
                }
            }
        }

        // Normalize
        let total_visits: usize = visits.values().sum();
        let mut scores = HashMap::new();

        if total_visits > 0 {
            for (node, count) in visits {
                scores.insert(node, count as f32 / total_visits as f32);
            }
        } else {
            // Should not happen if num_walks > 0, as start node is always visited
            scores.insert(seed, 1.0);
        }

        Ok(scores)
    }

    /// Estimate the probability of reaching `target` from `start` within `max_steps`.
    ///
    /// # Arguments
    /// * `start`: Starting node.
    /// * `target`: Target node.
    /// * `max_steps`: Maximum depth to search.
    /// * `num_simulations`: Number of Monte Carlo simulations.
    pub fn reachability_probability(
        &self,
        start: NodeId,
        target: NodeId,
        max_steps: usize,
        num_simulations: usize,
    ) -> Result<f32> {
        // Basic checks
        if self.db.get_node(start).is_err() {
            return Err(crate::core::error::Error::Storage(
                StorageError::NodeNotFound(start),
            ));
        }
        if self.db.get_node(target).is_err() {
            return Err(crate::core::error::Error::Storage(
                StorageError::NodeNotFound(target),
            ));
        }

        if start == target {
            return Ok(1.0);
        }

        let mut hits = 0;
        let mut rng = rand::thread_rng();

        for _ in 0..num_simulations {
            let mut current = start;
            let mut found = false;

            for _ in 0..max_steps {
                let edges = self.db.get_outgoing_edges(current);
                if edges.is_empty() {
                    break;
                }

                let edge_idx = rng.gen_range(0..edges.len());
                let edge_id = edges[edge_idx];

                match self.db.get_edge_target(edge_id) {
                    Ok(next) => {
                        if next == target {
                            found = true;
                            break;
                        }
                        current = next;
                    }
                    Err(_) => break,
                }
            }

            if found {
                hits += 1;
            }
        }

        Ok(hits as f32 / num_simulations as f32)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::property::PropertyMapBuilder;

    #[test]
    fn test_oracle_ppr_sink() {
        let db = AletheiaDB::new().unwrap();
        let props = PropertyMapBuilder::new().build();

        // A -> B (Sink)
        let a = db.create_node("Node", props.clone()).unwrap();
        let b = db.create_node("Node", props.clone()).unwrap();
        db.create_edge(a, b, "NEXT", props.clone()).unwrap();

        let oracle = Oracle::new(&db);

        // Run PPR from A
        // Alpha = 0.5 (50% chance to stop at each step)
        // Expected: A is visited (start), then 50% chance to visit B.
        // Normalized: A should be higher than B because A is always visited at start.
        // Visits: A (N times), B (N * 0.5 times).
        // Total: 1.5 N.
        // P(A) = 1 / 1.5 = 0.66
        // P(B) = 0.5 / 1.5 = 0.33

        let ppr = oracle.personalized_page_rank(a, 0.5, 1000, 10).unwrap();

        let score_a = *ppr.get(&a).unwrap();
        let score_b = *ppr.get(&b).unwrap();

        assert!(
            score_a > score_b,
            "Start node should have higher probability in sink graph"
        );
        assert!(
            score_a > 0.6 && score_a < 0.75,
            "P(A) should be around 0.66 (got {})",
            score_a
        );
    }

    #[test]
    fn test_oracle_reachability() {
        let db = AletheiaDB::new().unwrap();
        let props = PropertyMapBuilder::new().build();

        // A -> B -> C
        let a = db.create_node("Node", props.clone()).unwrap();
        let b = db.create_node("Node", props.clone()).unwrap();
        let c = db.create_node("Node", props.clone()).unwrap();

        db.create_edge(a, b, "NEXT", props.clone()).unwrap();
        db.create_edge(b, c, "NEXT", props.clone()).unwrap();

        let oracle = Oracle::new(&db);

        // Reach C from A in 2 steps
        // Deterministic path, so probability should be 1.0 (approximated)
        let prob = oracle.reachability_probability(a, c, 5, 100).unwrap();
        assert!(prob > 0.95, "Should reach C with high probability");

        // Reach C from A in 1 step (Impossible)
        let prob_short = oracle.reachability_probability(a, c, 1, 100).unwrap();
        assert!(prob_short < 0.01, "Should not reach C in 1 step");
    }

    #[test]
    fn test_oracle_cycle() {
        let db = AletheiaDB::new().unwrap();
        let props = PropertyMapBuilder::new().build();

        // A <-> B
        let a = db.create_node("Node", props.clone()).unwrap();
        let b = db.create_node("Node", props.clone()).unwrap();

        db.create_edge(a, b, "NEXT", props.clone()).unwrap();
        db.create_edge(b, a, "NEXT", props.clone()).unwrap();

        let oracle = Oracle::new(&db);

        // PPR from A with low alpha (long walks)
        // Should converge to 0.5 / 0.5 distribution
        let ppr = oracle.personalized_page_rank(a, 0.1, 2000, 100).unwrap();

        let score_a = *ppr.get(&a).unwrap();
        let score_b = *ppr.get(&b).unwrap();

        assert!(
            (score_a - score_b).abs() < 0.1,
            "Scores should be roughly equal in symmetric cycle (A: {}, B: {})",
            score_a,
            score_b
        );
    }
}
