//! Muse: The Semantic Ideator.
//!
//! "Innovation is connecting two existing modules that haven't met yet."
//!
//! Muse explores the "conceptual voids" in the vector space between existing nodes.
//! It calculates the centroid of a set of nodes and checks if that space is empty.
//! If it is, it proposes a "Creative Leap" - a new concept that bridges the gap.
//!
//! # Concepts
//! - **Centroid**: The geometric center of the input vectors.
//! - **Void Score**: A measure of how "empty" the space around the centroid is.
//!   High score = High novelty (no existing concepts nearby).
//! - **Inspiration**: The proposed new concept.

use crate::AletheiaDB;
use crate::core::error::Result;
use crate::core::id::NodeId;

/// A proposed creative leap or new concept.
#[derive(Debug, Clone)]
pub struct Inspiration {
    /// The vector representation of the new concept (centroid).
    pub centroid: Vec<f32>,
    /// Existing nodes that are nearest to this new concept (context).
    pub nearby_concepts: Vec<(NodeId, f32)>,
    /// How "empty" the space is (0.0 = crowded, 1.0 = empty).
    /// Calculated as `1.0 - max(similarity of nearest neighbor)`.
    pub novelty_score: f32,
    /// How well the centroid connects the input nodes.
    /// Average similarity to input nodes.
    pub coherence_score: f32,
}

/// The Muse Engine.
pub struct Muse<'a> {
    db: &'a AletheiaDB,
}

#[cfg(feature = "nova")]
impl<'a> Muse<'a> {
    /// Create a new Muse instance.
    pub fn new(db: &'a AletheiaDB) -> Self {
        Self { db }
    }

    /// Inspire a new concept based on the input nodes.
    ///
    /// # Arguments
    /// * `nodes` - The source nodes to combine.
    /// * `property` - The vector property to use (optional, defaults to first available).
    /// * `limit` - Max number of nearby concepts to return for context.
    pub fn inspire(
        &self,
        nodes: &[NodeId],
        property: Option<&str>,
        limit: usize,
    ) -> Result<Option<Inspiration>> {
        if nodes.is_empty() {
            return Ok(None);
        }

        // 1. Resolve Property Name
        let prop_name = if let Some(p) = property {
            p.to_string()
        } else {
            let indexes = self.db.list_vector_indexes();
            if let Some(info) = indexes.first() {
                info.property_name.clone()
            } else {
                return Ok(None); // No vector indexes
            }
        };

        // 2. Fetch Vectors
        let mut vectors = Vec::with_capacity(nodes.len());
        for &node_id in nodes {
            let node = self.db.get_node(node_id)?;
            if let Some(val) = node.properties.get(&prop_name) {
                if let Some(vec) = val.as_vector() {
                    vectors.push(vec.to_vec());
                }
            }
        }

        if vectors.is_empty() {
            return Ok(None);
        }

        // 3. Calculate Centroid
        let dim = vectors[0].len();
        let mut centroid = vec![0.0; dim];
        for vec in &vectors {
            if vec.len() != dim {
                continue; // Skip mismatched dimensions
            }
            for (i, val) in vec.iter().enumerate() {
                centroid[i] += val;
            }
        }

        // Normalize (Average then Normalize to unit length)
        // Dividing by count is uniform scaling, so just normalize is enough for direction.
        // We rely on `normalize` from core.
        let centroid = crate::core::vector::ops::normalize(&centroid);

        // 4. Search for Neighbors (Context)
        // We search for `limit` neighbors to provide context.
        let neighbors = self.db.search_vectors_in(&prop_name, &centroid, limit)?;

        // 5. Calculate Scores
        let max_sim = neighbors.first().map(|(_, score)| *score).unwrap_or(-1.0);

        // Novelty: 1.0 - max_sim.
        // If max_sim is 1.0 (exact match), novelty is 0.0.
        // If max_sim is 0.0 (orthogonal), novelty is 1.0.
        let novelty_score = 1.0 - max_sim.max(0.0);

        // Coherence: Average similarity to input nodes.
        let mut total_sim = 0.0;
        let mut count = 0;
        for vec in &vectors {
            if let Ok(sim) = crate::core::vector::ops::cosine_similarity(&centroid, vec) {
                total_sim += sim;
                count += 1;
            }
        }
        let coherence_score = if count > 0 {
            total_sim / count as f32
        } else {
            0.0
        };

        Ok(Some(Inspiration {
            centroid,
            nearby_concepts: neighbors,
            novelty_score,
            coherence_score,
        }))
    }
}

