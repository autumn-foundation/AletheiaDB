//! Metaphor: Semantic Graph Alignment Engine.
//!
//! "What is the 'Steve Jobs' of the 'Culinary World'?"
//!
//! Metaphor finds analogies between two subgraphs by aligning them based on
//! both semantic similarity (vectors) and structural isomorphism (edges).
//!
//! # How it works
//! 1. **Semantic Anchoring**: Calculates initial similarity scores between all source and target nodes using vector embeddings.
//! 2. **Structural Propagation**: When a node pair is aligned, it boosts the alignment score of their neighbors ("If A maps to X, and A->B, then B should map to Y where X->Y").
//! 3. **Greedy Alignment**: Iteratively selects the best alignment pair and updates scores.
//!
//! # Use Cases
//! - **Knowledge Migration**: Map concepts from one domain to another.
//! - **Digital Twins**: Align a simulation graph with a real-world graph.
//! - **Recommender Systems**: "You liked Movie A (Graph A). Movie B (Graph B) has a similar character dynamic."
//!
//! ## Examples
//!
//! ```rust
//! // Requires features = ["nova"]
//! use aletheiadb::AletheiaDB;
//! use aletheiadb::experimental::metaphor::Metaphor;
//! use aletheiadb::core::property::PropertyMapBuilder;
//! use aletheiadb::index::vector::{HnswConfig, DistanceMetric};
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let db = AletheiaDB::new()?;
//! db.enable_vector_index("embedding", HnswConfig::new(2, DistanceMetric::Cosine))?;
//!
//! // Source Graph
//! let props_a = PropertyMapBuilder::new().insert_vector("embedding", &[1.0, 0.0]).build();
//! let a = db.create_node("Concept", props_a)?;
//!
//! // Target Graph
//! let props_x = PropertyMapBuilder::new().insert_vector("embedding", &[1.0, 0.0]).build();
//! let x = db.create_node("Analogue", props_x)?;
//!
//! let props_y = PropertyMapBuilder::new().insert_vector("embedding", &[0.0, 1.0]).build();
//! let y = db.create_node("Analogue", props_y)?;
//!
//! let metaphor = Metaphor::new(&db);
//! let alignment = metaphor.align(&[a], &[x, y], "embedding", 0.5)?;
//!
//! // A perfectly aligns with X based on semantic similarity
//! assert_eq!(alignment.mappings[0].source, a);
//! assert_eq!(alignment.mappings[0].target, x);
//! # Ok(())
//! # }
//! ```

#![allow(clippy::needless_range_loop, clippy::collapsible_if)]

use crate::AletheiaDB;
use crate::core::error::Result;
use crate::core::id::NodeId;
#[cfg(feature = "nova")]
use crate::core::vector::cosine_similarity;
#[cfg(feature = "nova")]
use std::collections::{HashMap, HashSet};

/// A single mapping in the alignment.
#[derive(Debug, Clone, PartialEq)]
pub struct Mapping {
    /// The node in the source subgraph.
    pub source: NodeId,
    /// The node in the target subgraph.
    pub target: NodeId,
    /// The final confidence score of this mapping.
    pub score: f32,
}

/// The result of an alignment operation.
#[derive(Debug, Clone)]
pub struct Alignment {
    /// The list of mappings found.
    pub mappings: Vec<Mapping>,
    /// The global alignment score (average of mapping scores).
    pub global_score: f32,
}

/// The Metaphor Engine.
pub struct Metaphor<'a> {
    #[allow(dead_code)]
    db: &'a AletheiaDB,
}

#[cfg(feature = "nova")]
impl<'a> Metaphor<'a> {
    /// Create a new Metaphor engine.
    pub fn new(db: &'a AletheiaDB) -> Self {
        Self { db }
    }

