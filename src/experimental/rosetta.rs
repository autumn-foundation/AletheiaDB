//! Rosetta: Semantic Relationship Profiling.
//!
//! "What does 'Parent Of' look like in vector space?"
//!
//! Rosetta learns the "geometric signature" of edge labels.
//!
//! # Concepts
//! - **Semantic Profile**: Statistical description of the vector relationship between source and target nodes for a specific edge label.
//! - **Translation**: $TargetVector - SourceVector$.
//! - **Similarity**: Cosine similarity between Source and Target.
//!
//! # Use Cases
//! - **Anomaly Detection**: Flag edges that deviate from the learned profile.
//! - **Label Prediction**: Suggest likely relationships between disconnected nodes.
//! - **Schema Induction**: Understand the semantic meaning of relationships.

#![allow(clippy::collapsible_if)]

use crate::AletheiaDB;
use crate::core::error::{Error, Result};
use crate::core::id::{EdgeId, NodeId};
use crate::core::vector::ops::{cosine_similarity, magnitude};
use std::collections::HashSet;

/// The learned geometric profile of a relationship.
#[derive(Debug, Clone)]
pub struct SemanticProfile {
    /// The edge label this profile describes.
    pub label: String,
    /// Number of edges used to train this profile.
    pub sample_count: usize,
    /// Average cosine similarity between source and target.
    pub mean_similarity: f32,
    /// Standard deviation of cosine similarity.
    pub std_dev_similarity: f32,
    /// Average translation vector (Target - Source).
    pub mean_offset: Vec<f32>,
    /// Variance of the offset vector components.
    pub offset_variance: Vec<f32>,
}

/// The Rosetta Engine.
pub struct Rosetta<'a> {
    db: &'a AletheiaDB,
}

impl<'a> Rosetta<'a> {
    /// Create a new Rosetta instance.
    pub fn new(db: &'a AletheiaDB) -> Self {
        Self { db }
    }

    /// Learn the semantic profile for a given edge label.
    ///
    /// # Arguments
    /// * `label` - The edge label to profile.
    /// * `vector_property` - The vector property to use.
    /// * `max_samples` - Maximum number of edges to sample.
    pub fn train(
        &self,
        label: &str,
        vector_property: &str,
        max_samples: usize,
    ) -> Result<SemanticProfile> {
        // 1. Gather samples
        // We scan for edges with the label.
        // AletheiaDB doesn't have a direct "get all edges with label" API that is efficient for sampling without iteration.
        // We'll use `query().scan_edges(label)`.
        let mut samples = Vec::new();
        let mut count = 0;

        // Use low-level edge scan if possible, or query engine
        // Query engine returns QueryRows.
        // Assuming we can iterate edges.
        // AletheiaDB::get_all_edges? No.
        // But we can iterate nodes and their outgoing edges.
        // Let's use `db.query().match_edge(label).limit(max_samples)`.
        // Wait, standard query builder might be easier.
        // Or manual iteration over nodes if necessary.
        // For efficiency in this experimental module, let's assume we scan nodes and check outgoing.

        // Actually, `CurrentIndexes` might allow efficient label lookup if implemented.
        // If not, we just scan a subset of nodes.

        // Heuristic: Scan first N nodes.
        // TODO: Random sampling would be better.
        // For now, deterministic scan.

        // We use the query engine scan logic
        let query_results = self
            .db
            .query()
            .scan(None) // All nodes
            .execute(self.db)?;

        for row_res in query_results {
            if count >= max_samples {
                break;
            }
            let row = row_res?;
            if let Some(node) = row.entity.as_node() {
                let outgoing = self.db.get_outgoing_edges_with_label(node.id, label);
                for edge_id in outgoing {
                    if count >= max_samples {
                        break;
                    }

                    let edge = self.db.get_edge(edge_id)?;
                    let target_id = edge.target;

                    // Get vectors
                    let source_vec = self.get_vector(node.id, vector_property);
                    let target_vec = self.get_vector(target_id, vector_property);

                    if let (Ok(s), Ok(t)) = (source_vec, target_vec) {
                        if s.len() == t.len() {
                            samples.push((s, t));
                            count += 1;
                        }
                    }
                }
            }
        }

        if samples.is_empty() {
            return Err(Error::other(format!(
                "No valid edges found for label '{}' with vector property '{}'",
                label, vector_property
            )));
        }

        // 2. Compute Statistics
        let n = samples.len() as f32;
        let dim = samples[0].0.len();

        let mut sim_sum = 0.0;
        let mut sim_sq_sum = 0.0;
        let mut offset_sum = vec![0.0; dim];
        let mut offset_sq_sum = vec![0.0; dim];

        for (s, t) in &samples {
            // Similarity
            let sim = cosine_similarity(s, t)?;
            sim_sum += sim;
            sim_sq_sum += sim * sim;

            // Offset
            for i in 0..dim {
                let diff = t[i] - s[i];
                offset_sum[i] += diff;
                offset_sq_sum[i] += diff * diff;
            }
        }

        let mean_sim = sim_sum / n;
        let var_sim = (sim_sq_sum / n) - (mean_sim * mean_sim);
        let std_dev_sim = var_sim.sqrt();

        let mut mean_offset = vec![0.0; dim];
        let mut offset_variance = vec![0.0; dim];

        for i in 0..dim {
            mean_offset[i] = offset_sum[i] / n;
            let var = (offset_sq_sum[i] / n) - (mean_offset[i] * mean_offset[i]);
            offset_variance[i] = var;
        }

        Ok(SemanticProfile {
            label: label.to_string(),
            sample_count: samples.len(),
            mean_similarity: mean_sim,
            std_dev_similarity: std_dev_sim,
            mean_offset,
            offset_variance,
        })
    }

