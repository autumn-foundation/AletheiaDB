//! Pendulum: Semantic Oscillation Detector.
//!
//! "Are we going in circles?"
//!
//! Pendulum analyzes the historical semantic trajectory of a node to detect
//! oscillation—when a node repeatedly moves back and forth between distinct
//! semantic states (poles).
//!
//! # Concepts
//! - **Poles**: The two most semantically distant states in the node's history.
//! - **Projection**: Projecting each historical state onto the axis between the poles.
//! - **Swings**: The number of times the node's semantic state crossed the midpoint between poles.
//!
//! # Use Cases
//! - **Agent Loops**: Detecting if an LLM agent is stuck flipping between two reasoning states.
//! - **Indecision**: Finding users oscillating between contrasting preferences.
//! - **Cyclic Trends**: Identifying concepts that undergo cyclical popularity.
//!
//! # Example
//! ```rust,no_run
//! use aletheiadb::AletheiaDB;
//! use aletheiadb::experimental::pendulum::Pendulum;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let db = AletheiaDB::new()?;
//! let pendulum = Pendulum::new(&db);
//!
//! # let node_id = db.create_node("Node", Default::default())?;
//! // Analyze oscillation on the "embedding" property
//! if let Some(oscillation) = pendulum.analyze(node_id, "embedding")? {
//!     println!("Detected {} swings between poles.", oscillation.swings);
//!     println!("Distance between poles: {:.4}", oscillation.pole_distance);
//! }
//! # Ok(())
//! # }
//! ```

use crate::AletheiaDB;
use crate::core::error::Result;
use crate::core::id::NodeId;

/// The result of a Pendulum analysis.
#[derive(Debug, Clone)]
pub struct Oscillation {
    /// Number of times the semantic state crossed the midpoint between poles.
    pub swings: usize,
    /// The Euclidean distance between the two most extreme semantic states (poles).
    pub pole_distance: f32,
    /// Whether the node exhibits significant oscillation (>0 swings and notable distance).
    pub is_oscillating: bool,
}

#[cfg(feature = "nova")]
/// The Pendulum Engine.
pub struct Pendulum<'a> {
    db: &'a AletheiaDB,
}

#[cfg(not(feature = "nova"))]
/// The Pendulum Engine.
#[deprecated(
    note = "Pendulum requires the 'nova' feature. Add 'features = [\"nova\"]' to your Cargo.toml."
)]
pub struct Pendulum<'a> {
    _marker: std::marker::PhantomData<&'a AletheiaDB>,
}

#[cfg(feature = "nova")]
impl<'a> Pendulum<'a> {
    /// Create a new Pendulum instance.
    pub fn new(db: &'a AletheiaDB) -> Self {
        Self { db }
    }

    /// Analyze a node's semantic history to detect oscillation.
    ///
    /// Returns `None` if the node lacks sufficient history (e.g., < 3 states)
    /// or lacks the specified vector property.
    pub fn analyze(&self, node_id: NodeId, vector_property: &str) -> Result<Option<Oscillation>> {
        let history = self.db.get_node_history(node_id)?;

        // Extract historical vectors
        let mut vectors: Vec<Vec<f32>> = Vec::new();

        // Ensure chronological order by valid_time start
        let mut sorted_versions = history.versions.clone();
        sorted_versions.sort_by_key(|v| v.temporal.valid_time().start());

        for version in sorted_versions {
            if let Some(prop_val) = version.properties.get(vector_property) {
                #[allow(clippy::collapsible_if)]
                if let Some(vec) = prop_val.as_vector() {
                    vectors.push(vec.to_vec());
                }
            }
        }

        // We need at least 3 points to define an oscillation (A -> B -> A)
        if vectors.len() < 3 {
            return Ok(None);
        }

        // 1. Find the Poles (the two most distant points in the history)
        let mut max_dist_sq = 0.0_f32;
        let mut pole_a_idx = 0;
        let mut pole_b_idx = 0;

        for i in 0..vectors.len() {
            for j in (i + 1)..vectors.len() {
                let dist_sq = self.euclidean_dist_sq(&vectors[i], &vectors[j]);
                if dist_sq > max_dist_sq {
                    max_dist_sq = dist_sq;
                    pole_a_idx = i;
                    pole_b_idx = j;
                }
            }
        }

        if max_dist_sq == 0.0 {
            // Node hasn't moved
            return Ok(Some(Oscillation {
                swings: 0,
                pole_distance: 0.0,
                is_oscillating: false,
            }));
        }

        let pole_a = &vectors[pole_a_idx];
        let pole_b = &vectors[pole_b_idx];
        let pole_dist = max_dist_sq.sqrt();

        // 2. Project all points onto the axis defined by (Pole A -> Pole B)
        // Axis vector V = B - A
        let axis: Vec<f32> = pole_b
            .iter()
            .zip(pole_a.iter())
            .map(|(b, a)| b - a)
            .collect();
        let axis_mag_sq = self.euclidean_dist_sq(pole_a, pole_b); // Same as max_dist_sq

        // Midpoint on the axis is projection value 0.5 (where A=0, B=1)
        let mut projections = Vec::with_capacity(vectors.len());

        for vec in &vectors {
            // Vector relative to A: P = vec - A
            // Projection = (P dot V) / (V dot V)
            let mut dot = 0.0_f32;
            for i in 0..vec.len() {
                dot += (vec[i] - pole_a[i]) * axis[i];
            }
            let proj = dot / axis_mag_sq;
            projections.push(proj);
        }

        // 3. Count Swings across the midpoint (0.5)
        let mut swings = 0;
        let mut current_side = projections[0] > 0.5;

        for proj in projections.iter().skip(1) {
            let side = *proj > 0.5;
            if side != current_side {
                swings += 1;
                current_side = side;
            }
        }

        // Heuristic for "is oscillating": at least 2 swings and meaningful distance
        let is_oscillating = swings >= 2 && pole_dist > 0.1;

        Ok(Some(Oscillation {
            swings,
            pole_distance: pole_dist,
            is_oscillating,
        }))
    }

