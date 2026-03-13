#![allow(clippy::collapsible_if)]

//! Catalyst: Semantic Chain Reaction Detector 💥
//!
//! "Who started the fire?"
//!
//! Catalyst identifies nodes that triggered massive semantic shifts in their surrounding network.
//! It looks for nodes whose appearance or structural changes immediately precede
//! a spike in the semantic volatility (vector movement) of their neighbors.
//!
//! # Concepts
//! - **The Catalyst**: A node suspected of causing a chain reaction.
//! - **Volatility Spikes**: The difference in average vector movement speed of neighbors
//!   before and after the Catalyst's intervention.
//!
//! # Example
//! ```rust
//! // Requires features = ["nova"]
//! use aletheiadb::AletheiaDB;
//! use aletheiadb::experimental::catalyst::CatalystEngine;
//! use aletheiadb::core::id::NodeId;
//! use aletheiadb::core::temporal::Timestamp;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let db = AletheiaDB::new()?;
//! // ... setup graph ...
//! # let candidate = NodeId::new(0).unwrap();
//! # let t_event = Timestamp::from(100);
//!
//! let catalyst = CatalystEngine::new(&db);
//! let score = catalyst.calculate_catalyst_score(
//!     candidate,
//!     "embedding",
//!     t_event,
//!     50, // window_before
//!     50, // window_after
//! )?;
//!
//! println!("Catalyst Score: {}", score);
//! # Ok(())
//! # }
//! ```

use crate::AletheiaDB;
use crate::core::error::{Error, Result};
use crate::core::id::NodeId;
use crate::core::temporal::Timestamp;
use crate::core::vector::ops;
use crate::api::transaction::ReadOps;

/// The Catalyst Engine.
pub struct CatalystEngine<'a> {
    db: &'a AletheiaDB,
}

impl<'a> CatalystEngine<'a> {
    /// Create a new Catalyst Engine.
    pub fn new(db: &'a AletheiaDB) -> Self {
        Self { db }
    }

    /// Calculate the catalyst score for a node around a specific event time.
    ///
    /// The score is the difference in the average semantic volatility of the node's neighbors
    /// before and after the event. A high positive score indicates the node is a catalyst.
    ///
    /// # Arguments
    /// * `candidate` - The node suspected of being the catalyst.
    /// * `property_name` - The vector property to use for semantic analysis.
    /// * `t_event` - The timestamp of the event (e.g., node creation or connection).
    /// * `window_before` - The duration before the event to measure baseline volatility.
    /// * `window_after` - The duration after the event to measure reaction volatility.
    ///
    /// # Returns
    /// A score from -1.0 to 1.0.
    /// - `> 0.0`: Catalyst (Volatility increased after event).
    /// - `< 0.0`: Stabilizer (Volatility decreased after event).
    /// - `~ 0.0`: No significant effect.
    pub fn calculate_catalyst_score(
        &self,
        candidate: NodeId,
        property_name: &str,
        t_event: Timestamp,
        window_before: u64,
        window_after: u64,
    ) -> Result<f32> {
        let t_start = Timestamp::from(t_event.logical() as i64 - window_before as i64);
        let t_end = Timestamp::from(t_event.logical() as i64 + window_after as i64);

        // Get neighbors of the candidate at the event time.
        // We fallback to current transaction state if temporal query fails,
        // similar to other Nova modules handling indexing lag.
        let mut neighbors = Vec::new();

        if let Ok(results) = self
            .db
            .query()
            .as_of(t_event, t_event)
            .start(candidate)
            .traverse_all()
            .execute(self.db)
        {
            for r in results.flatten() {
                if let Some(neighbor) = r.entity.as_node() {
                    if neighbor.id != candidate {
                        neighbors.push(neighbor.id);
                    }
                }
            }
        }

        if neighbors.is_empty() {
            let _ = self.db.read(|tx| {
                neighbors = tx
                    .get_outgoing_edges(candidate)
                    .into_iter()
                    .filter_map(|eid| {
                        if let Ok(edge) = tx.get_edge(eid) {
                            Some(edge.target)
                        } else {
                            None
                        }
                    })
                    .collect();
                Ok::<(), Error>(())
            });
        }

        if neighbors.is_empty() {
            return Ok(0.0);
        }

        let mut total_volatility_before = 0.0;
        let mut total_volatility_after = 0.0;
        let mut valid_neighbors = 0;

        for neighbor in neighbors {
            let vec_start = self.get_vector_at(neighbor, property_name, t_start);
            let vec_event = self.get_vector_at(neighbor, property_name, t_event);
            let vec_end = self.get_vector_at(neighbor, property_name, t_end);

            if let (Some(mut v1), Some(mut v2), Some(mut v3)) = (vec_start, vec_event, vec_end) {
                ops::normalize_in_place(&mut v1);
                ops::normalize_in_place(&mut v2);
                ops::normalize_in_place(&mut v3);

                // Volatility is measured as 1.0 - cosine_similarity.
                // Distance before event.
                let sim_before = ops::cosine_similarity(&v1, &v2).unwrap_or(1.0);
                let vol_before = 1.0 - sim_before;

                // Distance after event.
                let sim_after = ops::cosine_similarity(&v2, &v3).unwrap_or(1.0);
                let vol_after = 1.0 - sim_after;

                total_volatility_before += vol_before;
                total_volatility_after += vol_after;
                valid_neighbors += 1;
            }
        }

        if valid_neighbors == 0 {
            return Ok(0.0);
        }

        let avg_vol_before = total_volatility_before / valid_neighbors as f32;
        let avg_vol_after = total_volatility_after / valid_neighbors as f32;

        let score = (avg_vol_after - avg_vol_before).clamp(-1.0, 1.0);
        Ok(score)
    }

