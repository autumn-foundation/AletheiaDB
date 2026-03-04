#![allow(clippy::collapsible_if)]

//! Entanglement: Detecting Quantum Entanglement in the Graph 🌌
//!
//! "Spooky action at a distance."
//!
//! The Entanglement Detector identifies pairs of nodes whose semantic vectors change
//! synchronously over time, even without direct edges. This reveals hidden correlations
//! or external forces acting on multiple nodes simultaneously.
//!
//! # Concepts
//! - **Entanglement Score**: A measure of how correlated the *changes* (deltas)
//!   in two nodes' vectors are over their history, grouped by transaction time.
//!
//! # Use Cases
//! - **Hidden Influences**: Finding accounts controlled by the same botnet because
//!   their interests (vectors) pivot at the exact same times.
//! - **Market Correlation**: Identifying stocks that move together structurally,
//!   even if in different sectors.
//!
//! # Example
//! ```rust
//! // Requires features = ["nova"]
//! use aletheiadb::AletheiaDB;
//! use aletheiadb::experimental::entanglement::EntanglementDetector;
//! use aletheiadb::core::id::NodeId;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let db = AletheiaDB::new()?;
//! # let node_id_1 = db.create_node("Node", Default::default())?;
//! # let node_id_2 = db.create_node("Node", Default::default())?;
//!
//! let detector = EntanglementDetector::new(&db);
//! let nodes = vec![node_id_1, node_id_2];
//! let entangled_pairs = detector.detect_entanglement(&nodes, "embedding")?;
//!
//! for pair in entangled_pairs {
//!     println!("Nodes {} and {} have entanglement score: {}",
//!         pair.node_a, pair.node_b, pair.score);
//! }
//! # Ok(())
//! # }
//! ```

use crate::AletheiaDB;
use crate::core::error::{Error, Result};
use crate::core::id::NodeId;
use crate::core::vector::ops;
use std::collections::HashMap;

/// A pair of entangled nodes.
#[derive(Debug, Clone, PartialEq)]
pub struct EntangledPair {
    /// The first node.
    pub node_a: NodeId,
    /// The second node.
    pub node_b: NodeId,
    /// The entanglement score (correlation of vector deltas).
    /// Range: [-1.0, 1.0]. High positive score = high entanglement.
    pub score: f32,
}

/// The Entanglement Detector.
pub struct EntanglementDetector<'a> {
    db: &'a AletheiaDB,
}

impl<'a> EntanglementDetector<'a> {
    /// Create a new Entanglement Detector.
    pub fn new(db: &'a AletheiaDB) -> Self {
        Self { db }
    }

