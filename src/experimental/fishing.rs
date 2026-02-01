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
use crate::core::temporal::time;
use crate::utils::{Error, Result};
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
    /// Optional edge labels to follow during graph expansion. If None, follows all edges.
    pub edge_labels: Option<Vec<String>>,
}

impl Default for FishingTrip {
    fn default() -> Self {
        Self {
            limit: 10,
            depth: 1,
            vector_weight: 1.0,
            graph_weight: 0.5,
            freshness_weight: 0.1,
            edge_labels: None,
        }
    }
}

/// The input to the fishing algorithm.
#[derive(Debug, Clone)]
pub enum Bait {
    /// Start with an existing node in the graph.
    /// Optionally specify which vector property to use for similarity search.
    /// If None, it will attempt to use the first available vector index or rely on default behavior.
    Node {
        /// The Node ID to start fishing from.
        id: NodeId,
        /// Optional vector property to use for similarity search.
        property: Option<String>,
    },
    /// Start with a raw embedding vector.
    /// Optionally specify the target property name.
    /// If None, it will use the first available vector index.
    Vector {
        /// The raw vector embedding to start fishing from.
        vector: Vec<f32>,
        /// Optional vector property to target.
        property: Option<String>,
    },
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
        if config.depth > 1 {
            return Err(Error::not_implemented(
                "Multi-hop traversal (depth > 1)",
                "Not yet implemented in FishingRod",
            ));
        }

