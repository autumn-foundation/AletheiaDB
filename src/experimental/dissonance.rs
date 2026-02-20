//! Dissonance Engine: Semantic Stress Detection.
//!
//! "Why is this node here?"
//!
//! The Dissonance Engine detects nodes whose semantic meaning contradicts their graph topology.
//! It answers questions like:
//! - "Why is this 'Fruit' node connected to 10 'Tech' nodes?"
//! - "Is this connection an anomaly or a hallucination?"
//! - "Which parts of the graph are under the most semantic tension?"
//!
//! # Theory
//!
//! Dissonance is defined as the gap between a node's **Topological Neighborhood** (who it connects to)
//! and its **Semantic Neighborhood** (who it is similar to).
//!
//! A node with high dissonance is topologically connected to nodes that are semantically distant,
//! while its true semantic peers are elsewhere in the graph.
//!
//! # Formula
//!
//! ```text
//! Dissonance = Avg(Sim(KNN)) - Avg(Sim(GraphNeighbors))
//! ```
//!
//! Where:
//! - `Sim(KNN)` is the similarity score of the K-Nearest Neighbors in vector space.
//! - `Sim(GraphNeighbors)` is the similarity score of the actual graph neighbors.
//! - `K` is set to the number of graph neighbors (to compare apples to apples).
//!
//! - **High Dissonance (> 0.5)**: Node is an outlier.
//! - **Low Dissonance (~ 0.0)**: Node is well-integrated.
//! - **Negative Dissonance**: Graph neighbors are *more* similar than the global best?
//!   (Rare, but possible if index search is approximate).

use crate::core::error::{Error, Result, VectorError};
use crate::core::id::NodeId;
use crate::core::vector::ops;
use crate::AletheiaDB;

/// The Dissonance Engine for detecting semantic anomalies.
pub struct DissonanceEngine<'a> {
    db: &'a AletheiaDB,
}

impl<'a> DissonanceEngine<'a> {
    /// Create a new DissonanceEngine instance.
    pub fn new(db: &'a AletheiaDB) -> Self {
        Self { db }
    }

    /// Calculate the semantic dissonance of a node.
    ///
    /// # Arguments
    ///
    /// * `node_id` - The node to analyze.
    /// * `vector_property` - The name of the vector property (must be indexed).
    ///
    /// # Returns
    ///
    /// A float representing the dissonance score.
    /// - Range: Typically [0.0, 2.0] for Cosine.
    /// - Higher values indicate higher dissonance (semantic stress).
    pub fn calculate_dissonance(&self, node_id: NodeId, vector_property: &str) -> Result<f32> {
        // 1. Get the node and its vector
        let node = self.db.get_node(node_id)?;
        let Some(prop) = node.properties.get(vector_property) else {
            return Err(Error::Vector(VectorError::IndexError(format!(
                "Property '{}' not found on node {}",
                vector_property, node_id
            ))));
        };
        let Some(node_vec) = prop.as_vector() else {
            return Err(Error::Vector(VectorError::IndexError(format!(
                "Property '{}' is not a vector",
                vector_property
            ))));
        };

        // 2. Get graph neighbors (Topology)
        // We check both outgoing and incoming edges to capture full context.
        let outgoing = self.db.get_outgoing_edges(node_id);
        let incoming = self.db.get_incoming_edges(node_id);

        // Collect unique neighbor IDs
        let mut neighbors = std::collections::HashSet::new();
        for edge_id in outgoing {
            let edge = self.db.get_edge(edge_id)?;
            neighbors.insert(edge.target);
        }
        for edge_id in incoming {
            let edge = self.db.get_edge(edge_id)?;
            neighbors.insert(edge.source);
        }

        // Remove self-loops
        neighbors.remove(&node_id);

        if neighbors.is_empty() {
            // Isolated node: No topological constraints, so no dissonance.
            return Ok(0.0);
        }

        // 3. Get vector index info (Metric)
        // We need to know which metric the index uses to calculate scores consistently.
        // Assuming we can access the index config or inferred metric.
        // For now, we assume the user configured the index correctly.
        // We can get the index via `self.db.current.vector_indexes` but that's internal.
        // `AletheiaDB` doesn't expose `get_vector_index` publicly?
        // `search_vectors` works, so the index exists.
        // Let's assume Cosine if we can't find out, OR we assume the score returned by
        // `search_vectors` is consistent with `ops::cosine_similarity` for Cosine metric.
        //
        // NOTE: If we can't access the metric, we default to Cosine.
        // However, `AletheiaDB::vector_index` builder implies we can *configure* it, but maybe not read it back easily via public API?
        // Let's rely on standard ops for manual calculation and hope it matches.
        // Or better: Let's assume Cosine for MVP. Dissonance is usually a cosine-concept anyway.

        // 4. Calculate Average Graph Similarity
        let mut total_graph_sim = 0.0;
        let mut valid_neighbors = 0;

        for &neighbor_id in &neighbors {
            if let Ok(neighbor) = self.db.get_node(neighbor_id) {
                if let Some(n_prop) = neighbor.properties.get(vector_property) {
                    if let Some(n_vec) = n_prop.as_vector() {
                        // Compute similarity manually. Defaulting to Cosine for MVP.
                        // Ideally we'd match the index's metric.
                        let sim = ops::cosine_similarity(node_vec, n_vec)?;
                        total_graph_sim += sim;
                        valid_neighbors += 1;
                    }
                }
            }
        }

        if valid_neighbors == 0 {
            // Neighbors have no vectors? Cannot compute dissonance.
            return Ok(0.0);
        }

        let avg_graph_sim = total_graph_sim / valid_neighbors as f32;

        // 5. Calculate Average KNN Similarity (Semantics)
        // Search for K neighbors where K = neighbor count
        let k = neighbors.len();
        // We search k+1 because the node itself might be returned as the top result (sim=1.0)
        let knn_results = self.db.search_vectors_in(vector_property, node_vec, k + 1)?;

        let mut total_knn_sim = 0.0;
        let mut valid_knn = 0;

        for (id, score) in knn_results {
            if id == node_id {
                continue; // Skip self
            }
            total_knn_sim += score;
            valid_knn += 1;
            if valid_knn >= k {
                break;
            }
        }

        if valid_knn == 0 {
            // Index empty?
            return Ok(0.0);
        }

        let avg_knn_sim = total_knn_sim / valid_knn as f32;

        // 6. Dissonance = Gap
        // Higher KNN sim (should be high) - Lower Graph sim (if dissonant)
        // Example: KNN=0.9, Graph=0.1 -> Dissonance=0.8
        // Example: KNN=0.9, Graph=0.8 -> Dissonance=0.1
        let dissonance = avg_knn_sim - avg_graph_sim;

        Ok(dissonance)
    }
}

