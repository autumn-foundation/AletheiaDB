#![allow(clippy::collapsible_if)]

//! Mirror: Semantic Reflection Engine 🪞
//!
//! "Looking across the void and seeing yourself."
//!
//! Mirror identifies nodes that are semantically identical (or very similar)
//! but have no structural connections between them. They represent duplicate
//! entities, parallel concepts, or unexplored symmetries in the graph.
//!
//! # Concepts
//! - **Semantic Twin**: Two nodes with highly similar vector embeddings.
//! - **Structural Void**: The absence of any edges (in either direction) between the twins.
//! - **Reflection**: A detected pair of semantic twins separated by a structural void.
//!
//! # Use Cases
//! - **Entity Resolution**: Finding duplicate user profiles that haven't interacted.
//! - **Knowledge Discovery**: Connecting silos of information that discuss the same concept.
//!
//! # Example
//! ```rust
//! // Requires features = ["nova"]
//! use aletheiadb::AletheiaDB;
//! use aletheiadb::experimental::mirror::Mirror;
//! use aletheiadb::core::id::NodeId;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let db = AletheiaDB::new()?;
//! // ... setup graph ...
//! # let candidates = vec![];
//!
//! let mirror = Mirror::new(&db);
//! // Find twins with > 0.95 similarity
//! let reflections = mirror.find_reflections(&candidates, "embedding", 0.95)?;
//!
//! println!("Found {} reflections!", reflections.len());
//! # Ok(())
//! # }
//! ```

use crate::AletheiaDB;
use crate::api::transaction::ReadOps;
use crate::core::error::Result;
use crate::core::id::NodeId;
use crate::core::vector::cosine_similarity;

/// A detected semantic twin separated by a structural void.
#[derive(Debug, Clone, PartialEq)]
pub struct Reflection {
    /// The first node in the pair.
    pub node_a: NodeId,
    /// The second node in the pair.
    pub node_b: NodeId,
    /// The semantic similarity score (e.g., cosine similarity).
    pub similarity_score: f32,
}

/// The Mirror Engine.
pub struct Mirror<'a> {
    db: &'a AletheiaDB,
}

impl<'a> Mirror<'a> {
    /// Create a new Mirror Engine.
    pub fn new(db: &'a AletheiaDB) -> Self {
        Self { db }
    }

    /// Find reflections (semantic twins with no structural connection) among the candidates.
    ///
    /// # Arguments
    /// * `candidates` - The list of node IDs to check.
    /// * `property_name` - The vector property to use for semantic analysis.
    /// * `similarity_threshold` - Minimum similarity to be considered a twin (e.g., 0.9).
    pub fn find_reflections(
        &self,
        candidates: &[NodeId],
        property_name: &str,
        similarity_threshold: f32,
    ) -> Result<Vec<Reflection>> {
        let mut reflections = Vec::new();

        if candidates.len() < 2 {
            return Ok(reflections);
        }

        self.db.read(|tx| {
            // Extract vectors for all candidates
            let mut node_vectors = Vec::new();
            for &node_id in candidates {
                if let Ok(node) = tx.get_node(node_id) {
                    if let Some(prop) = node.get_property(property_name).and_then(|p| p.as_vector())
                    {
                        node_vectors.push((node_id, prop.to_vec()));
                    }
                }
            }

            // Compare all pairs
            for i in 0..node_vectors.len() {
                for j in (i + 1)..node_vectors.len() {
                    let (node_a, vec_a) = &node_vectors[i];
                    let (node_b, vec_b) = &node_vectors[j];

                    // Check similarity first (often cheaper than graph traversal if we have optimized SIMD)
                    let similarity = cosine_similarity(vec_a, vec_b)?;

                    if similarity >= similarity_threshold {
                        // They are semantic twins. Now check for a structural void.
                        let mut connected = false;

                        // Check A -> B
                        for edge_id in tx.get_outgoing_edges(*node_a) {
                            if let Ok(edge) = tx.get_edge(edge_id) {
                                if edge.target == *node_b {
                                    connected = true;
                                    break;
                                }
                            }
                        }

                        // Check B -> A
                        if !connected {
                            for edge_id in tx.get_outgoing_edges(*node_b) {
                                if let Ok(edge) = tx.get_edge(edge_id) {
                                    if edge.target == *node_a {
                                        connected = true;
                                        break;
                                    }
                                }
                            }
                        }

                        if !connected {
                            reflections.push(Reflection {
                                node_a: *node_a,
                                node_b: *node_b,
                                similarity_score: similarity,
                            });
                        }
                    }
                }
            }
            Ok::<(), crate::core::error::Error>(())
        })?;

        Ok(reflections)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::transaction::WriteOps;
    use crate::core::property::PropertyMapBuilder;

    #[test]
    fn test_mirror_finds_reflections() {
        let db = AletheiaDB::new().unwrap();

        let mut n1 = NodeId::new(0).unwrap();
        let mut n2 = NodeId::new(0).unwrap();
        let mut n3 = NodeId::new(0).unwrap();
        let mut n4 = NodeId::new(0).unwrap();

        db.write(|tx| {
            // Twin 1
            n1 = tx
                .create_node(
                    "Node",
                    PropertyMapBuilder::new()
                        .insert_vector("embedding", &[1.0, 0.0, 0.0])
                        .build(),
                )
                .unwrap();

            // Twin 2 (very similar to Twin 1, but no connection)
            n2 = tx
                .create_node(
                    "Node",
                    PropertyMapBuilder::new()
                        .insert_vector("embedding", &[0.99, 0.01, 0.0])
                        .build(),
                )
                .unwrap();

            // Twin 3 (identical to Twin 1, but connected)
            n3 = tx
                .create_node(
                    "Node",
                    PropertyMapBuilder::new()
                        .insert_vector("embedding", &[1.0, 0.0, 0.0])
                        .build(),
                )
                .unwrap();

            // Different concept
            n4 = tx
                .create_node(
                    "Node",
                    PropertyMapBuilder::new()
                        .insert_vector("embedding", &[0.0, 1.0, 0.0])
                        .build(),
                )
                .unwrap();

            // Connect n1 and n3
            tx.create_edge(n1, n3, "LINK", Default::default()).unwrap();

            Ok::<(), crate::core::error::Error>(())
        })
        .unwrap();

        let mirror = Mirror::new(&db);
        let candidates = vec![n1, n2, n3, n4];

        let reflections = mirror
            .find_reflections(&candidates, "embedding", 0.95)
            .unwrap();

        // n1 and n2 are reflections (similar, no connection)
        // n1 and n3 are NOT reflections (connected)
        // n2 and n3 are reflections (similar, no connection)
        // n4 is not similar to anything

        assert_eq!(reflections.len(), 2);

        let has_n1_n2 = reflections
            .iter()
            .any(|r| (r.node_a == n1 && r.node_b == n2) || (r.node_a == n2 && r.node_b == n1));
        assert!(has_n1_n2, "Should find reflection between n1 and n2");

        let has_n2_n3 = reflections
            .iter()
            .any(|r| (r.node_a == n2 && r.node_b == n3) || (r.node_a == n3 && r.node_b == n2));
        assert!(has_n2_n3, "Should find reflection between n2 and n3");
    }
}
