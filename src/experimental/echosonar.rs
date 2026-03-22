//! EchoSonar: Cyclic Semantic Behavior Detector.
//!
//! "Does this concept keep returning to its origins?"
//!
//! EchoSonar tracks if a node's vector properties oscillate or frequently
//! revert to earlier states, indicating a "cyclic semantic behavior" or
//! "echoing" over time, rather than a linear semantic drift.
//!
//! # Concepts
//! - **Echo Score**: A score from 0.0 to 1.0 indicating how much a node's history
//!   returns to previous semantic states. A score closer to 1.0 means highly cyclic,
//!   while a score near 0.0 means it constantly moves into new semantic territory.
//!
//! # Example
//! ```rust,no_run
//! use aletheiadb::AletheiaDB;
//! use aletheiadb::experimental::echosonar::EchoSonar;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let db = AletheiaDB::new()?;
//! # let node_id = db.create_node("Concept", Default::default())?;
//! let sonar = EchoSonar::new(&db);
//!
//! let score = sonar.detect_echoes(node_id, "embedding")?;
//! println!("Echo Score: {:.4}", score);
//! # Ok(())
//! # }
//! ```

use crate::AletheiaDB;
use crate::core::error::Result;
use crate::core::id::NodeId;
use crate::core::vector::ops::cosine_similarity;

/// The EchoSonar Engine for detecting cyclic semantic shifts.
pub struct EchoSonar<'a> {
    db: &'a AletheiaDB,
}

impl<'a> EchoSonar<'a> {
    /// Create a new EchoSonar instance.
    pub fn new(db: &'a AletheiaDB) -> Self {
        Self { db }
    }

    /// Detect echoing behavior for a specific node's vector property.
    ///
    /// # Arguments
    /// * `node_id` - The ID of the node to analyze.
    /// * `property_name` - The vector property to track.
    pub fn detect_echoes(&self, node_id: NodeId, property_name: &str) -> Result<f32> {
        let history = self.db.get_node_history(node_id)?;

        let mut vectors = Vec::new();

        for version in history.versions {
            #[allow(clippy::collapsible_if)]
            if let Some(prop) = version.properties.get(property_name) {
                if let Some(vec) = prop.as_vector() {
                    vectors.push(vec.to_vec());
                }
            }
        }

        if vectors.len() < 3 {
            // Need at least 3 points to determine if it echoes (A -> B -> A)
            return Ok(0.0);
        }

        // Calculate echo score
        // We compare pairs of non-adjacent versions
        // E.g., A, B, C. Distance between A and C.
        // If A and C are highly similar, it's an echo.
        // If A, B, C are linearly drifting, A and C are very dissimilar.

        let mut total_echo = 0.0;
        let mut comparisons = 0;

        for i in 0..vectors.len() {
            for j in (i + 2)..vectors.len() {
                let similarity = cosine_similarity(&vectors[i], &vectors[j])?;
                // similarity is typically -1.0 to 1.0 (though often 0 to 1 for embeddings)
                // Let's map similarity to an echo score where highly similar (1.0) contributes highly to echo
                let normalized_sim = similarity.max(0.0);
                total_echo += normalized_sim;
                comparisons += 1;
            }
        }

        if comparisons == 0 {
            return Ok(0.0);
        }

        Ok(total_echo / (comparisons as f32))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::transaction::WriteOps;
    use crate::core::error::Error;
    use crate::core::property::PropertyMapBuilder;

    #[test]
    fn test_echosonar_high_echo() {
        let db = AletheiaDB::new().unwrap();

        let mut node_id = NodeId::new(0).unwrap();

        db.write(|tx| {
            // Version 1: Start at A [1.0, 0.0]
            node_id = tx
                .create_node(
                    "Concept",
                    PropertyMapBuilder::new()
                        .insert_vector("vec", &[1.0, 0.0])
                        .build(),
                )
                .unwrap();
            Ok::<(), Error>(())
        })
        .unwrap();

        std::thread::sleep(std::time::Duration::from_millis(10));

        db.write(|tx| {
            // Version 2: Move to B [0.0, 1.0]
            tx.update_node(
                node_id,
                PropertyMapBuilder::new()
                    .insert_vector("vec", &[0.0, 1.0])
                    .build(),
            )
            .unwrap();
            Ok::<(), Error>(())
        })
        .unwrap();

        std::thread::sleep(std::time::Duration::from_millis(10));

        db.write(|tx| {
            // Version 3: Return to A [1.0, 0.0]
            tx.update_node(
                node_id,
                PropertyMapBuilder::new()
                    .insert_vector("vec", &[1.0, 0.0])
                    .build(),
            )
            .unwrap();
            Ok::<(), Error>(())
        })
        .unwrap();

        let sonar = EchoSonar::new(&db);
        let score = sonar.detect_echoes(node_id, "vec").unwrap();

        // Distance A to C is 0 (cosine similarity 1)
        // Only one comparison is made (i=0, j=2)
        assert!(score > 0.9, "Echo score should be high for cyclic behavior");
    }

    #[test]
    fn test_echosonar_low_echo() {
        let db = AletheiaDB::new().unwrap();

        let mut node_id = NodeId::new(0).unwrap();

        db.write(|tx| {
            // Version 1: Start at A [1.0, 0.0]
            node_id = tx
                .create_node(
                    "Concept",
                    PropertyMapBuilder::new()
                        .insert_vector("vec", &[1.0, 0.0])
                        .build(),
                )
                .unwrap();
            Ok::<(), Error>(())
        })
        .unwrap();

        std::thread::sleep(std::time::Duration::from_millis(10));

        db.write(|tx| {
            // Version 2: Move to B [0.0, 1.0]
            tx.update_node(
                node_id,
                PropertyMapBuilder::new()
                    .insert_vector("vec", &[0.0, 1.0])
                    .build(),
            )
            .unwrap();
            Ok::<(), Error>(())
        })
        .unwrap();

        std::thread::sleep(std::time::Duration::from_millis(10));

        db.write(|tx| {
            // Version 3: Move to C [-1.0, 0.0]
            tx.update_node(
                node_id,
                PropertyMapBuilder::new()
                    .insert_vector("vec", &[-1.0, 0.0])
                    .build(),
            )
            .unwrap();
            Ok::<(), Error>(())
        })
        .unwrap();

        let sonar = EchoSonar::new(&db);
        let score = sonar.detect_echoes(node_id, "vec").unwrap();

        // Distance A to C is high (cosine similarity -1)
        // Only one comparison is made (i=0, j=2) -> max(0.0) -> 0.0
        assert!(score < 0.1, "Echo score should be low for linear drift");
    }

    #[test]
    fn test_echosonar_insufficient_history() {
        let db = AletheiaDB::new().unwrap();

        let mut node_id = NodeId::new(0).unwrap();

        db.write(|tx| {
            // Version 1: Start at A [1.0, 0.0]
            node_id = tx
                .create_node(
                    "Concept",
                    PropertyMapBuilder::new()
                        .insert_vector("vec", &[1.0, 0.0])
                        .build(),
                )
                .unwrap();
            Ok::<(), Error>(())
        })
        .unwrap();

        let sonar = EchoSonar::new(&db);
        let score = sonar.detect_echoes(node_id, "vec").unwrap();

        // Less than 3 points
        assert_eq!(score, 0.0);
    }
}