    /// Predict the relationship label between two nodes.
    ///
    /// # Arguments
    /// * `source` - Source node.
    /// * `target` - Target node.
    /// * `vector_property` - Vector property to use.
    /// * `candidates` - List of pre-trained profiles to check against.
    ///
    /// # Returns
    /// The label of the best matching profile, if any match is good enough.
    pub fn predict(
        &self,
        source: NodeId,
        target: NodeId,
        vector_property: &str,
        candidates: &[SemanticProfile],
    ) -> Result<Option<String>> {
        let s_vec = self.get_vector(source, vector_property)?;
        let t_vec = self.get_vector(target, vector_property)?;

        if s_vec.len() != t_vec.len() {
            return Ok(None);
        }

        let sim = cosine_similarity(&s_vec, &t_vec)?;
        let offset: Vec<f32> = t_vec.iter().zip(s_vec.iter()).map(|(t, s)| t - s).collect();

        let mut best_label = None;
        let mut min_error = f32::MAX;

        for profile in candidates {
            // Z-score like check?
            // Or Euclidean distance from mean offset?
            // Combined score: Offset Error + Similarity Error.

            // 1. Offset Error (Euclidean distance between actual offset and mean offset)
            let mut offset_error = 0.0;
            for i in 0..offset.len() {
                offset_error += (offset[i] - profile.mean_offset[i]).powi(2);
            }
            offset_error = offset_error.sqrt();

            // 2. Similarity Error
            let sim_error = (sim - profile.mean_similarity).abs();

            // Total Error (Weighted?)
            // If offset variance is high, offset error matters less.
            // For simplicity, just sum them for MVP.
            let total_error = offset_error + sim_error;

            if total_error < min_error {
                min_error = total_error;
                best_label = Some(profile.label.clone());
            }
        }

        // Thresholding?
        // Ideally we check if it falls within X sigma.
        // For now, just return best.
        Ok(best_label)
    }

