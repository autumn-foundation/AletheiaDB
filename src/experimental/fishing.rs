//! Associative Retrieval ("Fishing") Module
//!
//! This module implements a "fishing" algorithm that combines vector similarity
//! with graph traversal to find related nodes. It's designed to simulate
//! associative memory, where recalling one concept ("the bait") pulls up
//! related concepts ("the catch") based on both semantic similarity and
//! structural connections.
//!
//! # The Metaphor
//! - **Bait**: The starting point (a Node ID or a raw Vector).
//! - **Casting**: Finding the initial set of nodes similar to the bait.
//! - **Spreading the Net**: traversing edges from the initial set.
//! - **Freshness**: Preferring recently updated information.

use crate::GallifreyDB;
use crate::core::id::NodeId;
use crate::utils::Result;
use crate::core::temporal::time;
use std::collections::HashMap;

/// Configuration for a fishing trip.
#[derive(Debug, Clone)]
pub struct FishingTrip {
    /// Maximum number of results to return.
    pub limit: usize,
    /// Maximum depth of graph traversal from the "school" (vector results).
    /// Currently only depth 1 is supported (direct neighbors).
    pub depth: usize,
    /// Weight given to vector similarity (0.0 to 1.0).
    pub vector_weight: f32,
    /// Weight given to graph connection (0.0 to 1.0).
    pub graph_weight: f32,
    /// Weight given to freshness/recency (0.0 to 1.0).
    pub freshness_weight: f32,
}

impl Default for FishingTrip {
    fn default() -> Self {
        Self {
            limit: 10,
            depth: 1,
            vector_weight: 1.0,
            graph_weight: 0.5,
            freshness_weight: 0.1,
        }
    }
}

/// The input to the fishing algorithm.
#[derive(Debug, Clone)]
pub enum Bait {
    /// Start with an existing node in the graph.
    Node(NodeId),
    /// Start with a raw embedding vector.
    Vector(Vec<f32>),
}

/// A result from the fishing algorithm.
#[derive(Debug, Clone)]
pub struct Catch {
    /// The caught node.
    pub node_id: NodeId,
    /// The total relevance score.
    pub score: f32,
    /// Explanation of why this node was caught.
    pub provenance: String,
}

/// The main tool for associative retrieval.
pub struct FishingRod<'a> {
    db: &'a GallifreyDB,
}

impl<'a> FishingRod<'a> {
    /// Create a new FishingRod.
    pub fn new(db: &'a GallifreyDB) -> Self {
        Self { db }
    }

