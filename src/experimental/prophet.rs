//! Prophet: Link Prediction Engine.
//!
//! This module implements a link prediction engine that suggests missing connections
//! in the graph based on topological structure (Adamic-Adar) and semantic similarity.
//!
//! # The Algorithm
//! Score(A, B) = AdamicAdar(A, B) * (1.0 + VectorSimilarity(A, B))

use crate::AletheiaDB;
use crate::core::error::Result;
use crate::core::id::NodeId;
use std::collections::HashSet;

/// The Prophet predicts the future (connections).
pub struct Prophet<'a> {
    db: &'a AletheiaDB,
    /// Optional vector property override.
    property_name: Option<String>,
}

impl<'a> Prophet<'a> {
    /// Create a new Prophet.
    pub fn new(db: &'a AletheiaDB) -> Self {
        Self {
            db,
            property_name: None,
        }
    }

    /// Set the property name to use for vector operations.
    pub fn with_property(mut self, name: impl Into<String>) -> Self {
        self.property_name = Some(name.into());
        self
    }

    /// Resolve the property name to use.
    fn get_property_name(&self) -> Result<String> {
        if let Some(ref name) = self.property_name {
            return Ok(name.clone());
        }

        let indexes = self.db.list_vector_indexes();
        if let Some(idx_info) = indexes.first() {
            Ok(idx_info.property_name.clone())
        } else {
            // It's okay if no vector index exists, we just won't use vector scoring.
            Ok("".to_string())
        }
    }

    /// Get neighbors of a node (undirected: outgoing U incoming).
    fn get_neighbors(&self, node_id: NodeId) -> Result<HashSet<NodeId>> {
        let out_edges = self.db.get_outgoing_edges(node_id);
        let in_edges = self.db.get_incoming_edges(node_id);

        let mut neighbors = HashSet::with_capacity(out_edges.len() + in_edges.len());

        for eid in out_edges {
            let target = self.db.get_edge_target(eid)?;
            neighbors.insert(target);
        }
        for eid in in_edges {
            let source = self.db.get_edge_source(eid)?;
            neighbors.insert(source);
        }

        Ok(neighbors)
    }

    /// Calculate Adamic-Adar score between two nodes.
    /// Sum(1 / log(degree(z))) for z in CommonNeighbors(x, y).
    fn adamic_adar(&self, neighbors_a: &HashSet<NodeId>, neighbors_b: &HashSet<NodeId>) -> f32 {
        let mut score = 0.0;
        for &neighbor in neighbors_a {
            if neighbors_b.contains(&neighbor) {
                // We use out_degree + in_degree as total degree proxy.
                let degree = self.db.out_degree(neighbor) + self.db.in_degree(neighbor);
                if degree > 1 {
                    score += 1.0 / (degree as f32).ln();
                }
            }
        }
        score
    }

    /// Get vector similarity between two nodes.
    fn vector_similarity(&self, a: NodeId, b: NodeId, property: &str) -> f32 {
        if property.is_empty() {
            return 0.0;
        }

        // Helper to get vector
        let get_vec = |id| -> Option<Vec<f32>> {
            self.db
                .get_node(id)
                .ok()?
                .properties
                .get(property)?
                .as_vector()
                .map(|v| v.to_vec())
        };

        let vec_a = match get_vec(a) {
            Some(v) => v,
            None => return 0.0,
        };
        let vec_b = match get_vec(b) {
            Some(v) => v,
            None => return 0.0,
        };

        // Cosine Similarity
        if vec_a.len() != vec_b.len() {
            return 0.0;
        }

        let dot: f32 = vec_a.iter().zip(vec_b.iter()).map(|(x, y)| x * y).sum();
        let mag_a: f32 = vec_a.iter().map(|x| x * x).sum::<f32>().sqrt();
        let mag_b: f32 = vec_b.iter().map(|x| x * x).sum::<f32>().sqrt();

        if mag_a == 0.0 || mag_b == 0.0 {
            0.0
        } else {
            dot / (mag_a * mag_b)
        }
    }

