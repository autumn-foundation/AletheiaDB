#![allow(clippy::collapsible_if)]

//! Aura: Semantic Essence over Time 🌟
//!
//! "Who are you, really?"
//!
//! The Aura Engine calculates the "Aura" of a node—an exponentially time-weighted
//! average of its semantic vector over its entire history. While a node's *current*
//! vector represents what it is thinking or doing *right now*, its Aura represents
//! its long-term identity or essence.
//!
//! # Concepts
//! - **Aura Vector**: The historical, time-weighted average vector. Recent states
//!   have more weight, but deep history still exerts a gravitational pull.
//! - **Divergence**: The distance between a node's Aura and its current vector.
//!   A high divergence means the node is acting "out of character".
//!
//! # Use Cases
//! - **Identity Hijack Detection**: An LLM agent suddenly starts outputting vectors
//!   that are highly divergent from its established Aura.
//! - **Core Concept Extraction**: Distilling a concept down to its most stable,
//!   enduring semantic meaning, filtering out recent noise.
//!
//! # Example
//! ```rust
//! // Requires features = ["nova"]
//! use aletheiadb::AletheiaDB;
//! use aletheiadb::experimental::aura::AuraEngine;
//! use aletheiadb::core::id::NodeId;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let db = AletheiaDB::new()?;
//! // ... setup temporal graph ...
//! # let subject_node = NodeId::new(0).unwrap();
//!
//! let engine = AuraEngine::new(&db);
//! // Calculate the Aura using a half-life of 7 days
//! let half_life_us = 7 * 24 * 3600 * 1_000_000;
//! let result = engine.calculate_aura(subject_node, "embedding", half_life_us)?;
//!
//! if let Some(aura) = result.aura_vector {
//!     println!("Aura Divergence: {}", result.divergence_score);
//! }
//! # Ok(())
//! # }
//! ```

use crate::AletheiaDB;
use crate::core::error::Result;
use crate::core::id::NodeId;
use crate::core::temporal::time;
use crate::core::vector::ops;

/// The result of an Aura calculation.
#[derive(Debug, Clone)]
pub struct AuraResult {
    /// The calculated Aura vector. None if the node has no history for the property.
    pub aura_vector: Option<Vec<f32>>,
    /// The current vector of the node. None if it doesn't currently exist.
    pub current_vector: Option<Vec<f32>>,
    /// The cosine distance (1.0 - similarity) between the Aura and the current vector.
    /// Range [0.0, 2.0]. 0.0 means perfectly aligned.
    pub divergence_score: f32,
}

/// The Aura Engine.
pub struct AuraEngine<'a> {
    db: &'a AletheiaDB,
}

impl<'a> AuraEngine<'a> {
    /// Create a new Aura Engine.
    pub fn new(db: &'a AletheiaDB) -> Self {
        Self { db }
    }