    fn euclidean_dist_sq(&self, a: &[f32], b: &[f32]) -> f32 {
        a.iter().zip(b.iter()).map(|(x, y)| (x - y) * (x - y)).sum()
    }
}

#[cfg(not(feature = "nova"))]
#[allow(deprecated)]
impl<'a> Pendulum<'a> {
    /// Create a new Pendulum instance.
    ///
    /// # Panics
    ///
    /// This method panics if the `nova` feature is not enabled.
    #[allow(unused_variables)]
    #[track_caller]
    pub fn new(db: &'a AletheiaDB) -> Self {
        panic!(
            "Pendulum requires the 'nova' feature. Add 'features = [\"nova\"]' to your Cargo.toml."
        );
    }

    /// Analyze a node's semantic history to detect oscillation.
    ///
    /// # Panics
    ///
    /// This method panics if the `nova` feature is not enabled.
    #[allow(unused_variables)]
    #[track_caller]
    pub fn analyze(&self, node_id: NodeId, vector_property: &str) -> Result<Option<Oscillation>> {
        panic!(
            "Pendulum requires the 'nova' feature. Add 'features = [\"nova\"]' to your Cargo.toml."
        );
    }
}

#[cfg(all(test, feature = "nova"))]
mod tests {
    use super::*;
    use crate::api::transaction::WriteOps;
    use crate::core::property::PropertyMapBuilder;
    use crate::core::temporal::time;

    #[test]
    fn test_pendulum_detects_oscillation() {
        let db = AletheiaDB::new().unwrap();

        let t0 = time::from_millis(100);
        let t1 = time::from_millis(200);
        let t2 = time::from_millis(300);
        let t3 = time::from_millis(400);

        // State A: [0, 0]
        let p0 = PropertyMapBuilder::new()
            .insert_vector("vec", &[0.0, 0.0])
            .build();
        let node_id = db
            .write(|tx| tx.create_node_with_valid_time("Agent", p0, Some(t0)))
            .unwrap();

        // State B: [10, 0]
        let p1 = PropertyMapBuilder::new()
            .insert_vector("vec", &[10.0, 0.0])
            .build();
        db.write(|tx| tx.update_node_with_valid_time(node_id, p1, Some(t1)))
            .unwrap();

        // State A: [0, 0]
        let p2 = PropertyMapBuilder::new()
            .insert_vector("vec", &[0.0, 0.0])
            .build();
        db.write(|tx| tx.update_node_with_valid_time(node_id, p2, Some(t2)))
            .unwrap();

        // State B: [10, 0]
        let p3 = PropertyMapBuilder::new()
            .insert_vector("vec", &[10.0, 0.0])
            .build();
        db.write(|tx| tx.update_node_with_valid_time(node_id, p3, Some(t3)))
            .unwrap();

        let pendulum = Pendulum::new(&db);
        let result = pendulum.analyze(node_id, "vec").unwrap().unwrap();

        // Path: A -> B -> A -> B
        // Projections: 0 -> 1 -> 0 -> 1
        // Midpoint: 0.5
        // Swings: 3
        assert_eq!(result.swings, 3);
        assert!((result.pole_distance - 10.0).abs() < 0.001);
        assert!(result.is_oscillating);
    }

    #[test]
    fn test_pendulum_linear_trajectory() {
        let db = AletheiaDB::new().unwrap();

        let t0 = time::from_millis(100);
        let t1 = time::from_millis(200);
        let t2 = time::from_millis(300);

        // State A: [0, 0]
        let p0 = PropertyMapBuilder::new()
            .insert_vector("vec", &[0.0, 0.0])
            .build();
        let node_id = db
            .write(|tx| tx.create_node_with_valid_time("Agent", p0, Some(t0)))
            .unwrap();

        // State B: [5, 0]
        let p1 = PropertyMapBuilder::new()
            .insert_vector("vec", &[5.0, 0.0])
            .build();
        db.write(|tx| tx.update_node_with_valid_time(node_id, p1, Some(t1)))
            .unwrap();

        // State C: [10, 0]
        let p2 = PropertyMapBuilder::new()
            .insert_vector("vec", &[10.0, 0.0])
            .build();
        db.write(|tx| tx.update_node_with_valid_time(node_id, p2, Some(t2)))
            .unwrap();

        let pendulum = Pendulum::new(&db);
        let result = pendulum.analyze(node_id, "vec").unwrap().unwrap();

        // Path: A -> B -> C
        // Projections: 0 -> 0.5 -> 1.0 (midpoint is 0.5, so 0 -> 0.5 is not a swing, 0.5 -> 1 is one swing)
        // Wait, current side > 0.5.
        // 0.0 > 0.5 = false.
        // 0.5 > 0.5 = false.
        // 1.0 > 0.5 = true.
        // Swings: 1
        assert_eq!(result.swings, 1);
        assert!(!result.is_oscillating, "1 swing is not oscillating");
    }

    #[test]
    fn test_pendulum_insufficient_history() {
        let db = AletheiaDB::new().unwrap();

        let p0 = PropertyMapBuilder::new()
            .insert_vector("vec", &[0.0, 0.0])
            .build();
        let node_id = db.create_node("Agent", p0).unwrap();

        let pendulum = Pendulum::new(&db);
        let result = pendulum.analyze(node_id, "vec").unwrap();

        assert!(
            result.is_none(),
            "Should return None for insufficient history"
        );
    }
}