    /// Detect entanglement between the given nodes based on a vector property.
    ///
    /// # Arguments
    /// * `nodes` - The list of node IDs to analyze.
    /// * `property_name` - The vector property to track.
    pub fn detect_entanglement(
        &self,
        nodes: &[NodeId],
        property_name: &str,
    ) -> Result<Vec<EntangledPair>> {
        if nodes.len() < 2 {
            return Err(Error::other("Need at least two nodes to detect entanglement"));
        }

        // 1. Gather historical vector deltas for each node, mapped by transaction time
        // HashMap of NodeId -> HashMap<Timestamp, DeltaVector>
        let mut node_deltas_by_time: HashMap<NodeId, HashMap<i64, Vec<f32>>> = HashMap::new();
        // Also keep track of all distinct transaction times where ANY node was updated
        let mut all_update_times: Vec<i64> = Vec::new();

        for &node_id in nodes {
            let history = self.db.get_node_history(node_id)?;
            let mut deltas_map = HashMap::new();
            let mut previous_vector: Option<Vec<f32>> = None;

            for version in history.versions.iter() {
                if let Ok(node) = self.db.get_node_at_version(node_id, version.version_number) {
                    if let Some(prop) = node.get_property(property_name).and_then(|p| p.as_vector()) {
                        let current_vector = prop.to_vec();

                        if let Some(prev) = &previous_vector {
                            if prev.len() == current_vector.len() {
                                // Calculate delta
                                let mut delta = vec![0.0; current_vector.len()];
                                for i in 0..current_vector.len() {
                                    delta[i] = current_vector[i] - prev[i];
                                }

                                // Group by transaction time start. BiTemporalInterval holds the timestamps.
                                let tx_time = version.temporal.transaction_time().start().wallclock();
                                deltas_map.insert(tx_time, delta);
                                all_update_times.push(tx_time);
                            }
                        }

                        previous_vector = Some(current_vector);
                    }
                }
            }
            node_deltas_by_time.insert(node_id, deltas_map);
        }

        // Ensure unique, sorted times
        all_update_times.sort_unstable();
        all_update_times.dedup();

        // 2. Compare pairs of nodes
        let mut entangled_pairs = Vec::new();

        for i in 0..nodes.len() {
            for j in (i + 1)..nodes.len() {
                let node_a = nodes[i];
                let node_b = nodes[j];

                if let (Some(deltas_a), Some(deltas_b)) = (node_deltas_by_time.get(&node_a), node_deltas_by_time.get(&node_b)) {
                    let mut total_similarity = 0.0;
                    let mut valid_comparisons = 0;

                    // We only correlate changes that happened at the EXACT SAME transaction time
                    // (i.e. part of the same transaction or same logical time tick).
                    for &time in &all_update_times {
                        if let (Some(da_raw), Some(db_raw)) = (deltas_a.get(&time), deltas_b.get(&time)) {
                            let mut da = da_raw.clone();
                            let mut db = db_raw.clone();

                            // Normalize deltas (ignore zero vectors)
                            if da.iter().all(|&x| x == 0.0) || db.iter().all(|&x| x == 0.0) {
                                continue;
                            }

                            ops::normalize_in_place(&mut da);
                            ops::normalize_in_place(&mut db);

                            if let Ok(sim) = ops::cosine_similarity(&da, &db) {
                                total_similarity += sim;
                                valid_comparisons += 1;
                            }
                        }
                    }

                    if valid_comparisons > 0 {
                        let score = total_similarity / valid_comparisons as f32;
                        entangled_pairs.push(EntangledPair {
                            node_a,
                            node_b,
                            score,
                        });
                    }
                }
            }
        }

        // Sort by score descending
        entangled_pairs.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));

        Ok(entangled_pairs)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::transaction::WriteOps;
    use crate::core::property::PropertyMapBuilder;

    #[test]
    fn test_entanglement_high_correlation_synchronous() {
        let db = AletheiaDB::new().unwrap();

        let mut n1 = NodeId::new(0).unwrap();
        let mut n2 = NodeId::new(0).unwrap();
        let mut n3 = NodeId::new(0).unwrap();

        // Create initial nodes
        db.write(|tx| {
            n1 = tx
                .create_node(
                    "Node",
                    PropertyMapBuilder::new()
                        .insert_vector("embedding", &[0.0, 0.0])
                        .build(),
                )
                .unwrap();

            n2 = tx
                .create_node(
                    "Node",
                    PropertyMapBuilder::new()
                        .insert_vector("embedding", &[1.0, 1.0])
                        .build(),
                )
                .unwrap();

            n3 = tx
                .create_node(
                    "Node",
                    PropertyMapBuilder::new()
                        .insert_vector("embedding", &[0.0, 0.0])
                        .build(),
                )
                .unwrap();

            Ok::<(), Error>(())
        })
        .unwrap();

        std::thread::sleep(std::time::Duration::from_millis(10));

        // Update step 1: n1 and n2 move synchronously (+1.0, 0.0)
        // n3 moves synchronously but in a different direction (0.0, +1.0)
        db.write(|tx| {
            tx.update_node(
                n1,
                PropertyMapBuilder::new()
                    .insert_vector("embedding", &[1.0, 0.0])
                    .build(),
            ).unwrap();

            tx.update_node(
                n2,
                PropertyMapBuilder::new()
                    .insert_vector("embedding", &[2.0, 1.0])
                    .build(),
            ).unwrap();

            tx.update_node(
                n3,
                PropertyMapBuilder::new()
                    .insert_vector("embedding", &[0.0, 1.0])
                    .build(),
            ).unwrap();
            Ok::<(), Error>(())
        }).unwrap();

        std::thread::sleep(std::time::Duration::from_millis(10));

        // Update step 2: n1 and n2 move synchronously (0.0, +1.0)
        // n3 moves synchronously but in a different direction (-1.0, 0.0)
        db.write(|tx| {
            tx.update_node(
                n1,
                PropertyMapBuilder::new()
                    .insert_vector("embedding", &[1.0, 1.0])
                    .build(),
            ).unwrap();

            tx.update_node(
                n2,
                PropertyMapBuilder::new()
                    .insert_vector("embedding", &[2.0, 2.0])
                    .build(),
            ).unwrap();

            tx.update_node(
                n3,
                PropertyMapBuilder::new()
                    .insert_vector("embedding", &[-1.0, 1.0])
                    .build(),
            ).unwrap();
            Ok::<(), Error>(())
        }).unwrap();

        let detector = EntanglementDetector::new(&db);
        let nodes = vec![n1, n2, n3];
        let pairs = detector.detect_entanglement(&nodes, "embedding").unwrap();

        // n1 and n2 should be highly entangled
        let n1_n2_pair = pairs.iter().find(|p| (p.node_a == n1 && p.node_b == n2) || (p.node_a == n2 && p.node_b == n1)).unwrap();
        assert!(n1_n2_pair.score > 0.99);

        // n1 and n3 should have low or negative entanglement
        let n1_n3_pair = pairs.iter().find(|p| (p.node_a == n1 && p.node_b == n3) || (p.node_a == n3 && p.node_b == n1)).unwrap();
        assert!(n1_n3_pair.score < 0.1);
    }

    #[test]
    fn test_entanglement_asynchronous_updates_ignored() {
        let db = AletheiaDB::new().unwrap();

        let mut n1 = NodeId::new(0).unwrap();
        let mut n2 = NodeId::new(0).unwrap();

        db.write(|tx| {
            n1 = tx
                .create_node(
                    "Node",
                    PropertyMapBuilder::new()
                        .insert_vector("embedding", &[0.0, 0.0])
                        .build(),
                )
                .unwrap();

            n2 = tx
                .create_node(
                    "Node",
                    PropertyMapBuilder::new()
                        .insert_vector("embedding", &[1.0, 1.0])
                        .build(),
                )
                .unwrap();
            Ok::<(), Error>(())
        }).unwrap();

        std::thread::sleep(std::time::Duration::from_millis(10));

        // Node 1 updates alone
        db.write(|tx| {
            tx.update_node(
                n1,
                PropertyMapBuilder::new()
                    .insert_vector("embedding", &[1.0, 0.0])
                    .build(),
            ).unwrap();
            Ok::<(), Error>(())
        }).unwrap();

        std::thread::sleep(std::time::Duration::from_millis(10));

        // Node 2 updates alone, but with the exact same delta (+1.0, 0.0)
        db.write(|tx| {
            tx.update_node(
                n2,
                PropertyMapBuilder::new()
                    .insert_vector("embedding", &[2.0, 1.0])
                    .build(),
            ).unwrap();
            Ok::<(), Error>(())
        }).unwrap();

        let detector = EntanglementDetector::new(&db);
        let nodes = vec![n1, n2];
        let pairs = detector.detect_entanglement(&nodes, "embedding").unwrap();

        // There should be NO entanglement score because their updates were entirely asynchronous
        assert!(pairs.is_empty());
    }
}
