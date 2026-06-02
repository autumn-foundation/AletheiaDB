//! DejaVu: Semantic Cycle Detection.
//!
//! "History doesn't repeat itself, but it often rhymes."
//!
//! DejaVu analyzes the temporal history of a node to detect if it has
//! returned to a previous semantic state. It identifies instances where
//! a node's vector embedding drifts away from an original state and then
//! later returns to it, forming a semantic cycle.
//!
//! # Example
//! ```rust,no_run
//! use aletheiadb::AletheiaDB;
//! use aletheiadb::experimental::temporal::dejavu::DejaVu;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let db = AletheiaDB::new()?;
//! let dejavu = DejaVu::new(&db);
//!
//! # let node_id = aletheiadb::core::id::NodeId::new(1).unwrap();
//! // Find cycles where the similarity returns to >= 0.95
//! let cycles = dejavu.detect_cycles(node_id, "embedding", 0.95)?;
//! for cycle in cycles {
//!     println!("Node returned to its version {} state at version {}!", cycle.older_version, cycle.newer_version);
//! }
//! # Ok(())
//! # }
//! ```

use crate::AletheiaDB;
use crate::core::error::Result;
use crate::core::id::NodeId;
use crate::core::vector::ops::cosine_similarity;

/// A detected semantic cycle in a node's history.
#[derive(Debug, Clone, PartialEq)]
pub struct Cycle {
    /// The version number of the older state.
    pub older_version: u64,
    /// The version number of the newer state that rhymes with the older state.
    pub newer_version: u64,
    /// The cosine similarity between the two versions.
    pub similarity: f32,
}

/// The DejaVu Cycle Detection Engine.
pub struct DejaVu<'a> {
    db: &'a AletheiaDB,
}

#[cfg(feature = "semantic-temporal")]
#[allow(clippy::collapsible_if)]
impl<'a> DejaVu<'a> {
    /// Create a new DejaVu engine.
    pub fn new(db: &'a AletheiaDB) -> Self {
        Self { db }
    }

    /// Detects semantic cycles in a node's history.
    ///
    /// A cycle is defined as a sequence of versions where a node starts at version A,
    /// drifts to an intermediate version B (similarity < threshold), and then returns
    /// to version C (similarity between A and C >= threshold).
    pub fn detect_cycles(
        &self,
        node_id: NodeId,
        vector_property: &str,
        threshold: f32,
    ) -> Result<Vec<Cycle>> {
        let history = self.db.get_node_history(node_id)?;
        let mut cycles = Vec::new();

        let mut vectors_with_versions = Vec::new();

        for version in &history.versions {
            if let Some(prop) = version.properties.get(vector_property) {
                if let Some(vec) = prop.as_vector() {
                    vectors_with_versions.push((version.version_number, vec.to_vec()));
                }
            }
        }

        // We need at least 3 points to form a drift-and-return cycle (A -> B -> C)
        if vectors_with_versions.len() < 3 {
            return Ok(cycles);
        }

        for i in 0..vectors_with_versions.len() {
            for j in (i + 2)..vectors_with_versions.len() {
                let (v1, vec1) = &vectors_with_versions[i];
                let (v2, vec2) = &vectors_with_versions[j];

                if let Ok(sim) = cosine_similarity(vec1, vec2) {
                    if sim >= threshold {
                        // Check if there was an actual "drift" between A and C
                        // i.e., at least one intermediate version has similarity < threshold
                        let mut drifted = false;
                        for (_, vec_k) in vectors_with_versions.iter().take(j).skip(i + 1) {
                            if let Ok(sim_k) = cosine_similarity(vec1, vec_k) {
                                if sim_k < threshold {
                                    drifted = true;
                                    break;
                                }
                            }
                        }

                        if drifted {
                            cycles.push(Cycle {
                                older_version: *v1,
                                newer_version: *v2,
                                similarity: sim,
                            });
                        }
                    }
                }
            }
        }

        Ok(cycles)
    }
}

#[cfg(all(test, feature = "semantic-temporal"))]
mod tests {
    use super::*;
    use crate::api::transaction::WriteOps;
    use crate::core::property::PropertyMapBuilder;

    #[test]
    fn test_dejavu_detect_cycle() {
        let db = AletheiaDB::new().unwrap();

        let mut node_id = NodeId::new(1).unwrap();

        db.write(|tx| {
            // Version 1: "Happy" state
            let props = PropertyMapBuilder::new()
                .insert_vector("mood", &[1.0, 0.0])
                .build();
            node_id = tx.create_node("Person", props).unwrap();
            Ok::<(), crate::core::error::Error>(())
        })
        .unwrap();

        db.write(|tx| {
            // Version 2: "Sad" state (orthogonal, similarity = 0)
            let props = PropertyMapBuilder::new()
                .insert_vector("mood", &[0.0, 1.0])
                .build();
            tx.update_node(node_id, props).unwrap();
            Ok::<(), crate::core::error::Error>(())
        })
        .unwrap();

        db.write(|tx| {
            // Version 3: "Happy" state again (returns to original)
            let props = PropertyMapBuilder::new()
                .insert_vector("mood", &[0.99, 0.1])
                .build();
            tx.update_node(node_id, props).unwrap();
            Ok::<(), crate::core::error::Error>(())
        })
        .unwrap();

        let dejavu = DejaVu::new(&db);
        let cycles = dejavu.detect_cycles(node_id, "mood", 0.9).unwrap();

        assert_eq!(cycles.len(), 1);
        assert_eq!(cycles[0].older_version, 1);
        assert_eq!(cycles[0].newer_version, 3);
        assert!(cycles[0].similarity > 0.9);
    }

    #[test]
    fn test_dejavu_no_drift() {
        let db = AletheiaDB::new().unwrap();

        let mut node_id = NodeId::new(1).unwrap();

        db.write(|tx| {
            // Version 1
            let props = PropertyMapBuilder::new()
                .insert_vector("mood", &[1.0, 0.0])
                .build();
            node_id = tx.create_node("Person", props).unwrap();
            Ok::<(), crate::core::error::Error>(())
        })
        .unwrap();

        db.write(|tx| {
            // Version 2 (Very similar)
            let props = PropertyMapBuilder::new()
                .insert_vector("mood", &[0.99, 0.0])
                .build();
            tx.update_node(node_id, props).unwrap();
            Ok::<(), crate::core::error::Error>(())
        })
        .unwrap();

        db.write(|tx| {
            // Version 3 (Still very similar)
            let props = PropertyMapBuilder::new()
                .insert_vector("mood", &[0.98, 0.0])
                .build();
            tx.update_node(node_id, props).unwrap();
            Ok::<(), crate::core::error::Error>(())
        })
        .unwrap();

        let dejavu = DejaVu::new(&db);
        // Because there's no intermediate version that "drifts" (drops below 0.9), it shouldn't be considered a cycle
        let cycles = dejavu.detect_cycles(node_id, "mood", 0.9).unwrap();

        assert_eq!(cycles.len(), 0, "Should not detect a cycle without drift");
    }
}
