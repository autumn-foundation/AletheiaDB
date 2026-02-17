//! Mnemosyne: Memory Retention & Decay Engine.
//!
//! "Not all memories are created equal."
//!
//! Mnemosyne calculates a retention score for nodes based on:
//! 1. Recency (Time Decay)
//! 2. Connectivity (Degree Centrality)
//!
//! # Concepts
//! - **Retention Score**: A value from 0.0 to infinity. Higher means "keep".
//! - **Decay**: Score decreases exponentially with age.
//! - **Reinforcement**: Score increases linearly with connectivity (in-degree + out-degree).
//!
//! # Example
//! ```rust,no_run
//! use aletheiadb::AletheiaDB;
//! use aletheiadb::experimental::mnemosyne::Mnemosyne;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let db = AletheiaDB::new()?;
//! let mnemosyne = Mnemosyne::new(&db);
//!
//! // Find nodes that are "fading"
//! let forgotten = mnemosyne.forgetting_candidates(0.1)?;
//! for (node_id, score) in forgotten {
//!     println!("Node {} is fading (score: {:.4})", node_id, score);
//! }
//! # Ok(())
//! # }
//! ```

use crate::AletheiaDB;
use crate::core::id::NodeId;
use crate::core::temporal::time;
use crate::utils::Result;

/// The Mnemosyne engine.
pub struct Mnemosyne<'a> {
    db: &'a AletheiaDB,
    /// Decay half-life in seconds (default: 7 days)
    half_life_secs: f32,
}

impl<'a> Mnemosyne<'a> {
    /// Create a new Mnemosyne instance with default settings.
    pub fn new(db: &'a AletheiaDB) -> Self {
        Self {
            db,
            half_life_secs: 7.0 * 24.0 * 3600.0, // 7 days
        }
    }

    /// Set a custom half-life for decay.
    pub fn with_half_life_days(mut self, days: f32) -> Self {
        self.half_life_secs = days * 24.0 * 3600.0;
        self
    }

    /// Calculate the retention score for a node.
    ///
    /// Formula: `(1.0 + Connectivity) * 2^(-Age / HalfLife)`
    pub fn score(&self, node_id: NodeId) -> Result<f32> {
        // 1. Connectivity (Reinforcement)
        // In-degree is a strong signal of importance. Out-degree is moderate.
        let in_degree = self.db.in_degree(node_id) as f32;
        let out_degree = self.db.out_degree(node_id) as f32;

        // Base importance is 1.0 (existence).
        // Each incoming edge adds 1.0 importance.
        // Each outgoing edge adds 0.5 importance.
        let connectivity_score = 1.0 + in_degree + (out_degree * 0.5);

        // 2. Recency (Decay)
        // Get the latest version's timestamp
        let history = self.db.get_node_history(node_id)?;
        let last_update_time = history
            .current_version()
            .map(|v| v.temporal.transaction_time().start()) // When was it recorded?
            .unwrap_or_else(time::now); // Fallback to now (shouldn't happen for existing nodes)

        let age_micros = time::now()
            .wallclock()
            .saturating_sub(last_update_time.wallclock());
        let age_secs = age_micros as f32 / 1_000_000.0;

        // Exponential decay: N(t) = N0 * (1/2)^(t / half_life)
        // We use base 2 for "half-life" intuition.
        let decay_factor = 2.0_f32.powf(-age_secs / self.half_life_secs);

        Ok(connectivity_score * decay_factor)
    }