    /// Predict missing links for a target node.
    pub fn predict_links(&self, target: NodeId, k: usize) -> Result<Vec<(NodeId, f32)>> {
        let neighbors = self.get_neighbors(target)?;

        // 1. Gather candidates (Neighbors of Neighbors)
        // We exclude existing neighbors and self.
        let mut candidates = HashSet::new();
        for &neighbor in &neighbors {
            if let Ok(neighbor_neighbors) = self.get_neighbors(neighbor) {
                for &candidate in &neighbor_neighbors {
                    if candidate != target && !neighbors.contains(&candidate) {
                        candidates.insert(candidate);
                    }
                }
            }
        }

        // 2. Score candidates
        let property = self.get_property_name()?;
        let mut scored_candidates = Vec::with_capacity(candidates.len());

        for candidate in candidates {
            if let Ok(candidate_neighbors) = self.get_neighbors(candidate) {
                let topo_score = self.adamic_adar(&neighbors, &candidate_neighbors);
                let vec_score = self.vector_similarity(target, candidate, &property);

                // Final Score = Topo * (1 + Vec)
                // This boosts topologically relevant nodes if they are also semantically similar.
                let final_score = topo_score * (1.0 + vec_score);

                if final_score > 0.0 {
                    scored_candidates.push((candidate, final_score));
                }
            }
        }

        // 3. Sort and truncate
        scored_candidates
            .sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        if scored_candidates.len() > k {
            scored_candidates.truncate(k);
        }

        Ok(scored_candidates)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::property::PropertyMapBuilder;
    use crate::index::vector::{DistanceMetric, HnswConfig};

    #[test]
    fn test_prophet_diamond_prediction() {
        // Create a diamond-like structure where A and D share common neighbors B and C.
        // A -> B, A -> C
        // B -> D, C -> D
        // Prophet should predict A -> D.

        let db = AletheiaDB::new().unwrap();
        let props = PropertyMapBuilder::new().build();

        let a = db.create_node("Node", props.clone()).unwrap();
        let b = db.create_node("Node", props.clone()).unwrap();
        let c = db.create_node("Node", props.clone()).unwrap();
        let d = db.create_node("Node", props.clone()).unwrap();

        db.create_edge(a, b, "KNOWS", props.clone()).unwrap();
        db.create_edge(a, c, "KNOWS", props.clone()).unwrap();
        db.create_edge(b, d, "KNOWS", props.clone()).unwrap();
        db.create_edge(c, d, "KNOWS", props.clone()).unwrap();

        let prophet = Prophet::new(&db);
        let predictions = prophet.predict_links(a, 5).unwrap();

        // Should find D
        assert!(!predictions.is_empty());
        assert_eq!(predictions[0].0, d);
        assert!(predictions[0].1 > 0.0);
    }

    #[test]
    fn test_prophet_vector_boost() {
        // A -> B, A -> C
        // B -> D, C -> D (D is a topological candidate)
        // B -> E, C -> E (E is also a topological candidate, same score as D)
        //
        // But A and D have similar vectors, A and E do not.
        // D should score higher than E.

        let db = AletheiaDB::new().unwrap();
        let config = HnswConfig::new(2, DistanceMetric::Cosine);
        db.enable_vector_index("embedding", config).unwrap();

        // A: [1, 0]
        let p_a = PropertyMapBuilder::new()
            .insert_vector("embedding", &[1.0, 0.0])
            .build();
        let a = db.create_node("Node", p_a).unwrap();

        // B, C (Connectors)
        let p_x = PropertyMapBuilder::new().build();
        let b = db.create_node("Node", p_x.clone()).unwrap();
        let c = db.create_node("Node", p_x.clone()).unwrap();

        // D: [1, 0] (Similar to A)
        let p_d = PropertyMapBuilder::new()
            .insert_vector("embedding", &[1.0, 0.0])
            .build();
        let d = db.create_node("Node", p_d).unwrap();

        // E: [0, 1] (Orthogonal to A)
        let p_e = PropertyMapBuilder::new()
            .insert_vector("embedding", &[0.0, 1.0])
            .build();
        let e = db.create_node("Node", p_e).unwrap();

        // Edges
        db.create_edge(a, b, "KNOWS", p_x.clone()).unwrap();
        db.create_edge(a, c, "KNOWS", p_x.clone()).unwrap();

        db.create_edge(b, d, "KNOWS", p_x.clone()).unwrap();
        db.create_edge(c, d, "KNOWS", p_x.clone()).unwrap();

        db.create_edge(b, e, "KNOWS", p_x.clone()).unwrap();
        db.create_edge(c, e, "KNOWS", p_x.clone()).unwrap();

        let prophet = Prophet::new(&db);
        let predictions = prophet.predict_links(a, 5).unwrap();

        assert!(predictions.len() >= 2);

        let d_score = predictions
            .iter()
            .find(|(id, _)| *id == d)
            .map(|(_, s)| *s)
            .unwrap();
        let e_score = predictions
            .iter()
            .find(|(id, _)| *id == e)
            .map(|(_, s)| *s)
            .unwrap();

        assert!(
            d_score > e_score,
            "D should rank higher than E due to vector similarity (D: {}, E: {})",
            d_score,
            e_score
        );
    }
}
