//! Mnemosyne: Semantic Memory Consolidation.
//!
//! "What matters is what changed."
//!
//! Mnemosyne scans the history of a node and consolidates it into "Key Frames"
//! based on significant semantic shifts (vector distance) or structural changes.
//! This is useful for creating compressed histories for LLM context windows.
//!
//! # Concepts
//! - **Key Frame**: A version of the node where a significant change occurred.
//! - **Semantic Drift**: Accumulation of small vector changes that eventually become significant.
//! - **Consolidation**: Filtering out intermediate states that are just "noise".
//!
//! # Example
//! ```rust,no_run
//! use aletheiadb::AletheiaDB;
//! use aletheiadb::experimental::mnemosyne::{Mnemosyne, MemoryFrame};
//! use aletheiadb::core::id::NodeId;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let db = AletheiaDB::new()?;
//! # let node_id = db.create_node("N", Default::default())?;
//! let mnemosyne = Mnemosyne::new(&db);
//!
//! // Consolidate memory with a semantic threshold of 0.5
//! let frames = mnemosyne.consolidate_memory(node_id, "embedding", 0.5)?;
//!
//! for frame in frames {
//!     println!("[{}] {}", frame.timestamp, frame.reason);
//! }
//! # Ok(())
//! # }
//! ```

use crate::AletheiaDB;
use crate::core::error::Result;
use crate::core::id::{NodeId, VersionId};
use crate::core::property::PropertyMap;
use crate::core::vector::ops::euclidean_distance;

/// A consolidated memory snapshot.
#[derive(Debug, Clone)]
pub struct MemoryFrame {
    /// Timestamp of this frame (transaction time).
    pub timestamp: i64,
    /// The version ID corresponding to this frame.
    pub version_id: VersionId,
    /// The reason this frame was kept (e.g., "Initial", "Vector Shift: 0.8").
    pub reason: String,
    /// The properties at this point in time.
    pub properties: PropertyMap,
}

/// The Memory Consolidator engine.
pub struct Mnemosyne<'a> {
    db: &'a AletheiaDB,
}

impl<'a> Mnemosyne<'a> {
    /// Create a new Mnemosyne instance.
    pub fn new(db: &'a AletheiaDB) -> Self {
        Self { db }
    }

