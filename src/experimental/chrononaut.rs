//! Chrononaut: Temporal Vector Relevance Tracker.
//!
//! "Given a vector query, when in history was this query MOST relevant?"
//!
//! Chrononaut searches the history of a node to find the specific point in time
//! where its semantic vector was closest to a given target vector. It allows you
//! to track when a concept "peaked" or was most aligned with a specific idea.
//!
//! # Example
//! ```rust,no_run
//! use aletheiadb::AletheiaDB;
//! use aletheiadb::experimental::chrononaut::Chrononaut;
//! use aletheiadb::core::id::NodeId;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let db = AletheiaDB::new()?;
//! let chrononaut = Chrononaut::new(&db);
//!
//! let target_vector = vec![0.5, 0.5];
//! # let node_id = NodeId::new(1)?;
//!
//! if let Some((peak_time, score)) = chrononaut.find_peak_relevance(node_id, "embedding", &target_vector)? {
//!     println!("Peak relevance was at {:?} with score {}", peak_time, score);
//! }
//! # Ok(())
//! # }
//! ```

use crate::AletheiaDB;
use crate::core::error::Result;
use crate::core::id::NodeId;
use crate::core::temporal::Timestamp;
use crate::core::vector::ops;

/// The Chrononaut engine for tracking temporal vector relevance.
pub struct Chrononaut<'a> {
    db: &'a AletheiaDB,
}

impl<'a> Chrononaut<'a> {
    /// Create a new Chrononaut instance.
    pub fn new(db: &'a AletheiaDB) -> Self {
        Self { db }
    }

    /// Find the point in history where a node's vector property was closest to the target vector.
    ///
    /// # Arguments
    /// * `node_id` - The node to analyze.
    /// * `property_name` - The vector property to use for semantic analysis.
    /// * `target_vector` - The vector to compare against.
    ///
    /// # Returns
    /// An `Option` containing the timestamp of peak relevance and the cosine similarity score.
    /// Returns `None` if the node has no history or the property was never a vector.
    pub fn find_peak_relevance(
        &self,
        node_id: NodeId,
        property_name: &str,
        target_vector: &[f32],
    ) -> Result<Option<(Timestamp, f32)>> {
        let history = self.db.get_node_history(node_id)?;

        let mut peak_time = None;
        let mut max_similarity = -2.0_f32; // Cosine similarity ranges from -1.0 to 1.0

        for version in &history.versions {
            #[allow(clippy::collapsible_if)]
            if let Some(prop) = version.properties.get(property_name) {
                if let Some(vec) = prop.as_vector() {
                    // Normalize the target vector and the version vector for cosine similarity
                    // Note: `ops::cosine_similarity` does not require vectors to be normalized beforehand,
                    // but it expects the same dimensionality.
                    if vec.len() == target_vector.len() {
                        let similarity = ops::cosine_similarity(vec, target_vector)?;

                        if similarity > max_similarity {
                            max_similarity = similarity;
                            // valid_time() returns a TimeRange, start() gives the Timestamp
                            peak_time = Some(version.temporal.valid_time().start());
                        }
                    }
                }
            }
        }

        if let Some(time) = peak_time {
            Ok(Some((time, max_similarity)))
        } else {
            Ok(None)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::transaction::WriteOps;
    use crate::core::property::PropertyMapBuilder;
    use crate::core::temporal::time;

    #[test]
    fn test_chrononaut_peak_relevance() {
        use crate::core::error::Error;

        let db = AletheiaDB::new().unwrap();

        let target_vector = vec![1.0, 0.0];

        // Ensure we wait a tiny bit to make timestamps distinct from db init
        std::thread::sleep(std::time::Duration::from_millis(10));

        // Time 1: Node created with vector [0.0, 1.0] (orthogonal to target, sim=0)
        let mut n1 = NodeId::new(0).unwrap();
        db.write(|tx| {
            n1 = tx
                .create_node(
                    "Concept",
                    PropertyMapBuilder::new()
                        .insert_vector("embedding", &[0.0, 1.0])
                        .build(),
                )
                .unwrap();
            Ok::<(), Error>(())
        })
        .unwrap();

        std::thread::sleep(std::time::Duration::from_millis(50));
        let expected_peak_time = time::now(); // Approximate time before we commit the peak update
        std::thread::sleep(std::time::Duration::from_millis(10));

        // Time 2: Node updated with vector [1.0, 0.0] (perfect match, sim=1)
        db.write(|tx| {
            tx.update_node(
                n1,
                PropertyMapBuilder::new()
                    .insert_vector("embedding", &[1.0, 0.0])
                    .build(),
            )
            .unwrap();
            Ok::<(), Error>(())
        })
        .unwrap();

        std::thread::sleep(std::time::Duration::from_millis(50));

        // Time 3: Node updated with vector [-1.0, 0.0] (opposite, sim=-1)
        db.write(|tx| {
            tx.update_node(
                n1,
                PropertyMapBuilder::new()
                    .insert_vector("embedding", &[-1.0, 0.0])
                    .build(),
            )
            .unwrap();
            Ok::<(), Error>(())
        })
        .unwrap();

        let chrononaut = Chrononaut::new(&db);

        // Find the peak relevance for target [1.0, 0.0]
        let result = chrononaut
            .find_peak_relevance(n1, "embedding", &target_vector)
            .unwrap();

        assert!(result.is_some(), "Expected to find a peak relevance");

        let (peak_time, score) = result.unwrap();

        // The peak score should be exactly 1.0
        assert!(
            (score - 1.0).abs() < 0.001,
            "Expected peak similarity to be 1.0, got {}",
            score
        );

        // The peak time should be close to `expected_peak_time` (after T1, before T3)
        // Note: The actual peak_time returned will be the transaction time of Time 2.
        // We ensure it is greater than the creation time and less than now.
        assert!(peak_time.wallclock() as u64 > expected_peak_time.wallclock() as u64 - 100_000);
    }

    #[test]
    fn test_chrononaut_no_history() {
        let db = AletheiaDB::new().unwrap();
        let chrononaut = Chrononaut::new(&db);

        // Use an invalid node ID
        let n1 = NodeId::new(999).unwrap();
        let target_vector = vec![1.0, 0.0];

        // Trying to get history for a non-existent node returns an error in AletheiaDB
        let result = chrononaut.find_peak_relevance(n1, "embedding", &target_vector);
        assert!(result.is_err(), "Expected an error for non-existent node");
    }

    #[test]
    fn test_chrononaut_no_vector() {
        use crate::core::error::Error;

        let db = AletheiaDB::new().unwrap();

        let mut n1 = NodeId::new(0).unwrap();
        db.write(|tx| {
            n1 = tx
                .create_node(
                    "Concept",
                    PropertyMapBuilder::new()
                        .insert("name", "Test") // No vector property
                        .build(),
                )
                .unwrap();
            Ok::<(), Error>(())
        })
        .unwrap();

        let chrononaut = Chrononaut::new(&db);
        let target_vector = vec![1.0, 0.0];

        // Node exists, but has no vector property
        let result = chrononaut
            .find_peak_relevance(n1, "embedding", &target_vector)
            .unwrap();

        assert!(
            result.is_none(),
            "Expected no peak relevance since there's no vector"
        );
    }
}