    /// Align a source subgraph to a target subgraph.
    ///
    /// This method greedily matches nodes from the source graph to the target graph.
    /// The algorithm first evaluates the purely semantic similarity using the specified `vector_property`.
    /// When a match is finalized, the score of the remaining neighbors is artificially boosted
    /// by the `structural_weight`. This ensures that if node `A` maps to node `X`, the algorithm
    /// prefers mapping `A`'s neighbors to `X`'s neighbors over mapping them to isolated nodes
    /// that might have a slightly higher baseline semantic score.
    ///
    /// # Arguments
    /// * `source_nodes` - The nodes in the source graph to map.
    /// * `target_nodes` - The candidate nodes in the target graph.
    /// * `vector_property` - The property name containing vector embeddings.
    /// * `structural_weight` - How much structural consistency boosts the score (e.g., 0.5).
    ///   A higher value prioritizes preserving the shape/topology of the graph over pure vector similarity.
    ///
    /// ## Examples
    ///
    /// ```rust
    /// // Requires features = ["nova"]
    /// use aletheiadb::AletheiaDB;
    /// use aletheiadb::experimental::metaphor::Metaphor;
    /// use aletheiadb::core::property::PropertyMapBuilder;
    /// use aletheiadb::index::vector::{HnswConfig, DistanceMetric};
    ///
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// let db = AletheiaDB::new()?;
    /// db.enable_vector_index("embedding", HnswConfig::new(2, DistanceMetric::Cosine))?;
    ///
    /// let a = db.create_node("A", PropertyMapBuilder::new().insert_vector("embedding", &[1.0, 0.0]).build())?;
    /// let x = db.create_node("X", PropertyMapBuilder::new().insert_vector("embedding", &[1.0, 0.0]).build())?;
    ///
    /// let metaphor = Metaphor::new(&db);
    /// let alignment = metaphor.align(&[a], &[x], "embedding", 0.5)?;
    ///
    /// assert_eq!(alignment.mappings.len(), 1);
    /// assert_eq!(alignment.mappings[0].source, a);
    /// assert_eq!(alignment.mappings[0].target, x);
    /// # Ok(())
    /// # }
    /// ```
    #[allow(clippy::needless_range_loop, clippy::collapsible_if)]
    pub fn align(
        &self,
        source_nodes: &[NodeId],
        target_nodes: &[NodeId],
        vector_property: &str,
        structural_weight: f32,
    ) -> Result<Alignment> {
        // 1. Pre-fetch vectors and edges for performance
        // (In a real massive graph, we'd be more lazy, but for subgraphs this is fine)
        let source_data = self.fetch_subgraph_data(source_nodes, vector_property)?;
        let target_data = self.fetch_subgraph_data(target_nodes, vector_property)?;

        // 2. Compute initial similarity matrix (Semantic Score)
        // Map: (SourceIdx, TargetIdx) -> Score
        // We use a flat vector for O(1) access and better cache locality.
        // Index = s_idx * target_len + t_idx
        let target_len = target_nodes.len();
        let mut scores = vec![0.0f32; source_nodes.len() * target_len];

        for s_idx in 0..source_nodes.len() {
            for t_idx in 0..target_len {
                let s_vec = &source_data[s_idx].vector;
                let t_vec = &target_data[t_idx].vector;

                let sim = if let (Some(sv), Some(tv)) = (s_vec, t_vec) {
                    let s = cosine_similarity(sv, tv).unwrap_or(0.0);
                    // Sanitize NaN to prevent logic errors in greedy selection
                    if s.is_nan() { 0.0 } else { s }
                } else {
                    0.0 // No vector match possible
                };
                scores[s_idx * target_len + t_idx] = sim;
            }
        }

        // 3. Greedy Alignment with Structural Propagation
        // We iterate until all source nodes are mapped or we run out of targets.
        // On each step, we pick the highest score, finalize it, and boost neighbors.

        // Create index maps for O(1) lookup
        let source_idx_map: HashMap<NodeId, usize> = source_nodes
            .iter()
            .enumerate()
            .map(|(i, &id)| (id, i))
            .collect();
        let target_idx_map: HashMap<NodeId, usize> = target_nodes
            .iter()
            .enumerate()
            .map(|(i, &id)| (id, i))
            .collect();

        let mut mappings = Vec::new();
        // Use boolean flags for mapped status to allow deterministic iteration over indices
        let mut source_mapped = vec![false; source_nodes.len()];
        let mut target_mapped = vec![false; target_nodes.len()];
        let mut mapped_count = 0;
        let min_len = source_nodes.len().min(target_nodes.len());

        while mapped_count < min_len {
            // Find best pair
            let mut best_pair = None;
            let mut best_score = f32::NEG_INFINITY;

            // Deterministic iteration: 0..N
            for s in 0..source_nodes.len() {
                if source_mapped[s] {
                    continue;
                }

                for t in 0..target_len {
                    if target_mapped[t] {
                        continue;
                    }

                    let score = scores[s * target_len + t];
                    if score > best_score {
                        best_score = score;
                        best_pair = Some((s, t));
                    }
                }
            }

            if let Some((best_s, best_t)) = best_pair {
                // Finalize mapping
                source_mapped[best_s] = true;
                target_mapped[best_t] = true;
                mapped_count += 1;

                mappings.push(Mapping {
                    source: source_nodes[best_s],
                    target: target_nodes[best_t],
                    score: best_score,
                });

                // Propagate Structure (Boost neighbors)
                // If S maps to T, then for all neighbors S' of S and T' of T:
                // Boost score(S', T')
                let s_neighbors = &source_data[best_s].neighbors;
                let t_neighbors = &target_data[best_t].neighbors;

                for &s_neighbor_id in s_neighbors {
                    if let Some(&s_neighbor_idx) = source_idx_map.get(&s_neighbor_id) {
                        if source_mapped[s_neighbor_idx] {
                            continue;
                        }

                        for &t_neighbor_id in t_neighbors {
                            if let Some(&t_neighbor_idx) = target_idx_map.get(&t_neighbor_id) {
                                if target_mapped[t_neighbor_idx] {
                                    continue;
                                }

                                // Boost the score
                                // Bounds check for safety (though maps should guarantee this)
                                if s_neighbor_idx < source_nodes.len()
                                    && t_neighbor_idx < target_len
                                {
                                    scores[s_neighbor_idx * target_len + t_neighbor_idx] +=
                                        structural_weight;
                                }
                            }
                        }
                    }
                }
            } else {
                break; // No more valid pairs
            }
        }

        let global_score = if mappings.is_empty() {
            0.0
        } else {
            mappings.iter().map(|m| m.score).sum::<f32>() / mappings.len() as f32
        };

        Ok(Alignment {
            mappings,
            global_score,
        })
    }