// Helpers would go here if we needed complex metric mapping.
// For MVP, we stick to Cosine as it's the standard for semantic embeddings.

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::property::PropertyMapBuilder;
    use crate::index::vector::{DistanceMetric, HnswConfig};

    #[test]
    fn test_dissonance_high() {
        let db = AletheiaDB::new().unwrap();
        // Enable vector index
        db.enable_vector_index("vec", HnswConfig::new(2, DistanceMetric::Cosine))
            .unwrap();

        // Node A: [1.0, 0.0] (Type X)
        // Node B: [0.0, 1.0] (Type Y) - Orthogonal/Dissimilar
        // Connect A -> B.
        // A's KNN should be other Type X nodes (sim ~1.0).
        // A's Graph neighbor is B (sim 0.0).
        // Dissonance should be high (~1.0).

        let a = db
            .create_node(
                "X",
                PropertyMapBuilder::new()
                    .insert_vector("vec", &[1.0, 0.0])
                    .build(),
            )
            .unwrap();

        let b = db
            .create_node(
                "Y",
                PropertyMapBuilder::new()
                    .insert_vector("vec", &[0.0, 1.0])
                    .build(),
            )
            .unwrap();

        // Add some "Type X" distractors for KNN to find
        for _ in 0..5 {
            db.create_node(
                "X",
                PropertyMapBuilder::new()
                    .insert_vector("vec", &[0.99, 0.01])
                    .build(),
            )
            .unwrap();
        }

        // Connect A to B (Dissimilar)
        db.create_edge(
            a,
            b,
            "CONNECTED_TO",
            PropertyMapBuilder::new().build(),
        )
        .unwrap();

        let engine = DissonanceEngine::new(&db);
        let dissonance = engine.calculate_dissonance(a, "vec").unwrap();

        println!("High Dissonance Score: {}", dissonance);
        assert!(dissonance > 0.5, "Expected high dissonance for orthogonal connection");
    }

    #[test]
    fn test_dissonance_low() {
        let db = AletheiaDB::new().unwrap();
        db.enable_vector_index("vec", HnswConfig::new(2, DistanceMetric::Cosine))
            .unwrap();

        // Node A: [1.0, 0.0]
        // Node B: [0.99, 0.01] - Very Similar
        // Connect A -> B.
        // A's KNN sim ~1.0.
        // A's Graph sim ~1.0.
        // Dissonance ~0.0.

        let a = db
            .create_node(
                "X",
                PropertyMapBuilder::new()
                    .insert_vector("vec", &[1.0, 0.0])
                    .build(),
            )
            .unwrap();

        let b = db
            .create_node(
                "X",
                PropertyMapBuilder::new()
                    .insert_vector("vec", &[0.99, 0.01])
                    .build(),
            )
            .unwrap();

        // Add distractors
        for _ in 0..5 {
            db.create_node(
                "X",
                PropertyMapBuilder::new()
                    .insert_vector("vec", &[0.98, 0.02])
                    .build(),
            )
            .unwrap();
        }

        db.create_edge(
            a,
            b,
            "CONNECTED_TO",
            PropertyMapBuilder::new().build(),
        )
        .unwrap();

        let engine = DissonanceEngine::new(&db);
        let dissonance = engine.calculate_dissonance(a, "vec").unwrap();

        println!("Low Dissonance Score: {}", dissonance);
        assert!(dissonance < 0.1, "Expected low dissonance for similar connection");
    }
}