    /// Consolidate the history of a node into key frames.
    ///
    /// This method iterates through the node's history and keeps only versions that:
    /// 1. Are the initial version.
    /// 2. Have a vector distance > `threshold` from the last *kept* frame.
    /// 3. Have non-vector property changes (added/removed/modified).
    ///
    /// # Arguments
    /// * `node_id` - The node to analyze.
    /// * `vector_prop` - The property name for the vector embedding.
    /// * `threshold` - The semantic distance threshold (Euclidean).
    pub fn consolidate_memory(
        &self,
        node_id: NodeId,
        vector_prop: &str,
        threshold: f32,
    ) -> Result<Vec<MemoryFrame>> {
        let history = self.db.get_node_history(node_id)?;
        let mut frames = Vec::new();

        // State for tracking "last kept" context
        let mut last_kept_vector: Option<Vec<f32>> = None;
        let mut last_kept_props: Option<PropertyMap> = None;

        for (i, version) in history.versions.iter().enumerate() {
            let current_props = &version.properties;
            let current_vector = current_props
                .get(vector_prop)
                .and_then(|v| v.as_vector())
                .map(|v| v.to_vec());

            let timestamp = version.temporal.transaction_time().start().wallclock();

            // 1. Always keep the first version
            if i == 0 {
                frames.push(MemoryFrame {
                    timestamp,
                    version_id: version.version_id,
                    reason: "Initial State".to_string(),
                    properties: current_props.clone(),
                });
                last_kept_vector = current_vector;
                last_kept_props = Some(current_props.clone());
                continue;
            }

            // 2. Check for Semantic Drift (Vector Change)
            let mut semantic_change = false;
            let mut distance = 0.0;

            if let (Some(last_vec), Some(curr_vec)) = (&last_kept_vector, &current_vector) {
                // Calculate distance from LAST KEPT frame, not just previous version.
                // This captures slow drift.
                if let Ok(d) = euclidean_distance(last_vec, curr_vec) {
                    distance = d;
                    if distance > threshold {
                        semantic_change = true;
                    }
                }
            } else if last_kept_vector.is_some() != current_vector.is_some() {
                // Vector added or removed
                semantic_change = true;
                distance = f32::INFINITY; // Symbolic infinite distance
            }

            // 3. Check for Structural/Property Changes (ignoring the vector prop itself if it changed slightly)
            // We want to capture if OTHER properties changed.
            let mut property_change = false;
            if let Some(last_props) = &last_kept_props {
                // Simple check: are keys different? or values different?
                // We can use VersionDiff logic manually or just check equality.
                // Ideally we'd use VersionDiff but we are comparing arbitrary PropertyMaps (Last Kept vs Current),
                // and VersionDiff takes VersionIds which might imply direct ancestry in storage logic.
                // Let's just iterate keys.

                // Check for added/removed/modified keys excluding vector_prop (unless it was added/removed)
                for (k, v) in current_props.iter() {
                    // resolve key string to check if it matches vector_prop
                    // Optimization: check interned ID if we had it, but vector_prop is &str.
                    // We'll rely on property map iteration.
                    // A safer way is to assume any change in PropertyMap that isn't JUST a small vector shift is significant.
                    // But wait, if the vector changed (small shift), the PropertyMap *has* changed.
                    // So `last_props != current_props` will be true.
                    // We need to exclude the vector property from this check if we want to ignore small shifts.

                    // Implementation:
                    // 1. Check if non-vector properties changed.
                    // 2. If vector property changed, is it significant? (Handled by Step 2).

                    // Actually, we can just say:
                    // If semantic_change is true -> Keep.
                    // Else, if ANY other property changed -> Keep.

                    // How to check "any other property"?
                    // Compare maps excluding `vector_prop`.
                    // This requires resolving `vector_prop` to interned ID or resolving all keys.
                    // Let's resolve keys for now, it's safer.
                    if let Some(key_str) =
                        crate::core::GLOBAL_INTERNER.resolve_with(*k, |s| s.to_string())
                    {
                        if key_str == vector_prop {
                            continue;
                        }

                        // Check if value changed
                        match last_props.get(key_str.as_str()) {
                            Some(last_val) => {
                                if !last_val.semantically_equal(v) {
                                    property_change = true;
                                    break;
                                }
                            }
                            None => {
                                property_change = true; // Added
                                break;
                            }
                        }
                    }
                }

                if !property_change {
                    // Check for removed keys
                    for (k, _) in last_props.iter() {
                        if let Some(key_str) =
                            crate::core::GLOBAL_INTERNER.resolve_with(*k, |s| s.to_string())
                        {
                            if key_str == vector_prop {
                                continue;
                            }
                            if !current_props.contains_key(&key_str) {
                                property_change = true; // Removed
                                break;
                            }
                        }
                    }
                }
            } else {
                property_change = true; // Should be covered by Initial State, but for safety.
            }

            // Decide to keep
            let reason = if semantic_change && property_change {
                format!("Vector Shift ({:.4}) + Property Change", distance)
            } else if semantic_change {
                format!("Vector Shift ({:.4})", distance)
            } else if property_change {
                "Property Change".to_string()
            } else {
                String::new()
            };

            if !reason.is_empty() {
                frames.push(MemoryFrame {
                    timestamp,
                    version_id: version.version_id,
                    reason,
                    properties: current_props.clone(),
                });
                last_kept_vector = current_vector;
                last_kept_props = Some(current_props.clone());
            }
        }

        Ok(frames)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::transaction::WriteOps;
    use crate::core::property::PropertyMapBuilder;
    use crate::core::temporal::time;
    use crate::index::vector::{DistanceMetric, HnswConfig};

    #[test]
    fn test_mnemosyne_drift_consolidation() {
        let db = AletheiaDB::new().unwrap();
        // Config vector index
        let config = HnswConfig::new(2, DistanceMetric::Euclidean);
        db.enable_vector_index("vec", config).unwrap();

        // 1. Initial: [0.0, 0.0]
        let props = PropertyMapBuilder::new()
            .insert_vector("vec", &[0.0, 0.0])
            .build();
        let node = db.create_node("Node", props).unwrap();

        // 2. Small Drift: [0.1, 0.0] (Dist 0.1)
        // Sleep to ensure distinct timestamps
        std::thread::sleep(std::time::Duration::from_millis(1));
        db.write(|tx| {
            tx.update_node(
                node,
                PropertyMapBuilder::new()
                    .insert_vector("vec", &[0.1, 0.0])
                    .build(),
            )
        })
        .unwrap();

        // 3. Small Drift: [0.2, 0.0] (Dist from last kept [0.0] is 0.2)
        std::thread::sleep(std::time::Duration::from_millis(1));
        db.write(|tx| {
            tx.update_node(
                node,
                PropertyMapBuilder::new()
                    .insert_vector("vec", &[0.2, 0.0])
                    .build(),
            )
        })
        .unwrap();

        // 4. Big Jump: [1.0, 0.0] (Dist from last kept [0.0] is 1.0)
        std::thread::sleep(std::time::Duration::from_millis(1));
        db.write(|tx| {
            tx.update_node(
                node,
                PropertyMapBuilder::new()
                    .insert_vector("vec", &[1.0, 0.0])
                    .build(),
            )
        })
        .unwrap();

        let mnemosyne = Mnemosyne::new(&db);

        // Threshold 0.5
        // Expect:
        // Frame 1: Initial [0.0, 0.0]
        // Frame 2: [0.1, 0.0] -> Dist 0.1 < 0.5 (Skip)
        // Frame 3: [0.2, 0.0] -> Dist 0.2 < 0.5 (Skip)
        // Frame 4: [1.0, 0.0] -> Dist 1.0 > 0.5 (Keep)
        let frames = mnemosyne.consolidate_memory(node, "vec", 0.5).unwrap();

        assert_eq!(frames.len(), 2, "Should consolidate to 2 frames");
        assert_eq!(frames[0].reason, "Initial State");
        assert!(frames[1].reason.contains("Vector Shift"));

        // Check properties of last frame
        let last_vec = frames[1]
            .properties
            .get("vec")
            .unwrap()
            .as_vector()
            .unwrap();
        assert_eq!(last_vec, &[1.0, 0.0]);
    }

    #[test]
    fn test_mnemosyne_accumulation() {
        // Test that small drifts eventually trigger a frame if they accumulate
        let db = AletheiaDB::new().unwrap();
        let config = HnswConfig::new(2, DistanceMetric::Euclidean);
        db.enable_vector_index("vec", config).unwrap();

        let props = PropertyMapBuilder::new()
            .insert_vector("vec", &[0.0, 0.0])
            .build();
        let node = db.create_node("Node", props).unwrap();

        // Move 0.2 five times. Total 1.0. Threshold 0.5.
        // T1: 0.2 (Dist 0.2) -> Skip
        // T2: 0.4 (Dist 0.4) -> Skip
        // T3: 0.6 (Dist 0.6) -> Keep! (Reset base to 0.6)
        // T4: 0.8 (Dist from 0.6 is 0.2) -> Skip
        // T5: 1.0 (Dist from 0.6 is 0.4) -> Skip

        for i in 1..=5 {
            std::thread::sleep(std::time::Duration::from_millis(1));
            let val = i as f32 * 0.2;
            db.write(|tx| {
                tx.update_node(
                    node,
                    PropertyMapBuilder::new()
                        .insert_vector("vec", &[val, 0.0])
                        .build(),
                )
            })
            .unwrap();
        }

        let mnemosyne = Mnemosyne::new(&db);
        let frames = mnemosyne.consolidate_memory(node, "vec", 0.5).unwrap();

        // Expected: Initial, T3 (0.6)
        // T5 (1.0) is distance 0.4 from T3, so it is skipped with threshold 0.5.
        // Total 2 frames.
        assert_eq!(frames.len(), 2);

        let vec_t3 = frames[1]
            .properties
            .get("vec")
            .unwrap()
            .as_vector()
            .unwrap();
        // Float comparison with epsilon
        assert!((vec_t3[0] - 0.6).abs() < 1e-5);
    }

    #[test]
    fn test_mnemosyne_property_change() {
        let db = AletheiaDB::new().unwrap();
        let config = HnswConfig::new(2, DistanceMetric::Euclidean);
        db.enable_vector_index("vec", config).unwrap();

        let props = PropertyMapBuilder::new()
            .insert_vector("vec", &[0.0, 0.0])
            .insert("status", "ok")
            .build();
        let node = db.create_node("Node", props).unwrap();

        // Change property, keep vector same
        std::thread::sleep(std::time::Duration::from_millis(1));
        db.write(|tx| {
            tx.update_node(
                node,
                PropertyMapBuilder::new()
                    .insert_vector("vec", &[0.0, 0.0])
                    .insert("status", "error")
                    .build(),
            )
        })
        .unwrap();

        let mnemosyne = Mnemosyne::new(&db);
        let frames = mnemosyne.consolidate_memory(node, "vec", 0.5).unwrap();

        assert_eq!(frames.len(), 2);
        assert_eq!(frames[1].reason, "Property Change");
        assert_eq!(
            frames[1].properties.get("status").unwrap().as_str(),
            Some("error")
        );
    }
}
