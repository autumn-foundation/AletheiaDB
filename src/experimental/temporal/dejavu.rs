//! DejaVu: Semantic Recurrence Detector.
//!
//! "Haven't we been here before?"
//!
//! DejaVu compares a node's current semantic vector against the historical versions
//! of itself (or other nodes) to detect when a semantic state has recurred.
//!
//! # Concepts
//! - **Recurrence**: When a current semantic state closely matches a past historical state.
//! - **Self-Recurrence**: A node returning to a state it held in its own past (e.g. a user
//!   returning to an old topic of interest, a system cycling back to a previous failure mode).
//! - **Echo**: A node's current state matching the past state of a *different* node.
//!
//! # Example
//! ```rust,no_run
//! use aletheiadb::AletheiaDB;
//! use aletheiadb::experimental::temporal::dejavu::DejaVu;
//! use aletheiadb::core::id::NodeId;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let db = AletheiaDB::new()?;
//! let dejavu = DejaVu::new(&db);
//!
//! # let node_id = NodeId::new(1).unwrap();
//! // Find if this node is repeating its own history (similarity threshold 0.9)
//! let recurrences = dejavu.detect_self_recurrence(node_id, "embedding", 0.9)?;
//!
//! for recurrence in recurrences {
//!     println!("DejaVu! Current state matches version {} from {}",
//!         recurrence.version_number,
//!         recurrence.timestamp);
//! }
//! # Ok(())
//! # }
//! ```

use crate::AletheiaDB;
use crate::core::error::{Error, Result};
use crate::core::id::NodeId;
use crate::core::temporal::Timestamp;
use crate::core::vector::ops;

/// A detected recurrence of a semantic state.
#[derive(Debug, Clone, PartialEq)]
pub struct Recurrence {
    /// The node whose history matched.
    pub historical_node_id: NodeId,
    /// The version number that matched.
    pub version_number: u64,
    /// The valid time timestamp of the historical version.
    pub timestamp: Timestamp,
    /// The cosine similarity score of the match.
    pub similarity: f32,
}

/// The DejaVu engine for detecting semantic recurrences.
pub struct DejaVu<'a> {
    db: &'a AletheiaDB,
}

impl<'a> DejaVu<'a> {
    /// Create a new DejaVu instance.
    pub fn new(db: &'a AletheiaDB) -> Self {
        Self { db }
    }

    /// Detect if a node's current state is a recurrence of its own past history.
    ///
    /// # Arguments
    /// * `node_id` - The node to analyze.
    /// * `vector_property` - The name of the vector property.
    /// * `threshold` - The minimum cosine similarity to be considered a recurrence.
    pub fn detect_self_recurrence(
        &self,
        node_id: NodeId,
        vector_property: &str,
        threshold: f32,
    ) -> Result<Vec<Recurrence>> {
        self.detect_recurrence(node_id, node_id, vector_property, threshold)
    }