        // Step 1: Cast the Line (Vector Search)
        let school = match bait {
            Bait::Node { id, property } => {
                if let Some(prop) = property {
                    self.db.find_similar_in(&prop, id, config.limit)?
                } else {
                    // Fallback to finding similar across any index (default behavior)
                    self.db.find_similar(id, config.limit)?
                }
            }
            Bait::Vector { vector, property } => {
                let property_name = if let Some(p) = property {
                    p
                } else {
                    // Auto-detect index
                    let indexes = self.db.list_vector_indexes();
                    if let Some(idx_info) = indexes.first() {
                        idx_info.property_name.clone()
                    } else {
                        return Err(crate::utils::error::Error::Vector(
                            crate::utils::error::VectorError::IndexError(
                                "No vector indexes configured. Call enable_vector_index() first."
                                    .to_string(),
                            ),
                        ));
                    }
                };
                self.db
                    .search_vectors_in(&property_name, &vector, config.limit)?
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
        if config.depth > 0 {
            let mut neighbors: Vec<(NodeId, NodeId)> = Vec::new(); // (neighbor, source)

            for (source_node, _) in school.iter() {
                let edges = if let Some(ref labels) = config.edge_labels {
                    let mut filtered_edges = Vec::new();
                    for label in labels {
                        filtered_edges
                            .extend(self.db.get_outgoing_edges_with_label(*source_node, label));
                    }
                    filtered_edges
                } else {
                    self.db.get_outgoing_edges(*source_node)
                };

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

                provenance
                    .entry(target)
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
                // Not collapsing if to keep it simple and compatible with potentially older rust versions
                // or just to be explicit. But clippy complained, so let's try to satisfy it without let_chains
                // if they are experimental (as per memory), but `if let && let` is stable?
                // Memory says: "The project targets stable Rust and does not support experimental features like let_chains (if let ... && ...). Use nested if statements or Option::is_some_and for complex conditionals."
                // So I CANNOT collapse it using `if let ... && let ...`.
                // I will suppress the lint.
                #[allow(clippy::collapsible_if)]
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
        let mut catches: Vec<Catch> = candidate_scores
            .into_iter()
            .map(|(node_id, score)| Catch {
                node_id,
                score,
                provenance: provenance.remove(&node_id).unwrap_or_default(),
            })
            .collect();

        // Sort by score descending
        // SAFETY: partial_cmp only returns None for NaN, which we handle with unwrap_or
        catches.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        // Limit
        catches.truncate(config.limit);

        Ok(catches)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::HnswConfig;
    use crate::PropertyMapBuilder;
    use crate::index::vector::DistanceMetric;

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
            .insert_vector("embedding", &[1.0, 0.0])
            .build();
        let _n1 = db.create_node("Fish", props1).unwrap();

        // Node 2: "The Catch" (Similar vector)
        let props2 = PropertyMapBuilder::new()
            .insert("name", "Fish B")
            .insert_vector("embedding", &[0.9, 0.1])
            .build();
        let n2 = db.create_node("Fish", props2).unwrap();

        // Node 3: "The Neighbor" (Linked from Fish B)
        let props3 = PropertyMapBuilder::new().insert("name", "Coral").build(); // No vector needed for graph traversal catch
        let n3 = db.create_node("Coral", props3).unwrap();

        // Link N2 -> N3
        db.create_edge(n2, n3, "HIDES_IN", PropertyMapBuilder::new().build())
            .unwrap();

        // Go fishing!
        let rod = FishingRod::new(&db);

        // Fish with a vector similar to N1/N2
        let bait = Bait::Vector {
            vector: vec![1.0, 0.0],
            property: Some("embedding".to_string()),
        };
        let trip = FishingTrip {
            limit: 5,
            depth: 1,
            vector_weight: 1.0,
            graph_weight: 0.5,
            freshness_weight: 0.0, // Ignore time for this deterministic test
            edge_labels: None,
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

    #[test]
    fn test_fishing_with_edge_label_filter() {
        let db = GallifreyDB::new().unwrap();

        // Enable vector index
        let config = HnswConfig::new(2, DistanceMetric::Cosine);
        db.enable_vector_index("embedding", config).unwrap();

        // Node 1: "The Bait"
        let props1 = PropertyMapBuilder::new()
            .insert("name", "Bait")
            .insert_vector("embedding", &[1.0, 0.0])
            .build();
        let n1 = db.create_node("Node", props1).unwrap();

        // Node 2: "The School" (Similar to Bait)
        let props2 = PropertyMapBuilder::new()
            .insert("name", "Similar")
            .insert_vector("embedding", &[0.99, 0.01])
            .build();
        let n2 = db.create_node("Node", props2).unwrap();

        // Node 3: Connected to School via "KNOWS"
        let n3 = db
            .create_node("Node", PropertyMapBuilder::new().build())
            .unwrap();
        db.create_edge(n2, n3, "KNOWS", PropertyMapBuilder::new().build())
            .unwrap();

        // Node 4: Connected to School via "HATES"
        let n4 = db
            .create_node("Node", PropertyMapBuilder::new().build())
            .unwrap();
        db.create_edge(n2, n4, "HATES", PropertyMapBuilder::new().build())
            .unwrap();

        let rod = FishingRod::new(&db);

        // Use Node 1 as bait.
        // It will find Node 2 (similar).
        // Then from Node 2, it will expand to neighbors.
        let bait = Bait::Node {
            id: n1,
            property: None,
        };

        // Filter only for "KNOWS" edges
        let trip = FishingTrip {
            limit: 10,
            depth: 1,
            vector_weight: 1.0,
            graph_weight: 1.0,
            freshness_weight: 0.0,
            edge_labels: Some(vec!["KNOWS".to_string()]),
        };

        let catches = rod.cast(bait, trip).unwrap();

        // Expectation:
        // - N2 is caught (Vector similarity)
        // - N3 is caught (Graph neighbor via KNOWS)
        // - N4 is NOT caught (Graph neighbor via HATES)
        // - N1 is NOT caught (Self is excluded from find_similar)

        assert!(
            catches.iter().any(|c| c.node_id == n2),
            "Should catch N2 (School)"
        );
        assert!(
            catches.iter().any(|c| c.node_id == n3),
            "Should catch N3 (KNOWS neighbor)"
        );
        assert!(
            !catches.iter().any(|c| c.node_id == n4),
            "Should NOT catch N4 (HATES neighbor)"
        );
    }

    #[test]
    fn test_fishing_no_vector_index_error() {
        let db = GallifreyDB::new().unwrap();
        // Do NOT enable vector index

        let rod = FishingRod::new(&db);
        let bait = Bait::Vector {
            vector: vec![1.0, 0.0],
            property: None,
        };
        let trip = FishingTrip::default();

        let result = rod.cast(bait, trip);
        assert!(result.is_err());

        if let Err(crate::utils::error::Error::Vector(
            crate::utils::error::VectorError::IndexError(msg),
        )) = result
        {
            assert!(msg.contains("No vector indexes configured"));
        } else {
            panic!("Expected IndexError when no vector index exists");
        }
    }

    #[test]
    fn test_fishing_freshness_weight() {
        // This test mostly verifies the code path runs without error.
        // Deterministic testing of time decay is tricky without mocking time,
        // but we can ensure it doesn't crash or behave illogically.
        let db = GallifreyDB::new().unwrap();
        let config = HnswConfig::new(2, DistanceMetric::Cosine);
        db.enable_vector_index("embedding", config).unwrap();

        let props = PropertyMapBuilder::new()
            .insert_vector("embedding", &[1.0, 0.0])
            .build();
        let _n1 = db.create_node("Node", props).unwrap();

        let rod = FishingRod::new(&db);
        let bait = Bait::Vector {
            vector: vec![1.0, 0.0],
            property: None,
        };
        let trip = FishingTrip {
            freshness_weight: 0.5,
            ..Default::default()
        };

        let result = rod.cast(bait, trip);
        assert!(result.is_ok());
    }
}
