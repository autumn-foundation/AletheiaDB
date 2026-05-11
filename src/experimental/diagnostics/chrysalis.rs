//! Chrysalis: Semantic Metamorphosis Detector.
//!
//! "How far have we drifted from our origins?"
//!
//! The Chrysalis Engine identifies nodes that have undergone radical semantic shifts
//! over their lifetime by comparing their current embedding to their original (genesis) embedding.
//!
//! # Concepts
//! - **Genesis Vector**: The first recorded vector embedding of a node.
//! - **Current Vector**: The latest vector embedding of a node.
//! - **Metamorphosis Score**: The Euclidean distance between the genesis and current vectors.
//!   A high score indicates a massive pivot or concept drift.
//!
//! # Example
//! ```rust,no_run
//! use aletheiadb::AletheiaDB;
//! use aletheiadb::experimental::diagnostics::chrysalis::ChrysalisDetector;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let db = AletheiaDB::new()?;
//! # let node_id = db.create_node("Node", Default::default())?;
//! let detector = ChrysalisDetector::new(&db);
//!
//! let report = detector.detect_metamorphosis(node_id, "embedding")?;
//! if let Some(r) = report {
//!     println!("Node drifted by {} from its origin", r.metamorphosis_score);
//! }
//!
//! # Ok(())
//! # }
//! ```

use crate::AletheiaDB;
use crate::core::error::Result;
use crate::core::id::NodeId;
use crate::core::temporal::Timestamp;
use crate::core::vector::euclidean_distance;

/// A report describing the metamorphosis of a node.
#[derive(Debug, Clone, PartialEq)]
pub struct MetamorphosisReport {
    /// The ID of the node.
    pub node_id: NodeId,
    /// The timestamp of the first recorded version with the property.
    pub genesis_time: Timestamp,
    /// The timestamp of the current version.
    pub current_time: Timestamp,
    /// The Euclidean distance between genesis and current vectors.
    pub metamorphosis_score: f32,
}

/// The Chrysalis Engine for detecting semantic metamorphosis.
pub struct ChrysalisDetector<'a> {
    db: &'a AletheiaDB,
}

#[cfg(feature = "semantic-diagnostics")]
impl<'a> ChrysalisDetector<'a> {
    /// Create a new ChrysalisDetector.
    pub fn new(db: &'a AletheiaDB) -> Self {
        Self { db }
    }

    /// Detect the metamorphosis of a specific node by comparing its first and latest vectors.
    ///
    /// Returns `Ok(None)` if the node doesn't have the required vector property in its history.
    pub fn detect_metamorphosis(
        &self,
        node_id: NodeId,
        vector_key: &str,
    ) -> Result<Option<MetamorphosisReport>> {
        let history = self.db.get_node_history(node_id)?;

        let mut genesis_vector = None;
        let mut genesis_time = None;
        let mut current_vector = None;
        let mut current_time = None;

        for version in &history.versions {
            if let Some(prop_val) = version.properties.get(vector_key) {
                if let Some(vec) = prop_val.as_vector() {
                    let ts = version.temporal.transaction_time().start();

                    // First valid vector found is the genesis vector
                    if genesis_vector.is_none() {
                        genesis_vector = Some(vec.to_vec());
                        genesis_time = Some(ts);
                    }

                    // Always update the current vector (last one seen will be the most recent)
                    current_vector = Some(vec.to_vec());
                    current_time = Some(ts);
                }
            }
        }

        match (genesis_vector, current_vector) {
            (Some(gv), Some(cv)) => {
                let dist = euclidean_distance(&gv, &cv)
                    .map_err(|e| crate::core::error::Error::other(e.to_string()))?;

                Ok(Some(MetamorphosisReport {
                    node_id,
                    genesis_time: genesis_time.unwrap(),
                    current_time: current_time.unwrap(),
                    metamorphosis_score: dist,
                }))
            }
            _ => Ok(None),
        }
    }
}

#[cfg(all(test, feature = "semantic-diagnostics"))]
mod tests {
    use super::*;
    use crate::api::transaction::WriteOps;
    use crate::core::property::PropertyMapBuilder;
    #[test]
    fn test_detect_metamorphosis() {
        let db = AletheiaDB::new().unwrap();

        let genesis_props = PropertyMapBuilder::new()
            .insert_vector("embedding", &[1.0, 0.0])
            .build();

        let mut tx = db.write_transaction().unwrap();
        let node_id = tx.create_node("Concept", genesis_props).unwrap();
        tx.commit().unwrap();

        let detector = ChrysalisDetector::new(&db);

        // At genesis, score should be 0 since it hasn't moved.
        let report = detector
            .detect_metamorphosis(node_id, "embedding")
            .unwrap()
            .unwrap();
        assert_eq!(report.metamorphosis_score, 0.0);

        // Pivot the node
        let current_props = PropertyMapBuilder::new()
            .insert_vector("embedding", &[0.0, 1.0])
            .build();

        let mut tx2 = db.write_transaction().unwrap();
        tx2.update_node(node_id, current_props).unwrap();
        tx2.commit().unwrap();

        // It has now moved
        let report2 = detector
            .detect_metamorphosis(node_id, "embedding")
            .unwrap()
            .unwrap();
        assert!(report2.metamorphosis_score > 0.0);
        assert_eq!(report2.metamorphosis_score, 2.0f32.sqrt());
    }

    #[test]
    fn test_no_vector() {
        let db = AletheiaDB::new().unwrap();

        let props = PropertyMapBuilder::new().insert("name", "Alice").build();
        let mut tx = db.write_transaction().unwrap();
        let node_id = tx.create_node("Person", props).unwrap();
        tx.commit().unwrap();

        let detector = ChrysalisDetector::new(&db);
        let result = detector.detect_metamorphosis(node_id, "embedding").unwrap();
        assert!(result.is_none());
    }
}