    fn fetch_subgraph_data(
        &self,
        nodes: &[NodeId],
        vector_property: &str,
    ) -> Result<Vec<NodeData>> {
        let mut data = Vec::with_capacity(nodes.len());
        for &id in nodes {
            let node = self.db.get_node(id)?;
            let vector = node
                .properties
                .get(vector_property)
                .and_then(|v| v.as_vector())
                .map(|v| v.to_vec());

            // Collect neighbors (undirected for structural similarity)
            // We include both outgoing and incoming edges to capture full structural context.
            let mut neighbors = HashSet::new();

            // Outgoing
            for edge_id in self.db.get_outgoing_edges(id) {
                if let Ok(target) = self.db.get_edge_target(edge_id) {
                    neighbors.insert(target);
                }
            }

            // Incoming
            for edge_id in self.db.get_incoming_edges(id) {
                if let Ok(source) = self.db.get_edge_source(edge_id) {
                    neighbors.insert(source);
                }
            }

            let mut neighbors_vec: Vec<NodeId> = neighbors.into_iter().collect();
            // Sort for determinism
            neighbors_vec.sort();

            data.push(NodeData {
                vector,
                neighbors: neighbors_vec,
            });
        }
        Ok(data)
    }
}

#[cfg(not(feature = "nova"))]
impl<'a> Metaphor<'a> {
    /// Create a new Metaphor engine.
    pub fn new(_db: &'a AletheiaDB) -> Self {
        panic!(
            "Experimental features like Metaphor require the 'nova' feature. Please enable it in your Cargo.toml:\n\n[dependencies]\naletheiadb = {{ version = \"...\", features = [\"nova\"] }}\n"
        );
    }

    #[cfg(test)]
    fn new_internal(db: &'a AletheiaDB) -> Self {
        Self { db }
    }

    /// Align a source subgraph to a target subgraph.
    pub fn align(
        &self,
        _source_nodes: &[NodeId],
        _target_nodes: &[NodeId],
        _vector_property: &str,
        _structural_weight: f32,
    ) -> Result<Alignment> {
        panic!("Experimental features like Metaphor require the 'nova' feature.");
    }
}

#[cfg(feature = "nova")]
struct NodeData {
    vector: Option<Vec<f32>>,
    neighbors: Vec<NodeId>,
}

