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

use crate::AletheiaDB;
use crate::core::error::Result;
use crate::core::id::NodeId;
use crate::core::vector::cosine_similarity;
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
    db: &'a AletheiaDB,
}

impl<'a> Metaphor<'a> {
    /// Create a new Metaphor engine.
    pub fn new(db: &'a AletheiaDB) -> Self {
        Self { db }
    }

    /// Align a source subgraph to a target subgraph.
    ///
    /// # Arguments
    /// * `source_nodes` - The nodes in the source graph to map.
    /// * `target_nodes` - The candidate nodes in the target graph.
    /// * `vector_property` - The property name containing vector embeddings.
    /// * `structural_weight` - How much structural consistency boosts the score (e.g., 0.5).
    #[allow(clippy::collapsible_if)]
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
        let mut scores: HashMap<(usize, usize), f32> = HashMap::new();

        for (s_idx, s_node) in source_data.iter().enumerate() {
            for (t_idx, t_node) in target_data.iter().enumerate() {
                let s_vec = &s_node.vector;
                let t_vec = &t_node.vector;

                let sim = if let (Some(sv), Some(tv)) = (s_vec, t_vec) {
                    cosine_similarity(sv, tv).unwrap_or(0.0)
                } else {
                    0.0 // No vector match possible
                };
                scores.insert((s_idx, t_idx), sim);
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
            let mut best_score = -1.0;

            // Deterministic iteration: 0..N
            for (s, is_mapped) in source_mapped.iter().enumerate() {
                if *is_mapped {
                    continue;
                }

                for (t, is_target_mapped) in target_mapped.iter().enumerate() {
                    if *is_target_mapped {
                        continue;
                    }

                    if let Some(&score) = scores.get(&(s, t)) {
                        if score > best_score {
                            best_score = score;
                            best_pair = Some((s, t));
                        }
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
                                if let Some(score) =
                                    scores.get_mut(&(s_neighbor_idx, t_neighbor_idx))
                                {
                                    *score += structural_weight;
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

struct NodeData {
    vector: Option<Vec<f32>>,
    neighbors: Vec<NodeId>,
}

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