#[cfg(not(feature = "nova"))]
impl<'a> Muse<'a> {
    /// Create a new Muse instance.
    pub fn new(_db: &'a AletheiaDB) -> Self {
        panic!(
            "Experimental feature 'Muse' requires the 'nova' feature. Please enable it in Cargo.toml."
        );
    }

    /// Internal constructor for testing panic behavior.
    #[cfg(test)]
    fn new_internal(db: &'a AletheiaDB) -> Self {
        Self { db }
    }

    /// Inspire a new concept based on the input nodes.
    pub fn inspire(
        &self,
        _nodes: &[NodeId],
        _property: Option<&str>,
        _limit: usize,
    ) -> Result<Option<Inspiration>> {
        panic!(
            "Experimental feature 'Muse' requires the 'nova' feature. Please enable it in Cargo.toml."
        );
    }
}

#[cfg(all(test, feature = "nova"))]
mod tests {
    use super::*;
    use crate::core::property::PropertyMapBuilder;
    use crate::index::vector::{DistanceMetric, HnswConfig};

    #[test]
    fn test_muse_finds_void() {
        let db = AletheiaDB::new().unwrap();
        let config = HnswConfig::new(2, DistanceMetric::Cosine);
        db.enable_vector_index("vec", config).unwrap();

        // Node A: [1, 0]
        let props_a = PropertyMapBuilder::new()
            .insert_vector("vec", &[1.0, 0.0])
            .build();
        let a = db.create_node("Node", props_a).unwrap();

        // Node B: [0, 1]
        let props_b = PropertyMapBuilder::new()
            .insert_vector("vec", &[0.0, 1.0])
            .build();
        let b = db.create_node("Node", props_b).unwrap();

        // Centroid should be [0.707, 0.707] (Normalized [0.5, 0.5])

        let muse = Muse::new(&db);
        let inspiration = muse.inspire(&[a, b], None, 5).unwrap().unwrap();

        // Check Centroid
        let c = &inspiration.centroid;
        assert!((c[0] - 0.707).abs() < 0.01);
        assert!((c[1] - 0.707).abs() < 0.01);

        // Check Novelty
        // No other nodes exist, so nearest neighbors (A and B) have sim ~0.707
        // Novelty should be around 1.0 - 0.707 = 0.293
        assert!(inspiration.novelty_score > 0.2);
        assert!(inspiration.novelty_score < 0.4);

        // Check Coherence
        // Sim(Centroid, A) = 0.707
        // Sim(Centroid, B) = 0.707
        // Avg = 0.707
        assert!((inspiration.coherence_score - 0.707).abs() < 0.01);
    }

    #[test]
    fn test_muse_finds_crowded_space() {
        let db = AletheiaDB::new().unwrap();
        let config = HnswConfig::new(2, DistanceMetric::Cosine);
        db.enable_vector_index("vec", config).unwrap();

        // Node A: [1, 0]
        let props_a = PropertyMapBuilder::new()
            .insert_vector("vec", &[1.0, 0.0])
            .build();
        let a = db.create_node("Node", props_a).unwrap();

        // Node B: [0, 1]
        let props_b = PropertyMapBuilder::new()
            .insert_vector("vec", &[0.0, 1.0])
            .build();
        let b = db.create_node("Node", props_b).unwrap();

        // Node C: [0.707, 0.707] (Already exists in the gap)
        let props_c = PropertyMapBuilder::new()
            .insert_vector("vec", &[0.707, 0.707])
            .build();
        let _c = db.create_node("Node", props_c).unwrap();

        let muse = Muse::new(&db);
        // Inspire from A and B
        let inspiration = muse.inspire(&[a, b], None, 5).unwrap().unwrap();

        // Centroid matches C
        // Nearest neighbor is C (sim = 1.0)
        // Novelty should be 0.0
        assert!(inspiration.novelty_score < 0.01);
    }
}

#[cfg(all(test, not(feature = "nova")))]
mod stub_tests {
    use super::*;
    use crate::AletheiaDB;

    #[test]
    #[should_panic(expected = "Experimental feature 'Muse' requires the 'nova' feature")]
    fn test_stub_new_panics() {
        let db = AletheiaDB::new().unwrap();
        let _ = Muse::new(&db);
    }

    #[test]
    #[should_panic(expected = "Experimental feature 'Muse' requires the 'nova' feature")]
    fn test_stub_inspire_panics() {
        let db = AletheiaDB::new().unwrap();
        // Use internal constructor to bypass new's panic
        let muse = Muse::new_internal(&db);
        let _ = muse.inspire(&[], None, 0);
    }
}