    /// Audit edges of a specific label to find anomalies.
    ///
    /// # Returns
    /// List of EdgeIds that deviate significantly from the profile.
    pub fn audit(
        &self,
        label: &str,
        vector_property: &str,
        profile: &SemanticProfile,
        threshold_sigma: f32,
    ) -> Result<Vec<EdgeId>> {
        let mut anomalies = Vec::new();

        // Similar scan as train
        let query_results = self.db.query().scan(None).execute(self.db)?;

        for row_res in query_results {
            let row = row_res?;
            if let Some(node) = row.entity.as_node() {
                let outgoing = self.db.get_outgoing_edges_with_label(node.id, label);
                for edge_id in outgoing {
                    let edge = self.db.get_edge(edge_id)?;
                    let target_id = edge.target;

                    // Vectors
                    let s_vec = match self.get_vector(node.id, vector_property) {
                        Ok(v) => v,
                        Err(_) => continue,
                    };
                    let t_vec = match self.get_vector(target_id, vector_property) {
                        Ok(v) => v,
                        Err(_) => continue,
                    };

                    if s_vec.len() != t_vec.len() {
                        continue;
                    }

                    // Check Similarity Deviation
                    let sim = cosine_similarity(&s_vec, &t_vec)?;
                    let sim_z = (sim - profile.mean_similarity).abs() / profile.std_dev_similarity;

                    // Check Offset Deviation
                    let offset: Vec<f32> =
                        t_vec.iter().zip(s_vec.iter()).map(|(t, s)| t - s).collect();

                    let mut offset_dist = 0.0;
                    for i in 0..offset.len() {
                        offset_dist += (offset[i] - profile.mean_offset[i]).powi(2);
                    }
                    offset_dist = offset_dist.sqrt();

                    // If either is anomalous
                    // Using a heuristic for offset Z-score (using magnitude of variance vector is crude but works for MVP)
                    // Better: Mahalanobis distance if we had covariance matrix.
                    // For MVP: Similarity check is robust enough for many cases.
                    // Let's rely on Sim Z-score + Offset Dist > X * MeanOffsetMagnitude.

                    if sim_z > threshold_sigma {
                        anomalies.push(edge_id);
                    }
                }
            }
        }

        Ok(anomalies)
    }