    /// Identify candidates for "forgetting" (pruning).
    ///
    /// Scans all nodes and returns those with a retention score below the threshold.
    ///
    /// # Arguments
    /// * `threshold` - Score below which a node is considered "fading".
    pub fn forgetting_candidates(&self, threshold: f32) -> Result<Vec<(NodeId, f32)>> {
        let mut candidates = Vec::new();

        // Note: In a real production system, scanning all nodes is expensive.
        // We would likely use a specialized index or sampling.
        // For this experimental feature, we iterate all IDs.

        // Access internal storage via `db.current` (pub(crate))
        let all_ids = self.db.current.get_all_node_ids();

        for id in all_ids {
            let s = self.score(id)?;
            if s < threshold {
                candidates.push((id, s));
            }
        }

        // Sort by score (lowest first - most likely to be forgotten)
        candidates.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));

        Ok(candidates)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::property::PropertyMapBuilder;

    #[test]
    fn test_mnemosyne_decay() {
        let db = AletheiaDB::new().unwrap();
        // Half-life = 0.05 seconds (50ms)
        let half_life_days = 0.05 / 86400.0;
        let mnemosyne = Mnemosyne::new(&db).with_half_life_days(half_life_days);

        let props = PropertyMapBuilder::new().insert("name", "Test").build();
        let node = db.create_node("Node", props).unwrap();

        // Fresh node (Age ~0)
        let score_fresh = mnemosyne.score(node).unwrap();
        assert!(score_fresh > 0.9);

        // Sleep 100ms (2 half-lives)
        std::thread::sleep(std::time::Duration::from_millis(100));

        let score_old = mnemosyne.score(node).unwrap();

        // Should decay to approx 0.25 (1.0 * 0.5 * 0.5)
        assert!(
            score_old < score_fresh * 0.6,
            "Score should decay significantly (fresh: {}, old: {})",
            score_fresh,
            score_old
        );
    }

    #[test]
    fn test_mnemosyne_connectivity_boost() {
        let db = AletheiaDB::new().unwrap();
        let mnemosyne = Mnemosyne::new(&db);
        let props = PropertyMapBuilder::new().build();

        // Node A: Isolated
        let a = db.create_node("Node", props.clone()).unwrap();

        // Node B: Connected (1 incoming, 1 outgoing)
        let b = db.create_node("Node", props.clone()).unwrap();
        let c = db.create_node("Node", props.clone()).unwrap();

        db.create_edge(c, b, "POINTS_TO", props.clone()).unwrap(); // C -> B (Incoming to B)
        db.create_edge(b, a, "POINTS_TO", props.clone()).unwrap(); // B -> A (Outgoing from B)

        let score_a = mnemosyne.score(a).unwrap();
        let score_b = mnemosyne.score(b).unwrap();

        // A has 1 incoming (from B), 0 outgoing. Connectivity = 1.0 + 1.0 + 0 = 2.0
        // B has 1 incoming (from C), 1 outgoing (to A). Connectivity = 1.0 + 1.0 + 0.5 = 2.5
        // C has 0 incoming, 1 outgoing. Connectivity = 1.0 + 0 + 0.5 = 1.5

        // Since all created instantly, decay is negligible.

        assert!(
            score_b > score_a,
            "B (2.5) should score higher than A (2.0)"
        );

        // Verify rough values
        assert!(score_a > 1.9 && score_a < 2.1);
        assert!(score_b > 2.4 && score_b < 2.6);
    }

    #[test]
    fn test_forgetting_candidates() {
        let db = AletheiaDB::new().unwrap();
        let mnemosyne = Mnemosyne::new(&db);
        let props = PropertyMapBuilder::new().build();

        // Create 3 isolated nodes (Score ~1.0)
        let _n1 = db.create_node("Node", props.clone()).unwrap();
        let _n2 = db.create_node("Node", props.clone()).unwrap();
        let _n3 = db.create_node("Node", props.clone()).unwrap();

        // Threshold 0.5: Should return none (all are ~1.0)
        let candidates_low = mnemosyne.forgetting_candidates(0.5).unwrap();
        assert!(candidates_low.is_empty());

        // Threshold 1.5: Should return all (all are ~1.0)
        let candidates_high = mnemosyne.forgetting_candidates(1.5).unwrap();
        assert_eq!(candidates_high.len(), 3);
    }
}