    /// Detect if a target node's current state is a recurrence of a historical node's past.
    ///
    /// # Arguments
    /// * `target_node` - The node whose current state is being checked.
    /// * `historical_node` - The node whose history is being searched.
    /// * `vector_property` - The name of the vector property.
    /// * `threshold` - The minimum cosine similarity to be considered a recurrence.
    pub fn detect_recurrence(
        &self,
        target_node: NodeId,
        historical_node: NodeId,
        vector_property: &str,
        threshold: f32,
    ) -> Result<Vec<Recurrence>> {
        // 1. Get the current vector of the target node.
        let target = self.db.get_node(target_node)?;
        let target_vec = match target
            .properties
            .get(vector_property)
            .and_then(|p| p.as_vector())
        {
            Some(v) => v,
            None => {
                return Err(Error::other(format!(
                    "Node {} missing vector property '{}'",
                    target_node, vector_property
                )));
            }
        };

        // 2. Get the history of the historical node.
        let history = self.db.get_node_history(historical_node)?;
        let mut recurrences = Vec::new();

        // 3. Compare current vector against all historical versions.
        // We skip the very last version if historical_node == target_node,
        // because the last version *is* the current state.
        let total_versions = history.versions.len();

        for (i, version) in history.versions.iter().enumerate() {
            if target_node == historical_node && i == total_versions - 1 {
                // Skip comparing the current state against itself.
                continue;
            }

            #[allow(clippy::collapsible_if)]
            if let Some(hist_prop) = version.properties.get(vector_property) {
                if let Some(hist_vec) = hist_prop.as_vector() {
                    let similarity = ops::cosine_similarity(target_vec, hist_vec)?;

                    if similarity >= threshold {
                        // Extract start time of validity for this version
                        let timestamp = version.temporal.valid_time().start();

                        recurrences.push(Recurrence {
                            historical_node_id: historical_node,
                            version_number: version.version_number,
                            timestamp,
                            similarity,
                        });
                    }
                }
            }
        }

        // Sort descending by similarity
        recurrences.sort_by(|a, b| b.similarity.partial_cmp(&a.similarity).unwrap());

        Ok(recurrences)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::transaction::WriteOps;
    use crate::core::property::PropertyMapBuilder;
    use crate::core::temporal::time;

    #[test]
    fn test_dejavu_self_recurrence() {
        let db = AletheiaDB::new().unwrap();

        let t0 = time::from_millis(1000);
        let t1 = time::from_millis(2000);
        let t2 = time::from_millis(3000);

        // V1: Initial state (State A)
        let props1 = PropertyMapBuilder::new()
            .insert_vector("vec", &[1.0, 0.0])
            .build();
        let node_id = db
            .write(|tx| tx.create_node_with_valid_time("Agent", props1, Some(t0)))
            .unwrap();

        // V2: Changed state (State B)
        db.write(|tx| {
            let p = PropertyMapBuilder::new()
                .insert_vector("vec", &[0.0, 1.0])
                .build();
            tx.update_node_with_valid_time(node_id, p, Some(t1))
        })
        .unwrap();

        // V3: Returned to initial state (State A)
        db.write(|tx| {
            let p = PropertyMapBuilder::new()
                .insert_vector("vec", &[0.99, 0.01]) // Very close to V1
                .build();
            tx.update_node_with_valid_time(node_id, p, Some(t2))
        })
        .unwrap();

        let dejavu = DejaVu::new(&db);

        // Check for self recurrence. V3 should match V1.
        let recurrences = dejavu.detect_self_recurrence(node_id, "vec", 0.9).unwrap();

        assert_eq!(
            recurrences.len(),
            1,
            "Should detect exactly one recurrence (V1)"
        );
        assert_eq!(recurrences[0].version_number, 1); // V1
        assert_eq!(recurrences[0].timestamp, t0);
        assert!(recurrences[0].similarity > 0.95);
    }

    #[test]
    fn test_dejavu_cross_node_recurrence() {
        let db = AletheiaDB::new().unwrap();

        let t0 = time::from_millis(1000);
        let t1 = time::from_millis(2000);

        // Node A history
        let props_a1 = PropertyMapBuilder::new()
            .insert_vector("vec", &[1.0, 0.0])
            .build();
        let node_a = db
            .write(|tx| tx.create_node_with_valid_time("Theme", props_a1, Some(t0)))
            .unwrap();

        db.write(|tx| {
            let p = PropertyMapBuilder::new()
                .insert_vector("vec", &[0.0, 1.0])
                .build();
            tx.update_node_with_valid_time(node_a, p, Some(t1))
        })
        .unwrap();

        // Node B is currently identical to Node A's PAST state (V1)
        let props_b = PropertyMapBuilder::new()
            .insert_vector("vec", &[1.0, 0.0])
            .build();
        let node_b = db.create_node("NewTheme", props_b).unwrap();

        let dejavu = DejaVu::new(&db);

        // Node B is repeating Node A's history
        let recurrences = dejavu
            .detect_recurrence(node_b, node_a, "vec", 0.9)
            .unwrap();

        assert_eq!(
            recurrences.len(),
            1,
            "Node B current state matches Node A past state"
        );
        assert_eq!(recurrences[0].historical_node_id, node_a);
        assert_eq!(recurrences[0].version_number, 1);
    }
}
