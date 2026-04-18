//! Glitch: Semantic Chaos Monkey 🐒
//!
//! "What happens to our reasoning when concepts are slightly perturbed?"
//!
//! Glitch injects controlled anomalies into the vector embeddings of nodes.
//! It is a chaos engineering tool designed to test the robustness of semantic
//! search, clustering, and reasoning engines. By slightly altering the vectors,
//! we can see if our systems are overly brittle to minor semantic variations.
//!
//! # Concepts
//! - **Noise Injection**: Adds random gaussian-like noise to a vector.
//! - **Dropout**: Randomly zeroes out certain dimensions.
//! - **Inversion**: Flips the vector's direction (semantic opposite).

#![allow(clippy::collapsible_if)]

use crate::AletheiaDB;
use crate::api::transaction::WriteOps;
use crate::core::error::{Error, Result, VectorError};
use crate::core::id::NodeId;
use crate::core::property::PropertyMapBuilder;
use rand::Rng;

/// Configuration for the Glitch Engine.
#[derive(Debug, Clone)]
pub struct GlitchConfig {
    /// The magnitude of noise to inject (0.0 = no noise, 1.0 = massive noise).
    pub noise_level: f32,
    /// The probability (0.0 to 1.0) of a dimension being zeroed out.
    pub dropout_rate: f32,
    /// If true, the vector will be inverted (multiplied by -1).
    pub invert: bool,
}

impl Default for GlitchConfig {
    fn default() -> Self {
        Self {
            noise_level: 0.05,
            dropout_rate: 0.0,
            invert: false,
        }
    }
}

/// The Glitch Engine.
pub struct GlitchEngine<'a> {
    db: &'a AletheiaDB,
}

#[cfg(feature = "nova")]
impl<'a> GlitchEngine<'a> {
    /// Create a new Glitch Engine.
    pub fn new(db: &'a AletheiaDB) -> Self {
        Self { db }
    }

    /// Apply a glitch to a specific node's vector property.
    pub fn glitch_node(
        &self,
        node_id: NodeId,
        property: &str,
        config: &GlitchConfig,
    ) -> Result<()> {
        let mut rng = rand::thread_rng();
        self.glitch_node_with_rng(node_id, property, config, &mut rng)
    }

    /// Apply a glitch to a specific node's vector property using a provided RNG.
    pub fn glitch_node_with_rng<R: Rng>(
        &self,
        node_id: NodeId,
        property: &str,
        config: &GlitchConfig,
        rng: &mut R,
    ) -> Result<()> {
        let node = self.db.get_node(node_id)?;
        let mut vec = match node
            .properties
            .get(property)
            .and_then(|p| p.as_vector())
        {
            Some(v) => v.to_vec(),
            None => {
                return Err(Error::Vector(VectorError::IndexError(format!(
                    "Node {} does not have vector property '{}'",
                    node_id, property
                ))));
            }
        };

        for v in &mut vec {
            // 1. Inversion
            if config.invert {
                *v = -*v;
            }

            // 2. Dropout
            if config.dropout_rate > 0.0 && rng.r#gen::<f32>() < config.dropout_rate {
                *v = 0.0;
            }
            // 3. Noise Injection
            else if config.noise_level > 0.0 {
                // Simple uniform noise scaled by noise_level
                let noise: f32 = rng.r#gen_range(-1.0..1.0) * config.noise_level;
                *v += noise;
            }
        }

        // Re-normalize if needed? For cosine, we might want to keep it normalized,
        // but a true glitch might break normalization! Let's re-normalize to keep it
        // comparable in standard semantic spaces, unless inversion/dropout zeroes it.
        let norm: f32 = vec.iter().map(|x| x * x).sum::<f32>().sqrt();
        if norm > 1e-6 {
            for v in vec.iter_mut() {
                *v /= norm;
            }
        }

        self.db.write(|tx| {
            // Re-insert ALL existing properties to prevent data loss when glitching
            let mut builder = PropertyMapBuilder::new();
            for (key, val) in node.properties.iter() {
                builder = builder.try_insert_by_key(*key, val.clone())?;
            }
            // Overwrite the vector
            builder = builder.try_insert_vector(property, &vec)?;
            tx.update_node(node_id, builder.build())
        })?;

        Ok(())
    }
}

#[cfg(all(test, feature = "nova"))]
mod tests {
    use super::*;
    use crate::index::vector::{DistanceMetric, HnswConfig};
    use rand::SeedableRng;
    use rand::rngs::StdRng;

    #[test]
    fn test_glitch_noise_injection() {
        let db = AletheiaDB::new().unwrap();
        db.enable_vector_index("vec", HnswConfig::new(2, DistanceMetric::Cosine))
            .unwrap();

        let props = PropertyMapBuilder::new()
            .insert_vector("vec", &[1.0, 0.0])
            .insert("other", 123)
            .build();
        let node = db.create_node("Concept", props).unwrap();

        let engine = GlitchEngine::new(&db);
        let config = GlitchConfig {
            noise_level: 0.5, // High noise
            ..Default::default()
        };

        let mut rng = StdRng::seed_from_u64(42);
        engine.glitch_node_with_rng(node, "vec", &config, &mut rng).unwrap();

        let updated = db.get_node(node).unwrap();
        let new_vec = updated.properties.get("vec").unwrap().as_vector().unwrap();

        // Should still be normalized
        let norm: f32 = new_vec.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 0.01);

        // Y should no longer be exactly 0 due to noise
        assert!(new_vec[1].abs() > 0.01);

        // Ensure other properties were not deleted
        assert_eq!(updated.properties.get("other").unwrap().as_int(), Some(123));
    }

    #[test]
    fn test_glitch_dropout() {
        let db = AletheiaDB::new().unwrap();
        db.enable_vector_index("vec", HnswConfig::new(2, DistanceMetric::Cosine))
            .unwrap();

        let props = PropertyMapBuilder::new()
            .insert_vector("vec", &[0.5, 0.5])
            .build();
        let node = db.create_node("Concept", props).unwrap();

        let engine = GlitchEngine::new(&db);
        let config = GlitchConfig {
            dropout_rate: 1.0, // 100% dropout
            ..Default::default()
        };

        engine.glitch_node(node, "vec", &config).unwrap();

        let updated = db.get_node(node).unwrap();
        let new_vec = updated.properties.get("vec").unwrap().as_vector().unwrap();

        // All dimensions should be 0.0 (and normalization skipped)
        assert_eq!(new_vec[0], 0.0);
        assert_eq!(new_vec[1], 0.0);
    }

    #[test]
    fn test_glitch_inversion() {
        let db = AletheiaDB::new().unwrap();
        db.enable_vector_index("vec", HnswConfig::new(2, DistanceMetric::Cosine))
            .unwrap();

        let props = PropertyMapBuilder::new()
            .insert_vector("vec", &[1.0, 0.0])
            .build();
        let node = db.create_node("Concept", props).unwrap();

        let engine = GlitchEngine::new(&db);
        let config = GlitchConfig {
            invert: true,
            noise_level: 0.0,
            ..Default::default()
        };

        engine.glitch_node(node, "vec", &config).unwrap();

        let updated = db.get_node(node).unwrap();
        let new_vec = updated.properties.get("vec").unwrap().as_vector().unwrap();

        // Should be inverted
        assert!((new_vec[0] - (-1.0)).abs() < 0.01);
        assert_eq!(new_vec[1], 0.0);
    }
}