    /// Retrieve the vector of a node at a specific time.
    fn get_vector_at(
        &self,
        node_id: NodeId,
        property_name: &str,
        time: Timestamp,
    ) -> Option<Vec<f32>> {
        let mut vector_opt = None;
        if let Ok(history) = self.db.get_node_history(node_id) {
            let mut latest_valid_version = None;
            for version in history.versions.iter() {
                if version.temporal.valid_time().start() <= time
                    && time <= version.temporal.valid_time().end()
                {
                    if version.temporal.transaction_time().start() <= time {
                        latest_valid_version = Some(version);
                    }
                }
            }
            if let Some(v) = latest_valid_version {
                vector_opt = v
                    .properties
                    .get(property_name)
                    .and_then(|p| p.as_vector())
                    .map(|v| v.to_vec());
            }
        }

        if vector_opt.is_none() {
            let _ = self.db.read(|tx| {
                if let Ok(node) = tx.get_node(node_id) {
                    vector_opt = node
                        .get_property(property_name)
                        .and_then(|p| p.as_vector())
                        .map(|v| v.to_vec());
                }
                Ok::<(), Error>(())
            });
        }
        vector_opt
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_catalyst_math() {
        // Mock vectors indicating a massive shift after an event.
        // v1 (start) and v2 (event) are identical (no volatility before).
        let mut v1 = vec![1.0, 0.0];
        let mut v2 = vec![1.0, 0.0];
        // v3 (end) is orthogonal (high volatility after).
        let mut v3 = vec![0.0, 1.0];

        ops::normalize_in_place(&mut v1);
        ops::normalize_in_place(&mut v2);
        ops::normalize_in_place(&mut v3);

        let sim_before = ops::cosine_similarity(&v1, &v2).unwrap();
        let vol_before = 1.0 - sim_before; // 1.0 - 1.0 = 0.0

        let sim_after = ops::cosine_similarity(&v2, &v3).unwrap();
        let vol_after = 1.0 - sim_after; // 1.0 - 0.0 = 1.0

        let score = (vol_after - vol_before).clamp(-1.0, 1.0); // 1.0 - 0.0 = 1.0

        assert_eq!(score, 1.0, "Expected a high catalyst score");
    }

    #[test]
    fn test_stabilizer_math() {
        // Mock vectors indicating stabilization after an event.
        // v1 and v2 are orthogonal (high volatility before).
        let mut v1 = vec![1.0, 0.0];
        let mut v2 = vec![0.0, 1.0];
        // v2 and v3 are identical (no volatility after).
        let mut v3 = vec![0.0, 1.0];

        ops::normalize_in_place(&mut v1);
        ops::normalize_in_place(&mut v2);
        ops::normalize_in_place(&mut v3);

        let sim_before = ops::cosine_similarity(&v1, &v2).unwrap();
        let vol_before = 1.0 - sim_before; // 1.0 - 0.0 = 1.0

        let sim_after = ops::cosine_similarity(&v2, &v3).unwrap();
        let vol_after = 1.0 - sim_after; // 1.0 - 1.0 = 0.0

        let score = (vol_after - vol_before).clamp(-1.0, 1.0); // 0.0 - 1.0 = -1.0

        assert_eq!(score, -1.0, "Expected a negative score (stabilizer)");
    }
}
