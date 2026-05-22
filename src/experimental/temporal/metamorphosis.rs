//! Metamorphosis: Semantic Half-Life Calculator.
//!
//! "How long does it take for a concept to completely change its meaning?"
//!
//! The Metamorphosis Engine measures the "semantic half-life" of a node.
//! It calculates the time it takes for a node's vector embedding to drop below
//! a certain cosine similarity threshold from its original state, indicating
//! that the concept has fundamentally shifted.
//!
//! # Concepts
//! - **Original State**: The first recorded vector for a node.
//! - **Semantic Half-Life**: The duration from the original state until the
//!   similarity drops below the threshold.
//!
//! # Use Cases
//! - **Concept Drift**: Detecting when user profiles or documents have fundamentally
//!   changed their topics.
//! - **Aging Out Data**: Retiring historical data that no longer resembles its original intent.
//!
//! # Example
//! ```rust,no_run
//! use aletheiadb::AletheiaDB;
//! use aletheiadb::experimental::temporal::metamorphosis::Metamorphosis;
//! use aletheiadb::core::id::NodeId;
//! use std::time::Duration;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let db = AletheiaDB::new()?;
//! # let node_id = db.create_node("Node", Default::default())?;
//! let engine = Metamorphosis::new(&db);
//!
//! // Calculate how long it took for the embedding to drop below 0.5 similarity
//! let half_life = engine.semantic_half_life(node_id, "embedding", 0.5)?;
//!
//! if let Some(duration) = half_life {
//!     println!("The concept mutated beyond recognition in {} seconds.", duration.as_secs());
//! } else {
//!     println!("The concept has not yet mutated beyond the threshold.");
//! }
//! # Ok(())
//! # }
//! ```

#![allow(clippy::collapsible_if)]

use crate::AletheiaDB;
use crate::core::error::Result;
use crate::core::id::NodeId;
use crate::core::vector::ops::cosine_similarity;
use std::time::Duration;

/// The Metamorphosis engine for calculating semantic half-life.
pub struct Metamorphosis<'a> {
    db: &'a AletheiaDB,
}

impl<'a> Metamorphosis<'a> {
    /// Create a new Metamorphosis instance.
    pub fn new(db: &'a AletheiaDB) -> Self {
        Self { db }
    }

    /// Calculate the semantic half-life of a node based on a specific property.
    ///
    /// This method finds the first recorded vector for the property, then scans
    /// forward in time. It returns the duration from the original version's timestamp
    /// to the first version whose vector has a cosine similarity below the threshold.
    ///
    /// # Arguments
    /// * `node_id` - The ID of the node to analyze.
    /// * `property_key` - The name of the vector property.
    /// * `similarity_threshold` - The cosine similarity threshold (e.g., 0.5).
    ///
    /// # Returns
    /// * `Ok(Some(Duration))` - The semantic half-life duration.
    /// * `Ok(None)` - If the node hasn't crossed the threshold or lacks history.
    pub fn semantic_half_life(
        &self,
        node_id: NodeId,
        property_key: &str,
        similarity_threshold: f32,
    ) -> Result<Option<Duration>> {
        let history = self.db.get_node_history(node_id)?;
        if history.versions.is_empty() {
            return Ok(None);
        }

        let mut original_vector: Option<Vec<f32>> = None;
        let mut original_timestamp: Option<i64> = None;

        for version in &history.versions {
            let node = self.db.get_node_at_version(node_id, version.version_number)?;
            if let Some(prop_val) = node.get_property(property_key) {
                if let Some(vec) = prop_val.as_vector() {
                    let current_vec = vec.to_vec();

                    if let Some(orig) = &original_vector {
                        // Compare
                        let sim = cosine_similarity(orig, &current_vec)?;

                        if sim < similarity_threshold {
                            let current_timestamp = version.temporal.valid_time().start().wallclock();
                            let start = original_timestamp.unwrap();
                            if current_timestamp >= start {
                                let diff_micros = (current_timestamp - start) as u64;
                                return Ok(Some(Duration::from_micros(diff_micros)));
                            }
                        }
                    } else {
                        original_vector = Some(current_vec);
                        original_timestamp = Some(version.temporal.valid_time().start().wallclock());
                    }
                }
            }
        }

        Ok(None)
    }
}

#[cfg(all(test, feature = "semantic-temporal"))]
mod tests {
    use super::*;
    use crate::core::property::PropertyMapBuilder;
    use crate::api::transaction::WriteOps;

    #[test]
    fn test_semantic_half_life() -> Result<()> {
        let db = AletheiaDB::new()?;

        // Version 1: Original state
        let props1 = PropertyMapBuilder::new()
            .insert_vector("embedding", &[1.0, 0.0])
            .build();
        let node_id = db.write(|tx| tx.create_node("Concept", props1))?;

        let engine = Metamorphosis::new(&db);

        // Hasn't changed yet
        assert!(engine.semantic_half_life(node_id, "embedding", 0.5)?.is_none());

        // Version 2: Slight change
        let props2 = PropertyMapBuilder::new()
            .insert_vector("embedding", &[0.9, 0.1]) // Cosine sim ~0.99
            .build();
        db.write(|tx| tx.update_node(node_id, props2))?;

        assert!(engine.semantic_half_life(node_id, "embedding", 0.5)?.is_none());

        // Version 3: Major change
        let props3 = PropertyMapBuilder::new()
            .insert_vector("embedding", &[0.0, 1.0]) // Cosine sim 0.0
            .build();
        db.write(|tx| tx.update_node(node_id, props3))?;

        let half_life = engine.semantic_half_life(node_id, "embedding", 0.5)?;
        assert!(half_life.is_some());

        Ok(())
    }
}
