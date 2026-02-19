//! Doppelganger: Semantic-Structural Identity Detection.
//!
//! "We are more alike than unalike, my dear." - Maya Angelou
//!
//! The Doppelganger engine finds nodes that are structurally and semantically identical
//! to a target node, even if they have different IDs. This is useful for:
//! - De-duplication: Finding accidental duplicates.
//! - Fraud Detection: Detecting sockpuppets with identical behavioral signatures.
//! - Compression: Identifying candidates for merging.
//!
//! # Modes
//! - **Exact**: Requires identical properties AND identical edges to the EXACT SAME neighbors.
//! - **Structural**: Requires identical properties AND identical edge labels (but neighbors can differ).

use crate::AletheiaDB;
use crate::core::error::Result;
use crate::core::id::NodeId;
use crate::core::interning::GLOBAL_INTERNER;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

/// Modes for identity detection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DoppelgangerMode {
    /// Nodes must have identical properties and connect to the EXACT SAME neighbors (same IDs).
    Exact,
    /// Nodes must have identical properties and connect to neighbors with the SAME LABELS.
    /// The identity of the neighbor is ignored, only the relationship type matters.
    Structural,
}

/// A fingerprint representing the identity of a node.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct IdentityFingerprint {
    pub hash: u64,
}

/// The Doppelganger Engine.
pub struct DoppelgangerEngine<'a> {
    db: &'a AletheiaDB,
}

impl<'a> DoppelgangerEngine<'a> {
    /// Create a new DoppelgangerEngine.
    pub fn new(db: &'a AletheiaDB) -> Self {
        Self { db }
    }

    /// Compute the identity fingerprint for a node.
    pub fn compute_fingerprint(
        &self,
        node_id: NodeId,
        mode: DoppelgangerMode,
    ) -> Result<IdentityFingerprint> {
        let mut hasher = DefaultHasher::new();

        // 1. Hash Properties
        let node = self.db.get_node(node_id)?;

        // Convert to vec for sorting
        let mut props: Vec<_> = node.properties.iter().collect();
        // Sort by resolved key string for deterministic order
        props.sort_by(|(k1, _), (k2, _)| {
            let s1 = GLOBAL_INTERNER
                .resolve_with(**k1, |s| s.to_string())
                .unwrap_or_default();
            let s2 = GLOBAL_INTERNER
                .resolve_with(**k2, |s| s.to_string())
                .unwrap_or_default();
            s1.cmp(&s2)
        });

        for (key, value) in props {
            // Hash key string
            GLOBAL_INTERNER.resolve_with(*key, |s| s.hash(&mut hasher));

            // Hash value (serialized bytes)
            // Note: serialize() returns Result, we propagate error.
            let bytes = value.serialize()?;
            bytes.hash(&mut hasher);
        }

        // 2. Hash Edges
        // Outgoing
        let outgoing = self.db.get_outgoing_edges(node_id);
        let mut out_fingerprints = Vec::new();

        for edge_id in outgoing {
            let edge = self.db.get_edge(edge_id)?;
            let label = GLOBAL_INTERNER
                .resolve_with(edge.label, |s| s.to_string())
                .unwrap_or_default();

            match mode {
                DoppelgangerMode::Exact => {
                    // Hash (Label, "OUT", TargetID)
                    out_fingerprints.push((label, "OUT", Some(edge.target)));
                }
                DoppelgangerMode::Structural => {
                    // Hash (Label, "OUT") - Ignore TargetID
                    out_fingerprints.push((label, "OUT", None));
                }
            }
        }

        // Incoming
        let incoming = self.db.get_incoming_edges(node_id);
        let mut in_fingerprints = Vec::new();

        for edge_id in incoming {
            let edge = self.db.get_edge(edge_id)?;
            let label = GLOBAL_INTERNER
                .resolve_with(edge.label, |s| s.to_string())
                .unwrap_or_default();

            match mode {
                DoppelgangerMode::Exact => {
                    // Hash (Label, "IN", SourceID)
                    in_fingerprints.push((label, "IN", Some(edge.source)));
                }
                DoppelgangerMode::Structural => {
                    // Hash (Label, "IN") - Ignore SourceID
                    in_fingerprints.push((label, "IN", None));
                }
            }
        }

        // Sort edges deterministically
        out_fingerprints.sort();
        in_fingerprints.sort();

        // Hash sorted edges
        for fp in out_fingerprints {
            fp.hash(&mut hasher);
        }
        for fp in in_fingerprints {
            fp.hash(&mut hasher);
        }

        Ok(IdentityFingerprint {
            hash: hasher.finish(),
        })
    }

