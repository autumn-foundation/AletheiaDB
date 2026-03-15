//! Ouroboros: Semantic Loop & Regression Detection.
//!
//! "Are we repeating history?"
//!
//! Ouroboros analyzes a node's temporal trajectory to find semantic loops:
//! instances where a node's vector embedding drifted away over time,
//! but eventually returned close to a previous historical state.
//!
//! # Concepts
//! - **Return**: When a node's current or later state is semantically similar to an older state.
//! - **Divergence**: The maximum distance the node traveled *away* from the older state before returning.
//! - **Loop**: A sequence where the node diverged significantly, then returned.
//!
//! # Example
//! ```rust,no_run
//! use aletheiadb::AletheiaDB;
//! use aletheiadb::experimental::ouroboros::Ouroboros;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let db = AletheiaDB::new()?;
//! # let node_id = db.create_node("N", Default::default())?;
//! let ouroboros = Ouroboros::new(&db);
//!
//! // Find loops where the node returned within distance 1.0,
//! // after having diverged by at least 5.0.
//! let loops = ouroboros.detect_loops(node_id, "embedding", 1.0, 5.0)?;
//!
//! for loop_event in loops {
//!     println!("History repeated! Diverged by {:.2}, then returned with diff {:.2}",
//!              loop_event.max_divergence, loop_event.return_distance);
//! }
//! # Ok(())
//! # }
//! ```

use crate::AletheiaDB;
use crate::core::error::Result;
use crate::core::id::NodeId;
use crate::core::temporal::Timestamp;
use crate::core::vector::ops::euclidean_distance;

/// A detected semantic loop in a node's history.
#[derive(Debug, Clone)]
pub struct SemanticLoop {
    /// The timestamp when the loop started (the original state).
    pub start_time: Timestamp,
    /// The timestamp when the node returned to a similar state.
    pub end_time: Timestamp,
    /// The semantic distance between the start state and end state (should be low).
    pub return_distance: f32,
    /// The maximum semantic distance reached from the start state during the loop (should be high).
    pub max_divergence: f32,
    /// Number of version updates that occurred within this loop.
    pub intermediate_steps: usize,
}

/// The Ouroboros engine.
pub struct Ouroboros<'a> {
    db: &'a AletheiaDB,
}

impl<'a> Ouroboros<'a> {
    /// Create a new Ouroboros instance.
    pub fn new(db: &'a AletheiaDB) -> Self {
        Self { db }
    }

    /// Detect semantic loops for a specific node and vector property.
    ///
    /// # Arguments
    /// * `node_id` - The node to analyze.
    /// * `property` - The name of the vector property.
    /// * `return_threshold` - Maximum distance to be considered a "return" to the original state.
    /// * `min_divergence` - Minimum distance the node must have traveled away to count as a loop (filters out noise/stagnation).
    pub fn detect_loops(
        &self,
        node_id: NodeId,
        property: &str,
        return_threshold: f32,
        min_divergence: f32,
    ) -> Result<Vec<SemanticLoop>> {
        let history = self.db.get_node_history(node_id)?;

        let mut versions = Vec::new();
        for version in history.versions {
            if let Some(vec) = version.properties.get(property).and_then(|v| v.as_vector()) {
                versions.push((version.temporal.valid_time().start(), vec.to_vec()));
            }
        }

        // Sort chronologically
        versions.sort_by_key(|(t, _)| t.wallclock());

        let mut loops = Vec::new();

        // O(N^2) comparison of all pairs of states (N is usually small for a single node's history)
        for i in 0..versions.len() {
            let (start_t, start_vec) = &versions[i];

            for j in (i + 2)..versions.len() {
                // +2 ensures at least one intermediate step
                let (end_t, end_vec) = &versions[j];

                let ret_dist = euclidean_distance(start_vec, end_vec)?;

                if ret_dist <= return_threshold {
                    // It returned! Now check if it actually diverged in between.
                    let mut max_div = 0.0_f32;
                    for (_, mid_vec) in versions.iter().take(j).skip(i + 1) {
                        let div = euclidean_distance(start_vec, mid_vec)?;
                        if div > max_div {
                            max_div = div;
                        }
                    }

                    if max_div >= min_divergence {
                        loops.push(SemanticLoop {
                            start_time: *start_t,
                            end_time: *end_t,
                            return_distance: ret_dist,
                            max_divergence: max_div,
                            intermediate_steps: j - i - 1,
                        });

                        // Break to find the earliest/shortest return? Or keep finding more?
                        // Let's just find all instances.
                    }
                }
            }
        }

        Ok(loops)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::transaction::WriteOps;
    use crate::core::property::PropertyMapBuilder;

    #[test]
    fn test_ouroboros_detects_loop() {
        let db = AletheiaDB::new().unwrap();

        let props = PropertyMapBuilder::new()
            .insert_vector("vec", &[0.0, 0.0]) // Start at origin
            .build();
        let node = db.create_node("Point", props).unwrap();

        // Step 1: Move far away
        std::thread::sleep(std::time::Duration::from_millis(10));
        db.write(|tx| {
            tx.update_node(
                node,
                PropertyMapBuilder::new()
                    .insert_vector("vec", &[10.0, 0.0])
                    .build(),
            )
        })
        .unwrap();

        // Step 2: Move further
        std::thread::sleep(std::time::Duration::from_millis(10));
        db.write(|tx| {
            tx.update_node(
                node,
                PropertyMapBuilder::new()
                    .insert_vector("vec", &[10.0, 10.0])
                    .build(),
            )
        })
        .unwrap();

        // Step 3: Return to origin
        std::thread::sleep(std::time::Duration::from_millis(10));
        db.write(|tx| {
            tx.update_node(
                node,
                PropertyMapBuilder::new()
                    .insert_vector("vec", &[0.1, 0.1]) // Almost back
                    .build(),
            )
        })
        .unwrap();

        let ouroboros = Ouroboros::new(&db);
        let loops = ouroboros.detect_loops(node, "vec", 0.5, 5.0).unwrap();

        assert_eq!(
            loops.len(),
            1,
            "Should detect exactly one loop starting from origin"
        );
        let l = &loops[0];

        assert!(l.return_distance < 0.5);
        assert!(l.max_divergence >= 10.0);
        assert_eq!(l.intermediate_steps, 2);
    }

    #[test]
    fn test_ouroboros_ignores_stagnation() {
        let db = AletheiaDB::new().unwrap();

        let props = PropertyMapBuilder::new()
            .insert_vector("vec", &[0.0, 0.0])
            .build();
        let node = db.create_node("Point", props).unwrap();

        // Small movements, never diverging enough
        for _ in 0..3 {
            std::thread::sleep(std::time::Duration::from_millis(10));
            db.write(|tx| {
                tx.update_node(
                    node,
                    PropertyMapBuilder::new()
                        .insert_vector("vec", &[0.1, 0.1])
                        .build(),
                )
            })
            .unwrap();
        }

        let ouroboros = Ouroboros::new(&db);
        // Requires divergence of 5.0, which never happens
        let loops = ouroboros.detect_loops(node, "vec", 0.5, 5.0).unwrap();

        assert!(
            loops.is_empty(),
            "Should not detect loops if divergence is too small"
        );
    }
}