    /// Calculate the Aura vector and its divergence from the current state.
    ///
    /// # Arguments
    /// * `node_id` - The node to analyze.
    /// * `property_name` - The vector property to use for semantic analysis.
    /// * `half_life_us` - The half-life for the exponential decay in microseconds.
    ///   Older versions decay in weight. A larger half-life gives
    ///   more weight to distant history.
    pub fn calculate_aura(
        &self,
        node_id: NodeId,
        property_name: &str,
        half_life_us: i64,
    ) -> Result<AuraResult> {
        let history = self.db.get_node_history(node_id)?;

        if history.versions.is_empty() {
            return Ok(AuraResult {
                aura_vector: None,
                current_vector: None,
                divergence_score: 0.0,
            });
        }

        let now = time::now().wallclock();
        let mut weighted_sum: Option<Vec<f32>> = None;
        let mut total_weight = 0.0;
        let mut current_vector: Option<Vec<f32>> = None;

        // Iterate through history to calculate the weighted average
        for version in history.versions.iter() {
            if let Some(prop) = version.properties.get(property_name) {
                if let Some(vec) = prop.as_vector() {
                    let ts = version.temporal.valid_time().start().wallclock();

                    // We treat the "current" state as the latest one encountered
                    current_vector = Some(vec.to_vec());

                    // Calculate weight based on exponential decay
                    // age = now - ts. If ts > now (future?), weight is 1.0 (or higher, but clamped).
                    let age_us = (now - ts).max(0);

                    // Weight formula: W = 0.5 ^ (age / half_life)
                    let weight = if half_life_us > 0 {
                        0.5_f64.powf(age_us as f64 / half_life_us as f64) as f32
                    } else {
                        // If half_life is 0, only the most recent (age=0) matters conceptually,
                        // but let's just treat all as equal weight or fallback to 1.0 to avoid div by zero.
                        1.0_f32
                    };

                    total_weight += weight;

                    match &mut weighted_sum {
                        Some(sum) => {
                            // Ensure dimensions match before adding
                            if sum.len() == vec.len() {
                                for i in 0..sum.len() {
                                    sum[i] += vec[i] * weight;
                                }
                            }
                        }
                        None => {
                            let mut initial_sum = vec.to_vec();
                            for v in &mut initial_sum {
                                *v *= weight;
                            }
                            weighted_sum = Some(initial_sum);
                        }
                    }
                }
            }
        }

        let mut aura_vector = None;
        let mut divergence_score = 0.0;

        if let Some(mut sum) = weighted_sum {
            if total_weight > 0.0 {
                for v in &mut sum {
                    *v /= total_weight;
                }

                // Normalize the Aura vector for proper cosine comparison
                ops::normalize_in_place(&mut sum);

                if let Some(current) = &current_vector {
                    let mut curr_normalized = current.clone();
                    ops::normalize_in_place(&mut curr_normalized);

                    let sim = ops::cosine_similarity(&sum, &curr_normalized)?;
                    // Divergence is distance: 1.0 - similarity
                    divergence_score = (1.0 - sim).max(0.0);
                }

                aura_vector = Some(sum);
            }
        }

        Ok(AuraResult {
            aura_vector,
            current_vector,
            divergence_score,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::transaction::WriteOps;
    use crate::core::property::PropertyMapBuilder;

    #[test]
    fn test_aura_stable() {
        let db = AletheiaDB::new().unwrap();

        // Give some initial buffer time so tx time clearly separated from epoch 0
        std::thread::sleep(std::time::Duration::from_millis(10));

        let mut n1 = crate::core::id::NodeId::new(0).unwrap();

        // State 1: A stable node staying near [1.0, 0.0]
        db.write(|tx| {
            n1 = tx
                .create_node(
                    "Concept",
                    PropertyMapBuilder::new()
                        .insert_vector("vec", &[1.0, 0.0])
                        .build(),
                )
                .unwrap();
            Ok::<(), crate::core::error::Error>(())
        })
        .unwrap();

        std::thread::sleep(std::time::Duration::from_millis(20));

        // State 2: Still near [1.0, 0.0]
        db.write(|tx| {
            tx.update_node(
                n1,
                PropertyMapBuilder::new()
                    .insert_vector("vec", &[0.99, 0.01])
                    .build(),
            )
            .unwrap();
            Ok::<(), crate::core::error::Error>(())
        })
        .unwrap();

        std::thread::sleep(std::time::Duration::from_millis(20));

        let engine = AuraEngine::new(&db);
        // Half-life doesn't matter much here since states are very similar
        let result = engine.calculate_aura(n1, "vec", 1_000_000).unwrap();

        // It should have a vector
        assert!(result.aura_vector.is_some());

        // Divergence should be very low since the current state matches the historical aura
        assert!(result.divergence_score < 0.05);
    }

    #[test]
    fn test_aura_divergent() {
        let db = AletheiaDB::new().unwrap();

        std::thread::sleep(std::time::Duration::from_millis(10));

        let mut n1 = crate::core::id::NodeId::new(0).unwrap();

        // State 1: Established Aura at [1.0, 0.0]
        db.write(|tx| {
            n1 = tx
                .create_node(
                    "Concept",
                    PropertyMapBuilder::new()
                        .insert_vector("vec", &[1.0, 0.0])
                        .build(),
                )
                .unwrap();
            Ok::<(), crate::core::error::Error>(())
        })
        .unwrap();

        // Let some time pass so the first state gets decent weight
        std::thread::sleep(std::time::Duration::from_millis(100));

        // State 2: Sudden shift to [0.0, 1.0]
        db.write(|tx| {
            tx.update_node(
                n1,
                PropertyMapBuilder::new()
                    .insert_vector("vec", &[0.0, 1.0])
                    .build(),
            )
            .unwrap();
            Ok::<(), crate::core::error::Error>(())
        })
        .unwrap();

        let engine = AuraEngine::new(&db);

        // Use a long half life so the historical [1.0, 0.0] state still exerts heavy influence
        let result = engine.calculate_aura(n1, "vec", 1_000_000).unwrap();

        // The Aura vector should be somewhere between [1.0, 0.0] and [0.0, 1.0].
        // But the *current* vector is purely [0.0, 1.0].
        // This means there should be significant divergence!
        assert!(
            result.divergence_score > 0.1,
            "Expected significant divergence, got {}",
            result.divergence_score
        );
    }
}