#[cfg(feature = "nova")]
#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::property::PropertyMapBuilder;
    use crate::index::vector::{DistanceMetric, HnswConfig};

    #[test]
    fn test_metaphor_pure_semantic() {
        let db = AletheiaDB::new().unwrap();
        // Enable vector index
        let config = HnswConfig::new(2, DistanceMetric::Cosine);
        db.enable_vector_index("vec", config).unwrap();

        // Source: A [1, 0]
        // Target: X [1, 0], Y [0, 1]
        // Alignment should map A -> X

        let props_a = PropertyMapBuilder::new()
            .insert_vector("vec", &[1.0, 0.0])
            .build();
        let a = db.create_node("Source", props_a).unwrap();

        let props_x = PropertyMapBuilder::new()
            .insert_vector("vec", &[1.0, 0.0])
            .build();
        let x = db.create_node("Target", props_x).unwrap();

        let props_y = PropertyMapBuilder::new()
            .insert_vector("vec", &[0.0, 1.0])
            .build();
        let y = db.create_node("Target", props_y).unwrap();

        let metaphor = Metaphor::new(&db);
        let alignment = metaphor.align(&[a], &[x, y], "vec", 0.5).unwrap();

        assert_eq!(alignment.mappings.len(), 1);
        assert_eq!(alignment.mappings[0].source, a);
        assert_eq!(alignment.mappings[0].target, x);
        assert!(alignment.mappings[0].score > 0.9);
    }

    #[test]
    fn test_metaphor_exact_opposite_score() {
        // Regression test for issue where best_score was initialized to -1.0,
        // causing valid mappings with score -1.0 to be ignored.
        let db = AletheiaDB::new().unwrap();
        let config = HnswConfig::new(2, DistanceMetric::Cosine);
        db.enable_vector_index("vec", config).unwrap();

        // Source: A [1, 0]
        // Target: X [-1, 0] (Opposite, score -1.0)
        let props_a = PropertyMapBuilder::new()
            .insert_vector("vec", &[1.0, 0.0])
            .build();
        let a = db.create_node("Source", props_a).unwrap();

        let props_x = PropertyMapBuilder::new()
            .insert_vector("vec", &[-1.0, 0.0])
            .build();
        let x = db.create_node("Target", props_x).unwrap();

        let metaphor = Metaphor::new(&db);
        let alignment = metaphor.align(&[a], &[x], "vec", 0.0).unwrap();

        assert_eq!(alignment.mappings.len(), 1);
        assert_eq!(alignment.mappings[0].source, a);
        assert_eq!(alignment.mappings[0].target, x);
        // Score should be exactly -1.0 (clamped)
        assert!((alignment.mappings[0].score - -1.0).abs() < 1e-6);
    }

    #[test]
    fn test_metaphor_structural_disambiguation() {
        let db = AletheiaDB::new().unwrap();
        // Enable vector index
        let config = HnswConfig::new(2, DistanceMetric::Cosine);
        db.enable_vector_index("vec", config).unwrap();

        // Scenario: Two nodes in Source (A, B) and two in Target (X, Y).
        // A and B have identical vectors. X and Y have identical vectors.
        // A and X are semantically identical (1.0).
        // BUT:
        // A -> C (Anchor)
        // X -> Z (Anchor)
        // B -> (Nothing)
        // Y -> (Nothing)
        //
        // If we anchor C to Z first (based on unique vector),
        // then A should map to X because they are connected to the anchor.
        // B should map to Y (remaining).

        // Anchor C and Z (Unique vector [0, 1])
        let props_c = PropertyMapBuilder::new()
            .insert_vector("vec", &[0.0, 1.0])
            .build();
        let c = db.create_node("Anchor", props_c).unwrap();

        let props_z = PropertyMapBuilder::new()
            .insert_vector("vec", &[0.0, 1.0])
            .build();
        let z = db.create_node("Anchor", props_z).unwrap();

        // Ambiguous nodes A, B, X, Y (Vector [1, 0])
        let props_ambiguous = PropertyMapBuilder::new()
            .insert_vector("vec", &[1.0, 0.0])
            .build();

        let a = db.create_node("Node", props_ambiguous.clone()).unwrap();
        let b = db.create_node("Node", props_ambiguous.clone()).unwrap();

        let x = db.create_node("Node", props_ambiguous.clone()).unwrap();
        let y = db.create_node("Node", props_ambiguous.clone()).unwrap();

        // Structure: A -> C, X -> Z
        db.create_edge(a, c, "LINK", Default::default()).unwrap();
        db.create_edge(x, z, "LINK", Default::default()).unwrap();

        let metaphor = Metaphor::new(&db);
        let alignment = metaphor
            .align(
                &[c, a, b],
                &[z, x, y],
                "vec",
                0.5, // Structural weight
            )
            .unwrap();

        // Check C -> Z (Base semantic match)
        let map_c = alignment.mappings.iter().find(|m| m.source == c).unwrap();
        assert_eq!(map_c.target, z);

        // Check A -> X (Structurally boosted by C->Z)
        let map_a = alignment.mappings.iter().find(|m| m.source == a).unwrap();
        assert_eq!(map_a.target, x);
        assert!(map_a.score > 1.0); // 1.0 (Semantic) + 0.5 (Structure)

        // Check B -> Y (Leftover)
        let map_b = alignment.mappings.iter().find(|m| m.source == b).unwrap();
        assert_eq!(map_b.target, y);
    }
}

#[cfg(not(feature = "nova"))]
#[cfg(test)]
mod stub_tests {
    use super::*;
    use crate::AletheiaDB;

    #[test]
    #[should_panic(expected = "Experimental features like Metaphor require the 'nova' feature")]
    fn test_stub_new_panics() {
        let db = AletheiaDB::new().unwrap();
        let _ = Metaphor::new(&db);
    }

    #[test]
    #[should_panic(expected = "Experimental features like Metaphor require the 'nova' feature")]
    fn test_stub_align_panics() {
        let db = AletheiaDB::new().unwrap();
        let metaphor = Metaphor::new_internal(&db);
        let _ = metaphor.align(&[], &[], "vec", 0.0);
    }
}