    /// Find doppelgangers for a target node among all nodes with the same label.
    pub fn find_candidates(
        &self,
        target_id: NodeId,
        mode: DoppelgangerMode,
    ) -> Result<Vec<NodeId>> {
        let target_fingerprint = self.compute_fingerprint(target_id, mode)?;
        let target_label_id = self.db.current.get_node_label(target_id)?;

        // Resolve label to string for scan
        let target_label = GLOBAL_INTERNER
            .resolve_with(target_label_id, |s| s.to_string())
            .ok_or_else(|| {
                crate::core::error::Error::Storage(
                    crate::core::error::StorageError::InconsistentState {
                        reason: format!("Label {} not found in interner", target_label_id.as_u32()),
                    },
                )
            })?;

        let mut matches = Vec::new();

        // Scan all candidates
        for candidate_id in self.db.scan_nodes_by_label(&target_label) {
            if candidate_id == target_id {
                continue;
            }

            let fp = self.compute_fingerprint(candidate_id, mode)?;
            if fp == target_fingerprint {
                matches.push(candidate_id);
            }
        }

        Ok(matches)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::property::PropertyMapBuilder;

    #[test]
    fn test_exact_doppelganger() {
        let db = AletheiaDB::new().unwrap();

        // Create common neighbor
        let neighbor = db
            .create_node("Neighbor", PropertyMapBuilder::new().build())
            .unwrap();

        // Create Node A
        let props = PropertyMapBuilder::new().insert("name", "Clone").build();
        let node_a = db.create_node("Person", props.clone()).unwrap();
        db.create_edge(node_a, neighbor, "KNOWS", PropertyMapBuilder::new().build())
            .unwrap();

        // Create Node B (Identical)
        let node_b = db.create_node("Person", props).unwrap();
        db.create_edge(node_b, neighbor, "KNOWS", PropertyMapBuilder::new().build())
            .unwrap();

        let engine = DoppelgangerEngine::new(&db);

        // Exact Mode
        let matches = engine
            .find_candidates(node_a, DoppelgangerMode::Exact)
            .unwrap();
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0], node_b);

        // Structural Mode
        let matches_struct = engine
            .find_candidates(node_a, DoppelgangerMode::Structural)
            .unwrap();
        assert_eq!(matches_struct.len(), 1);
        assert_eq!(matches_struct[0], node_b);
    }

    #[test]
    fn test_structural_doppelganger() {
        let db = AletheiaDB::new().unwrap();

        // Create different neighbors
        let neighbor_1 = db
            .create_node(
                "Neighbor",
                PropertyMapBuilder::new().insert("id", 1).build(),
            )
            .unwrap();
        let neighbor_2 = db
            .create_node(
                "Neighbor",
                PropertyMapBuilder::new().insert("id", 2).build(),
            )
            .unwrap();

        // Create Node A -> Neighbor 1
        let props = PropertyMapBuilder::new().insert("role", "Worker").build();
        let node_a = db.create_node("Person", props.clone()).unwrap();
        db.create_edge(
            node_a,
            neighbor_1,
            "REPORTS_TO",
            PropertyMapBuilder::new().build(),
        )
        .unwrap();

        // Create Node B -> Neighbor 2
        let node_b = db.create_node("Person", props).unwrap();
        db.create_edge(
            node_b,
            neighbor_2,
            "REPORTS_TO",
            PropertyMapBuilder::new().build(),
        )
        .unwrap();

        let engine = DoppelgangerEngine::new(&db);

        // Exact Mode: Should fail (different neighbor IDs)
        let matches_exact = engine
            .find_candidates(node_a, DoppelgangerMode::Exact)
            .unwrap();
        assert!(matches_exact.is_empty());

        // Structural Mode: Should pass (same edge label "REPORTS_TO")
        let matches_struct = engine
            .find_candidates(node_a, DoppelgangerMode::Structural)
            .unwrap();
        assert_eq!(matches_struct.len(), 1);
        assert_eq!(matches_struct[0], node_b);
    }
}