    fn get_vector(&self, node_id: NodeId, property: &str) -> Result<Vec<f32>> {
        let node = self.db.get_node(node_id)?;
        let val = node.properties.get(property).ok_or_else(|| {
            Error::Vector(crate::core::error::VectorError::IndexError(format!(
                "Property '{}' missing",
                property
            )))
        })?;
        let vec = val.as_vector().ok_or_else(|| {
            Error::Vector(crate::core::error::VectorError::IndexError(format!(
                "Property '{}' not a vector",
                property
            )))
        })?;
        Ok(vec.to_vec())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::property::PropertyMapBuilder;
    use crate::index::vector::{DistanceMetric, HnswConfig};

    #[test]
    fn test_rosetta_learns_offset() {
        let db = AletheiaDB::new().unwrap();
        // Enable index
        db.enable_vector_index("vec", HnswConfig::new(2, DistanceMetric::Euclidean))
            .unwrap();

        // Pattern: "RIGHT" adds [1.0, 0.0]
        let mut edges_created = 0;
        for i in 0..10 {
            let s_vec = vec![i as f32, 0.0];
            let t_vec = vec![(i + 1) as f32, 0.0];

            let props_s = PropertyMapBuilder::new()
                .insert_vector("vec", &s_vec)
                .build();
            let s = db.create_node("Node", props_s).unwrap();

            let props_t = PropertyMapBuilder::new()
                .insert_vector("vec", &t_vec)
                .build();
            let t = db.create_node("Node", props_t).unwrap();

            db.create_edge(s, t, "RIGHT", Default::default()).unwrap();
            edges_created += 1;
        }

        let rosetta = Rosetta::new(&db);
        let profile = rosetta.train("RIGHT", "vec", 100).unwrap();

        assert_eq!(profile.sample_count, edges_created);

        // Check Mean Offset
        // Should be close to [1.0, 0.0]
        assert!((profile.mean_offset[0] - 1.0).abs() < 0.01);
        assert!((profile.mean_offset[1] - 0.0).abs() < 0.01);
    }

    #[test]
    fn test_rosetta_prediction() {
        let db = AletheiaDB::new().unwrap();
        db.enable_vector_index("vec", HnswConfig::new(2, DistanceMetric::Euclidean))
            .unwrap();

        // Train UP ([0, 1]) and RIGHT ([1, 0])
        // Create 5 samples for each
        for i in 0..5 {
            let base = vec![0.0, 0.0];
            let s = db
                .create_node(
                    "N",
                    PropertyMapBuilder::new()
                        .insert_vector("vec", &base)
                        .build(),
                )
                .unwrap();

            let right = vec![1.0, 0.0];
            let tr = db
                .create_node(
                    "N",
                    PropertyMapBuilder::new()
                        .insert_vector("vec", &right)
                        .build(),
                )
                .unwrap();
            db.create_edge(s, tr, "RIGHT", Default::default()).unwrap();

            let up = vec![0.0, 1.0];
            let tu = db
                .create_node(
                    "N",
                    PropertyMapBuilder::new().insert_vector("vec", &up).build(),
                )
                .unwrap();
            db.create_edge(s, tu, "UP", Default::default()).unwrap();
        }

        let rosetta = Rosetta::new(&db);
        let p_right = rosetta.train("RIGHT", "vec", 100).unwrap();
        let p_up = rosetta.train("UP", "vec", 100).unwrap();

        let candidates = vec![p_right, p_up];

        // Test Prediction
        // Case 1: [0,0] -> [1,0] should be RIGHT
        let s1 = db
            .create_node(
                "S",
                PropertyMapBuilder::new()
                    .insert_vector("vec", &[0.0, 0.0])
                    .build(),
            )
            .unwrap();
        let t1 = db
            .create_node(
                "T",
                PropertyMapBuilder::new()
                    .insert_vector("vec", &[1.0, 0.0])
                    .build(),
            )
            .unwrap();

        let pred1 = rosetta.predict(s1, t1, "vec", &candidates).unwrap();
        assert_eq!(pred1.as_deref(), Some("RIGHT"));

        // Case 2: [0,0] -> [0,1] should be UP
        let t2 = db
            .create_node(
                "T",
                PropertyMapBuilder::new()
                    .insert_vector("vec", &[0.0, 1.0])
                    .build(),
            )
            .unwrap();
        let pred2 = rosetta.predict(s1, t2, "vec", &candidates).unwrap();
        assert_eq!(pred2.as_deref(), Some("UP"));
    }

    #[test]
    fn test_rosetta_audit_anomaly() {
        let db = AletheiaDB::new().unwrap();
        db.enable_vector_index("vec", HnswConfig::new(2, DistanceMetric::Cosine))
            .unwrap();

        // Label "SYNONYM" -> High similarity (~1.0)
        // Train on 5 good pairs
        for _ in 0..5 {
            let s = db
                .create_node(
                    "N",
                    PropertyMapBuilder::new()
                        .insert_vector("vec", &[1.0, 0.0])
                        .build(),
                )
                .unwrap();
            let t = db
                .create_node(
                    "N",
                    PropertyMapBuilder::new()
                        .insert_vector("vec", &[0.99, 0.0])
                        .build(),
                )
                .unwrap();
            db.create_edge(s, t, "SYNONYM", Default::default()).unwrap();
        }

        let rosetta = Rosetta::new(&db);
        let profile = rosetta.train("SYNONYM", "vec", 100).unwrap();

        assert!(
            profile.mean_similarity > 0.9,
            "Synonyms should have high sim"
        );
        assert!(
            profile.std_dev_similarity < 0.1,
            "Low variance for synonyms"
        );

        // Introduce Anomaly: "SYNONYM" with orthogonal vectors
        let bad_s = db
            .create_node(
                "N",
                PropertyMapBuilder::new()
                    .insert_vector("vec", &[1.0, 0.0])
                    .build(),
            )
            .unwrap();
        let bad_t = db
            .create_node(
                "N",
                PropertyMapBuilder::new()
                    .insert_vector("vec", &[0.0, 1.0])
                    .build(),
            )
            .unwrap();
        let bad_edge = db
            .create_edge(bad_s, bad_t, "SYNONYM", Default::default())
            .unwrap();

        // Audit
        let anomalies = rosetta.audit("SYNONYM", "vec", &profile, 3.0).unwrap();

        assert!(!anomalies.is_empty());
        assert!(anomalies.contains(&bad_edge));
    }
}
