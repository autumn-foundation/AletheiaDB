//! Solstice: Semantic Cyclicality Detector 🌞🌛
//!
//! "Does history repeat itself?"
//!
//! Solstice analyzes a node's temporal journey through vector space
//! and determines if its semantic evolution is linear, stagnant, or cyclical.
//!
//! # Concepts
//! - **Cyclicality Score**: A measure of how frequently a node returns to previously
//!   visited regions of the vector space after having left them.
//!   - High score (~1.0) = Oscillating between distinct states (e.g., Work / Home).
//!   - Low score (~0.0) = Purely linear progression or complete stagnation.
//! - **Semantic Loop**: A pattern where a node moves away from state A to state B,
//!   and later returns close to state A.
//!
//! # Use Cases
//! - **Behavioral Analysis**: Identifying users who oscillate between periods of high
//!   engagement and churn risk.
//! - **Market Cycles**: Detecting cyclical economic shifts in financial entities.
//! - **Narrative Arcs**: Finding characters who regress to earlier stages of their arc.
//!
//! # Example
//! ```rust
//! // Requires features = ["nova"]
//! use aletheiadb::AletheiaDB;
//! use aletheiadb::experimental::solstice::SolsticeEngine;
//! use aletheiadb::core::id::NodeId;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let db = AletheiaDB::new()?;
//! // ... setup node with history ...
//! # let node_id = NodeId::new(1).unwrap();
//!
//! let solstice = SolsticeEngine::new(&db);
//! let score = solstice.analyze(node_id, "embedding")?;
//!
//! println!("Cyclicality Score: {:.2}", score.cyclicality_score);
//! # Ok(())
//! # }
//! ```

#![allow(clippy::collapsible_if)]

use crate::AletheiaDB;
use crate::core::error::Result;
use crate::core::id::NodeId;
use crate::core::vector::ops;

/// The result of a Solstice analysis.
#[derive(Debug, Clone)]
pub struct CyclicalityResult {
    /// Score between 0.0 (no cycles) and 1.0 (highly cyclical).
    pub cyclicality_score: f32,
    /// The number of distinct semantic "returns" or loops detected.
    pub loop_count: usize,
    /// The total path distance traveled by the node.
    pub total_distance: f32,
    /// The displacement distance (start to end).
    pub displacement: f32,
}

/// The Solstice Engine for Semantic Cyclicality Detection.
pub struct SolsticeEngine<'a> {
    db: &'a AletheiaDB,
}

impl<'a> SolsticeEngine<'a> {
    /// Create a new SolsticeEngine instance.
    pub fn new(db: &'a AletheiaDB) -> Self {
        Self { db }
    }

    /// Analyze a node's history for cyclical patterns.
    pub fn analyze(&self, node_id: NodeId, property_name: &str) -> Result<CyclicalityResult> {
        // Fetch full history directly.
        let history = self.db.get_node_history(node_id)?;

        // Extract ordered sequence of vectors from the node's history.
        // History is typically sorted by version ID. We want to sort chronologically by valid_time.start.
        let mut temporal_states = Vec::new();

        for version in &history.versions {
            if let Some(prop) = version.properties.get(property_name) {
                if let Some(vec) = prop.as_vector() {
                    let ts = version.temporal.valid_time().start().wallclock();
                    temporal_states.push((ts, vec.to_vec()));
                }
            }
        }

        if temporal_states.len() < 3 {
            // Need at least 3 points to form a meaningful cycle (A -> B -> A)
            return Ok(CyclicalityResult {
                cyclicality_score: 0.0,
                loop_count: 0,
                total_distance: 0.0,
                displacement: 0.0,
            });
        }

        // Ensure chronological order
        temporal_states.sort_by_key(|(ts, _)| *ts);

        let vectors: Vec<Vec<f32>> = temporal_states.into_iter().map(|(_, v)| v).collect();

        self.compute_cyclicality(&vectors)
    }