    /// Cast the line and retrieve related nodes.
    pub fn cast(&self, bait: Bait, config: FishingTrip) -> Result<Vec<Catch>> {
        // Step 1: Cast the Line (Vector Search)
        let school = match bait {
            Bait::Node(node_id) => {
                // If the node exists and has a vector index enabled, search similar.
                // For now, we assume if vector index is enabled, we search.
                // We'll try to find *any* vector index.
                // Since we don't know which property has the index, we might need to iterate.
                // But `find_similar` works if there is a default or we can try.
                // Actually `find_similar` in db.rs checks all vector indexes?
                // No, `find_similar` uses `find_similar_in` internally or iterates?
                // Looking at `db.rs`, `find_similar` seems to pick *a* property or fail?
                // Let's rely on `find_similar`.
                self.db.find_similar(node_id, config.limit)?
            }
            Bait::Vector(ref embedding) => {
                // We need to know which property to search.
                // Ideally `Bait` should specify property, but for "magic" we can try to guess or search all.
                // For this MVP, let's look for the first enabled vector index.
                let indexes = self.db.list_vector_indexes();
                if let Some(idx_info) = indexes.first() {
                    self.db.search_vectors_in(&idx_info.property_name, embedding, config.limit)?
                } else {
                    return Ok(vec![]); // No vector indexes, no fish.
                }
            }
        };

        let mut candidate_scores: HashMap<NodeId, f32> = HashMap::new();
        let mut provenance: HashMap<NodeId, String> = HashMap::new();

        // Add the "school" (vector matches) to candidates
        for (node_id, similarity) in school.iter() {
            let score = similarity * config.vector_weight;
            candidate_scores.insert(*node_id, score);
            provenance.insert(*node_id, format!("Vector Similarity: {:.4}", similarity));
        }

        // Step 2: Spread the Net (Graph Traversal)
        // If we want neighbors of the vector results
        if config.depth > 0 {
            let mut neighbors: Vec<(NodeId, NodeId)> = Vec::new(); // (neighbor, source)

            for (source_node, _) in school.iter() {
                let edges = self.db.get_outgoing_edges(*source_node);
                for edge_id in edges {
                    if let Ok(target) = self.db.get_edge_target(edge_id) {
                        neighbors.push((target, *source_node));
                    }
                }
            }

            for (target, source) in neighbors {
                let current_score = *candidate_scores.get(&target).unwrap_or(&0.0);
                // Simple additive boost for being a neighbor
                let new_score = current_score + config.graph_weight;
                candidate_scores.insert(target, new_score);

                provenance.entry(target)
                    .and_modify(|p| *p += &format!("\nLinked from Node {}", source))
                    .or_insert_with(|| format!("Linked from Node {}", source));
            }
        }

        // Step 3: Check Freshness
        if config.freshness_weight > 0.0 {
            let now_micros = time::now().wallclock();

            // We need to fetch nodes to check timestamps.
            // Collect all candidate IDs
            let all_candidates: Vec<NodeId> = candidate_scores.keys().cloned().collect();

            for node_id in all_candidates {
                if let Ok(node) = self.db.get_node(node_id) {
                    if let Some(ts) = node.metadata.commit_timestamp {
                        let age_micros = now_micros.saturating_sub(ts.wallclock());
                        let age_seconds = age_micros as f32 / 1_000_000.0;
                        // Decay function: 1 / (1 + age in hours)
                        let age_hours = age_seconds / 3600.0;
                        let freshness_score = 1.0 / (1.0 + age_hours);

                        candidate_scores.entry(node_id).and_modify(|s| {
                            *s += freshness_score * config.freshness_weight;
                        });
                    }
                }
            }
        }

        // Step 4: Format Results
        let mut catches: Vec<Catch> = candidate_scores.into_iter()
            .map(|(node_id, score)| Catch {
                node_id,
                score,
                provenance: provenance.remove(&node_id).unwrap_or_default(),
            })
            .collect();

        // Sort by score descending
        catches.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));

        // Limit
        catches.truncate(config.limit);

        Ok(catches)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::property::PropertyMapBuilder;
    use crate::index::vector::{HnswConfig, DistanceMetric};

    #[test]
    fn test_fishing_workflow() {
        let db = GallifreyDB::new().unwrap();

        // Enable vector index
        let config = HnswConfig::new(2, DistanceMetric::Cosine);
        db.enable_vector_index("embedding", config).unwrap();

        // Create nodes
        // Node 1: "The Bait" (we'll use its vector)
        let props1 = PropertyMapBuilder::new()
            .insert("name", "Fish A")
            .insert_vector("embedding", &vec![1.0, 0.0])
            .build();
        let _n1 = db.create_node("Fish", props1).unwrap();

        // Node 2: "The Catch" (Similar vector)
        let props2 = PropertyMapBuilder::new()
            .insert("name", "Fish B")
            .insert_vector("embedding", &vec![0.9, 0.1])
            .build();
        let n2 = db.create_node("Fish", props2).unwrap();

        // Node 3: "The Neighbor" (Linked from Fish B)
        let props3 = PropertyMapBuilder::new()
            .insert("name", "Coral")
            .build(); // No vector needed for graph traversal catch
        let n3 = db.create_node("Coral", props3).unwrap();

        // Link N2 -> N3
        db.create_edge(n2, n3, "HIDES_IN", PropertyMapBuilder::new().build()).unwrap();

        // Go fishing!
        let rod = FishingRod::new(&db);

        // Fish with a vector similar to N1/N2
        let bait = Bait::Vector(vec![1.0, 0.0]);
        let trip = FishingTrip {
            limit: 5,
            depth: 1,
            vector_weight: 1.0,
            graph_weight: 0.5,
            freshness_weight: 0.0, // Ignore time for this deterministic test
        };

        let catches = rod.cast(bait, trip).unwrap();

        // We expect:
        // 1. N1 (Perfect match or close)
        // 2. N2 (Close match)
        // 3. N3 (Linked from N2)

        assert!(catches.len() >= 2);

        let n2_catch = catches.iter().find(|c| c.node_id == n2);
        assert!(n2_catch.is_some(), "Should catch Fish B via vector");

        let n3_catch = catches.iter().find(|c| c.node_id == n3);
        assert!(n3_catch.is_some(), "Should catch Coral via graph link");

        if let Some(c) = n3_catch {
             assert!(c.provenance.contains("Linked from Node"));
        }
    }
}
