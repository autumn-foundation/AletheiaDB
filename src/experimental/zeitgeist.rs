//! Zeitgeist: Semantic Subgraph Evolution Engine.
//!
//! "What is the spirit of the times?"
//!
//! Zeitgeist measures the "cultural shift" or semantic center of gravity of a whole subgraph
//! over a time period. It calculates how a group of nodes' embeddings shift as a collective
//! over time.
//!
//! # The Hook
//! "Measure the evolving semantic soul of your subgraph. It doesn’t just track one node—it tracks the movement of a community."
//!
//! # Concepts
//! - **Centroid**: The average vector of a group of nodes at a specific time.
//! - **Shift**: The difference between the centroid at Time A and Time B.
//! - **Divergence**: The cosine distance between the two centroids, indicating how much the overall "meaning" changed.
//!
//! # Example
//!
//! ```rust
//! // Requires features = ["nova"]
//! # #[cfg(feature = "nova")]
//! # use aletheiadb::AletheiaDB;
//! # #[cfg(feature = "nova")]
//! # use aletheiadb::experimental::zeitgeist::Zeitgeist;
//! # #[cfg(feature = "nova")]
//! # use aletheiadb::core::temporal::time;
//! # #[cfg(feature = "nova")]
//! # fn example(db: &AletheiaDB) -> aletheiadb::core::error::Result<()> {
//! let zeitgeist = Zeitgeist::new(db);
//!
//! let nodes = vec![/* ... */];
//! let t1 = time::now();
//! let t2 = time::now(); // Normally later
//!
//! let shift = zeitgeist.analyze_shift(&nodes, "embedding", t1, t2)?;
//!
//! println!("The community shifted by {} over time", shift.divergence_score);
//! # Ok(())
//! # }
//! ```

use crate::AletheiaDB;
use crate::core::error::{Error, Result};
use crate::core::id::NodeId;
use crate::core::temporal::Timestamp;
use crate::core::vector::ops::{cosine_similarity, normalize};

/// The result of a Zeitgeist shift analysis.
#[derive(Debug, Clone)]
pub struct SubgraphShift {
    /// The centroid of the subgraph at the start time.
    pub start_centroid: Vec<f32>,
    /// The centroid of the subgraph at the end time.
    pub end_centroid: Vec<f32>,
    /// The vector difference between the start and end centroids.
    pub shift_vector: Vec<f32>,
    /// How much the collective meaning changed (1.0 - cosine_similarity).
    /// 0.0 means no change in direction, 2.0 means complete reversal.
    pub divergence_score: f32,
    /// Number of nodes successfully sampled at the start time.
    pub start_node_count: usize,
    /// Number of nodes successfully sampled at the end time.
    pub end_node_count: usize,
}

/// Zeitgeist engine for measuring subgraph semantic shifts over time.
pub struct Zeitgeist<'a> {
    db: &'a AletheiaDB,
}

impl<'a> Zeitgeist<'a> {
    /// Create a new Zeitgeist engine.
    pub fn new(db: &'a AletheiaDB) -> Self {
        Self { db }
    }

    /// Analyze how a subgraph's collective semantic meaning changed between two times.
    ///
    /// # Arguments
    /// * `nodes` - The list of node IDs forming the subgraph to analyze.
    /// * `property_name` - The vector property to use for semantic analysis.
    /// * `start_time` - The valid time for the beginning of the period.
    /// * `end_time` - The valid time for the end of the period.
    pub fn analyze_shift(
        &self,
        nodes: &[NodeId],
        property_name: &str,
        start_time: Timestamp,
        end_time: Timestamp,
    ) -> Result<SubgraphShift> {
        if nodes.is_empty() {
            return Err(Error::other("Cannot analyze shift of an empty subgraph"));
        }

        // Use the same valid_time as transaction_time for this analysis to see the state of the world
        // exactly as it was at that valid time.
        let (start_centroid, start_count) =
            self.compute_centroid(nodes, property_name, start_time, start_time)?;
        let (end_centroid, end_count) =
            self.compute_centroid(nodes, property_name, end_time, end_time)?;

        if start_count == 0 || end_count == 0 {
            return Err(Error::other(
                "Not enough nodes with valid vectors found at both times",
            ));
        }

        // Compute shift vector
        let mut shift_vector = Vec::with_capacity(start_centroid.len());
        for (s, e) in start_centroid.iter().zip(end_centroid.iter()) {
            shift_vector.push(e - s);
        }

        // Divergence is 1.0 - similarity
        // If similarity is 1.0 (identical direction), divergence is 0.0
        // If similarity is -1.0 (opposite direction), divergence is 2.0
        let similarity = cosine_similarity(&start_centroid, &end_centroid)?;
        let divergence_score = 1.0 - similarity;

        Ok(SubgraphShift {
            start_centroid,
            end_centroid,
            shift_vector,
            divergence_score,
            start_node_count: start_count,
            end_node_count: end_count,
        })
    }