    /// Internal logic for computing cyclicality metrics.
    fn compute_cyclicality(&self, path: &[Vec<f32>]) -> Result<CyclicalityResult> {
        let mut total_distance = 0.0;
        let n = path.len();

        // 1. Compute total path length
        for i in 0..(n - 1) {
            total_distance += ops::euclidean_distance(&path[i], &path[i + 1])?;
        }

        // 2. Compute net displacement (start to end)
        let displacement = ops::euclidean_distance(&path[0], &path[n - 1])?;

        // 3. Detect Cycles (A -> B -> A pattern)
        // We look for points where the node moves far away, then comes back close to a previous point.
        // Heuristic:
        // For each pair of points i, j (j > i + 1):
        // If distance(i, j) is small compared to the path length between i and j, it's a cycle.

        // Tolerance for returning to a state. Let's say, 20% of the total distance traveled so far.
        // Better yet: If eucl_dist(i, j) < 0.25 * path_length(i to j), we count it as a loop.

        let mut loops_found = 0;
        let return_threshold_ratio = 0.25;

        // Iterate over potential starts of a cycle
        let mut i = 0;
        while i < n - 2 {
            // Traverse forward to find a significant departure
            let mut j = i + 1;
            let mut path_dist = ops::euclidean_distance(&path[i], &path[j])?;

            // Move forward until we depart significantly from i
            // We need a loop to actually leave the area before coming back.
            // Let's define "leaving" as traveling at least some absolute threshold.
            // We'll use the average step size.
            let avg_step = if n > 1 {
                total_distance / ((n - 1) as f32)
            } else {
                0.0
            };
            let departure_threshold = avg_step * 0.5; // Lower threshold to trigger leaves

            while j < n - 1 && path_dist < departure_threshold {
                path_dist += ops::euclidean_distance(&path[j], &path[j + 1])?;
                j += 1;
            }

            if j == n - 1 && path_dist < departure_threshold {
                i += 1;
                continue; // Never really left state i, just stagnant. Try next anchor.
            }

            // Now we have left state i. Let's see if we ever return.
            let mut k = j + 1;
            let mut found_loop = false;
            while k < n {
                path_dist += ops::euclidean_distance(&path[k - 1], &path[k])?;
                let return_dist = ops::euclidean_distance(&path[i], &path[k])?;

                // If the return distance is very small relative to the wandering path length, it's a cycle!
                if return_dist < path_dist * return_threshold_ratio {
                    loops_found += 1;
                    found_loop = true;
                    // Jump i to the return point to start the next cycle search
                    i = k;
                    break;
                }
                k += 1;
            }

            if !found_loop {
                i += 1; // Try the next point as a starting anchor
            }
        }

        let loop_count = loops_found;

        // 4. Calculate Cyclicality Score
        // A pure cycle has displacement near 0 and high total_distance.
        // A linear path has displacement == total_distance.
        // A stagnant path has total_distance near 0.

        let score = if total_distance < 1e-5 {
            0.0 // Stagnant, no cycles.
        } else {
            // Tortuosity factor: how "twisty" is the path? 1.0 = straight line, high = very twisty.
            // Cyclicality is high when tortuosity is high AND loop_count > 0.

            // Let's use a simpler heuristic for the score:
            // High if loop_count > 0 and displacement is small relative to total_distance.
            // Max score is 1.0.

            let linearity = displacement / total_distance; // 1.0 is a straight line, 0.0 is a closed loop.
            let wandering_score = 1.0 - linearity; // 0.0 is straight, 1.0 is highly wandering/closed.

            // To be truly cyclical, we need at least one detected loop.
            if loop_count > 0 {
                // Boost the score based on loops.
                // 1 loop with high wandering is cyclical.
                wandering_score.min(1.0)
            } else {
                // It wandered but didn't close a loop. (e.g., drunkard's walk but didn't return home)
                wandering_score * 0.5
            }
        };

        Ok(CyclicalityResult {
            cyclicality_score: score,
            loop_count,
            total_distance,
            displacement,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::transaction::WriteOps;
    use crate::core::property::PropertyMapBuilder;
    use std::time::Duration;

    #[test]
    fn test_solstice_cyclical_pattern() {
        let db = AletheiaDB::new().unwrap();

        // Simulate a node moving: Origin -> Right -> Origin -> Right -> Origin
        let node = db
            .create_node(
                "User",
                PropertyMapBuilder::new()
                    .insert_vector("vec", &[0.0, 0.0])
                    .build(),
            )
            .unwrap();

        let path = vec![
            vec![10.0, 0.0], // Leave (Right)
            vec![0.1, 0.1],  // Return ~Origin
            vec![10.0, 0.0], // Leave (Right)
            vec![-0.1, 0.0], // Return ~Origin
        ];

        for pos in path {
            std::thread::sleep(Duration::from_millis(10));
            db.write(|tx| {
                tx.update_node(
                    node,
                    PropertyMapBuilder::new().insert_vector("vec", &pos).build(),
                )
            })
            .unwrap();
        }

        let solstice = SolsticeEngine::new(&db);
        let result = solstice.analyze(node, "vec").unwrap();

        println!("Cyclical Result: {:?}", result);

        assert!(result.loop_count >= 2);
        assert!(result.cyclicality_score > 0.8);
        assert!(result.displacement < 1.0); // Ends up very close to start
        assert!(result.total_distance > 30.0); // Traveled ~40 units
    }

    #[test]
    fn test_solstice_linear_pattern() {
        let db = AletheiaDB::new().unwrap();

        // Simulate a node moving linearly: Origin -> 10,0 -> 20,0 -> 30,0
        let node = db
            .create_node(
                "User",
                PropertyMapBuilder::new()
                    .insert_vector("vec", &[0.0, 0.0])
                    .build(),
            )
            .unwrap();

        let path = vec![vec![10.0, 0.0], vec![20.0, 0.0], vec![30.0, 0.0]];

        for pos in path {
            std::thread::sleep(Duration::from_millis(10));
            db.write(|tx| {
                tx.update_node(
                    node,
                    PropertyMapBuilder::new().insert_vector("vec", &pos).build(),
                )
            })
            .unwrap();
        }

        let solstice = SolsticeEngine::new(&db);
        let result = solstice.analyze(node, "vec").unwrap();

        println!("Linear Result: {:?}", result);

        assert_eq!(result.loop_count, 0);
        assert!(result.cyclicality_score < 0.1); // Near 0 for linear
        assert!((result.displacement - result.total_distance).abs() < 0.01); // Disp == Dist
    }

    #[test]
    fn test_solstice_stagnant_pattern() {
        let db = AletheiaDB::new().unwrap();

        // Node stays put
        let node = db
            .create_node(
                "User",
                PropertyMapBuilder::new()
                    .insert_vector("vec", &[5.0, 5.0])
                    .build(),
            )
            .unwrap();

        let path = vec![vec![5.0, 5.0], vec![5.0, 5.0], vec![5.0, 5.0]];

        for pos in path {
            std::thread::sleep(Duration::from_millis(10));
            db.write(|tx| {
                tx.update_node(
                    node,
                    PropertyMapBuilder::new().insert_vector("vec", &pos).build(),
                )
            })
            .unwrap();
        }

        let solstice = SolsticeEngine::new(&db);
        let result = solstice.analyze(node, "vec").unwrap();

        println!("Stagnant Result: {:?}", result);

        assert_eq!(result.loop_count, 0);
        assert_eq!(result.cyclicality_score, 0.0);
        assert_eq!(result.total_distance, 0.0);
    }
}