    /// Internal helper to compute the centroid of a set of nodes at a given time.
    fn compute_centroid(
        &self,
        nodes: &[NodeId],
        property_name: &str,
        valid_time: Timestamp,
        tx_time: Timestamp,
    ) -> Result<(Vec<f32>, usize)> {
        let mut sum: Vec<f32> = Vec::new();
        let mut count = 0;
        let mut expected_dim = None;

        for &node_id in nodes {
            // Attempt to fetch the node at the specific point in time
            if let Ok(node) = self.db.get_node_at_time(node_id, valid_time, tx_time) {
                // Try to get the vector property
                let Some(prop) = node.properties.get(property_name) else {
                    continue;
                };
                let Some(vec) = prop.as_vector() else {
                    continue;
                };

                let dim = vec.len();

                if dim == 0 {
                    continue;
                }

                match expected_dim {
                    None => {
                        expected_dim = Some(dim);
                        sum = vec![0.0; dim];
                    }
                    Some(expected) if expected != dim => {
                        // Skip vectors of mismatched dimensions
                        continue;
                    }
                    _ => {}
                }

                // Add to sum
                for (i, val) in vec.iter().enumerate() {
                    sum[i] += val;
                }
                count += 1;
            }
        }

        if count == 0 {
            return Ok((Vec::new(), 0));
        }

        // Divide by count to get centroid
        let centroid: Vec<f32> = sum.into_iter().map(|s| s / count as f32).collect();

        // Normalize the centroid for accurate cosine similarity comparisons later
        let normalized = normalize(&centroid);

        Ok((normalized, count))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::transaction::WriteOps;
    use crate::core::property::PropertyMapBuilder;
    use crate::core::temporal::time;

    #[test]
    fn test_zeitgeist_shift() {
        let db = AletheiaDB::new().unwrap();

        let _t1 = time::now();

        // Time 1: Node 1 and Node 2 are near [1.0, 0.0]
        let (n1, n2) = db
            .write(|tx| {
                let n1 = tx.create_node(
                    "Concept",
                    PropertyMapBuilder::new()
                        .insert_vector("vec", &[1.0, 0.0])
                        .build(),
                )?;
                let n2 = tx.create_node(
                    "Concept",
                    PropertyMapBuilder::new()
                        .insert_vector("vec", &[0.8, 0.2])
                        .build(),
                )?;
                crate::core::error::Result::Ok((n1, n2))
            })
            .unwrap();

        let t1_effective = db.get_node(n1).unwrap().metadata.commit_timestamp.unwrap();

        // Wait a bit to ensure a different timestamp if wallclock is too fast, or just create it in the same tx
        std::thread::sleep(std::time::Duration::from_millis(10));

        let _t2 = Timestamp::new(t1_effective.wallclock() + 1000, 0).unwrap();

        // Time 2: Both nodes shift towards [0.0, 1.0]
        db.write(|tx| {
            tx.update_node(
                n1,
                PropertyMapBuilder::new()
                    .insert_vector("vec", &[0.0, 1.0])
                    .build(),
            )?;
            tx.update_node(
                n2,
                PropertyMapBuilder::new()
                    .insert_vector("vec", &[0.2, 0.8])
                    .build(),
            )
        })
        .unwrap();

        let t2_effective = db.get_node(n1).unwrap().metadata.commit_timestamp.unwrap();

        let zeitgeist = Zeitgeist::new(&db);
        let nodes = vec![n1, n2];

        let shift = zeitgeist
            .analyze_shift(&nodes, "vec", t1_effective, t2_effective)
            .unwrap();

        assert_eq!(shift.start_node_count, 2);
        assert_eq!(shift.end_node_count, 2);

        // Start centroid should be roughly near [1.0, 0.0] (normalized)
        assert!(shift.start_centroid[0] > 0.9);
        assert!(shift.start_centroid[1] < 0.2);

        // End centroid should be roughly near [0.0, 1.0] (normalized)
        assert!(shift.end_centroid[0] < 0.2);
        assert!(shift.end_centroid[1] > 0.9);

        println!("Divergence score: {}", shift.divergence_score);
        println!("Start centroid: {:?}", shift.start_centroid);
        println!("End centroid: {:?}", shift.end_centroid);

        // Divergence should be significant (they moved about 90 degrees)
        // Cosine sim of [1,0] and [0,1] is 0, so divergence should be ~1.0
        assert!(shift.divergence_score > 0.7);
    }

    #[test]
    fn test_zeitgeist_empty_subgraph() {
        let db = AletheiaDB::new().unwrap();
        let zeitgeist = Zeitgeist::new(&db);

        let t1 = time::now();
        let t2 = Timestamp::new(t1.wallclock() + 1000, 0).unwrap();

        let result = zeitgeist.analyze_shift(&[], "vec", t1, t2);
        assert!(result.is_err());
    }
}
