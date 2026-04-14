//! Telepathy: Semantic Spreading Activation Engine.
//!
//! "If I think of 'Apple', what else lights up?"
//!
//! Telepathy simulates how activation signals propagate through the graph,
//! modulated by semantic similarity. It allows you to find nodes that are
//! both structurally connected AND semantically relevant.
//!
//! # How it works
//! 1. **Seed**: You start with a set of "activated" nodes (e.g., search results).
//! 2. **Propagate**: Activation flows to neighbors via edges.
//! 3. **Modulate**: The "width" of the pipe is determined by vector similarity.
//!    - High Similarity (Twin) -> High Conductivity (Signal passes through).
//!    - Low Similarity (Stranger) -> High Resistance (Signal decays).
//! 4. **Decay**: Signal naturally fades over hops.
//!
//! # Use Cases
//! - **Context Expansion**: "Find everything related to this incident, but stay on topic."
//! - **Recommendation**: "Users who liked X also liked Y (but only if Y is semantically similar)."
//! - **Ambiguity Resolution**: Disambiguate "Apple" (Fruit) vs "Apple" (Tech) by checking which cluster lights up.

use crate::AletheiaDB;
use crate::core::error::Result;
use crate::core::id::NodeId;
use crate::core::vector::ops::cosine_similarity;
use std::collections::HashMap;

/// Configuration for the Telepathy engine.
#[derive(Debug, Clone)]
pub struct TelepathyConfig {
    /// Name of the vector property to use for semantic modulation.
    pub vector_property: String,
    /// Decay factor per hop (0.0 to 1.0).
    /// e.g., 0.5 means signal halves at each step.
    pub decay: f32,
    /// Minimum activation threshold. Signals below this are culled.
    pub threshold: f32,
    /// Maximum number of propagation steps.
    pub max_steps: usize,
    /// Default weight for edges where one/both nodes lack vectors.
    /// (0.0 = Block, 1.0 = Free Pass).
    pub missing_vector_weight: f32,
}

impl Default for TelepathyConfig {
    fn default() -> Self {
        Self {
            vector_property: "embedding".to_string(),
            decay: 0.8,
            threshold: 0.01,
            max_steps: 3,
            missing_vector_weight: 0.1, // Penalize, but don't block completely
        }
    }
}

/// The Telepathy Engine.
pub struct TelepathyEngine<'a> {
    db: &'a AletheiaDB,
    config: TelepathyConfig,
}

impl<'a> TelepathyEngine<'a> {
    /// Create a new TelepathyEngine with default config.
    pub fn new(db: &'a AletheiaDB) -> Self {
        Self {
            db,
            config: TelepathyConfig::default(),
        }
    }

    /// Customize the configuration.
    pub fn with_config(mut self, config: TelepathyConfig) -> Self {
        self.config = config;
        self
    }

    /// Run the activation simulation.
    ///
    /// # Arguments
    /// * `seeds` - Initial activated nodes with their starting strength (usually 1.0).
    ///
    /// # Returns
    /// A map of NodeId -> Activation Strength, sorted by strength (descending).
    pub fn propagate(&self, seeds: &HashMap<NodeId, f32>) -> Result<Vec<(NodeId, f32)>> {
        let mut activations = seeds.clone();

        // Cache vectors to avoid repeated DB lookups
        // Map<NodeId, Option<Vec<f32>>>
        let mut vector_cache: HashMap<NodeId, Option<Vec<f32>>> = HashMap::new();

        // Helper to get vector (cached)
        let mut get_vector = |node_id: NodeId| -> Result<Option<Vec<f32>>> {
            if let Some(v) = vector_cache.get(&node_id) {
                return Ok(v.clone());
            }
            let node = self.db.get_node(node_id)?;
            let vec = node
                .properties
                .get(&self.config.vector_property)
                .and_then(|v| v.as_vector())
                .map(|v| v.to_vec());

            vector_cache.insert(node_id, vec.clone());
            Ok(vec)
        };

        // Pre-load seed vectors
        for &id in seeds.keys() {
            get_vector(id)?;
        }

        for _step in 0..self.config.max_steps {
            let mut next_activations = activations.clone();
            let mut changed = false;

            // Iterate over currently active nodes
            // We clone keys to avoid borrowing issues while updating next_activations
            let active_nodes: Vec<NodeId> = activations
                .iter()
                .filter(|(_, v)| **v >= self.config.threshold)
                .map(|(k, _)| *k)
                .collect();

            for source in active_nodes {
                let source_strength = activations[&source];
                let source_vec = get_vector(source)?;

                // Propagate to neighbors
                // We consider outgoing edges. Should we also consider incoming?
                // "Telepathy" usually implies following links. Let's stick to outgoing for now.
                // Or maybe both? Let's do outgoing to respect graph directionality.
                let edges = self.db.get_outgoing_edges(source);

                for edge_id in edges {
                    let edge = self.db.get_edge(edge_id)?;
                    let target = edge.target;

                    // Calculate semantic weight
                    let target_vec = get_vector(target)?;

                    let weight = match (&source_vec, &target_vec) {
                        (Some(a), Some(b)) => {
                            // Cosine similarity is [-1, 1].
                            // We map this to [0, 1] for activation weight.
                            // Negative similarity (opposites) should inhibit?
                            // Or just clamp to 0?
                            // Let's clamp max(0, sim).
                            let sim = cosine_similarity(a, b)?;
                            sim.max(0.0)
                        }
                        _ => self.config.missing_vector_weight,
                    };

                    // Signal = Source * Weight * Decay
                    let signal = source_strength * weight * self.config.decay;

                    if signal > 0.0 {
                        // Accumulate signal
                        // Simplest model: Max aggregation (signal takes strongest path)
                        // vs Sum aggregation (signals stack).
                        // Sum is more "physics-like", Max is more "path-finding-like".
                        // Spreading activation usually Sums.

                        let current_val = next_activations.entry(target).or_insert(0.0);

                        // Use MAX aggregation to find the strongest semantic path.
                        // This prevents signal explosion in cyclic graphs and simplifies interpretation.
                        if signal > *current_val {
                            let diff = signal - *current_val;
                            if diff > 0.0001 {
                                *current_val = signal;
                                changed = true;
                            }
                        }
                    }
                }
            }

            activations = next_activations;
            if !changed {
                break;
            }
        }

        // Convert to sorted vec
        let mut result: Vec<_> = activations.into_iter().collect();
        result.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::property::PropertyMapBuilder;
    use crate::index::vector::{DistanceMetric, HnswConfig};

    #[test]
    fn test_telepathy_semantic_gate() {
        let db = AletheiaDB::new().unwrap();
        // Config: 2D vectors
        db.enable_vector_index("vec", HnswConfig::new(2, DistanceMetric::Cosine))
            .unwrap();

        // Node A: Source [1.0, 0.0]
        // Node B: Twin   [1.0, 0.0] (Sim = 1.0)
        // Node C: Stranger [0.0, 1.0] (Sim = 0.0)

        // Graph: A -> B, A -> C

        let props_a = PropertyMapBuilder::new()
            .insert_vector("vec", &[1.0, 0.0])
            .build();
        let a = db.create_node("Node", props_a).unwrap();

        let props_b = PropertyMapBuilder::new()
            .insert_vector("vec", &[1.0, 0.0])
            .build();
        let b = db.create_node("Node", props_b).unwrap();

        let props_c = PropertyMapBuilder::new()
            .insert_vector("vec", &[0.0, 1.0])
            .build();
        let c = db.create_node("Node", props_c).unwrap();

        db.create_edge(a, b, "LINK", PropertyMapBuilder::new().build())
            .unwrap();
        db.create_edge(a, c, "LINK", PropertyMapBuilder::new().build())
            .unwrap();

        let engine = TelepathyEngine::new(&db).with_config(TelepathyConfig {
            vector_property: "vec".to_string(),
            decay: 1.0, // No decay for distance, purely semantic test
            threshold: 0.0,
            max_steps: 1,
            missing_vector_weight: 0.0,
        });

        let mut seeds = HashMap::new();
        seeds.insert(a, 1.0);

        let results = engine.propagate(&seeds).unwrap();
        let results_map: HashMap<NodeId, f32> = results.into_iter().collect();

        // B should be activated (1.0 * 1.0 * 1.0 = 1.0)
        // C should not be activated (1.0 * 0.0 * 1.0 = 0.0)

        assert!(results_map[&b] > 0.9, "Twin should be activated");
        assert!(
            *results_map.get(&c).unwrap_or(&0.0) < 0.1,
            "Stranger should be blocked"
        );
    }

    #[test]
    fn test_telepathy_decay_chain() {
        let db = AletheiaDB::new().unwrap();
        db.enable_vector_index("vec", HnswConfig::new(2, DistanceMetric::Cosine))
            .unwrap();

        // Chain: A -> B -> C
        // All vectors identical (Perfect semantic conductivity)
        // Only decay stops it.

        let props = PropertyMapBuilder::new()
            .insert_vector("vec", &[1.0, 0.0])
            .build();
        let a = db.create_node("Node", props.clone()).unwrap();
        let b = db.create_node("Node", props.clone()).unwrap();
        let c = db.create_node("Node", props.clone()).unwrap();

        db.create_edge(a, b, "NEXT", PropertyMapBuilder::new().build())
            .unwrap();
        db.create_edge(b, c, "NEXT", PropertyMapBuilder::new().build())
            .unwrap();

        let decay = 0.5;
        let engine = TelepathyEngine::new(&db).with_config(TelepathyConfig {
            vector_property: "vec".to_string(),
            decay,
            threshold: 0.0,
            max_steps: 2,
            missing_vector_weight: 0.0,
        });

        let mut seeds = HashMap::new();
        seeds.insert(a, 1.0);

        let results = engine.propagate(&seeds).unwrap();
        let results_map: HashMap<NodeId, f32> = results.into_iter().collect();

        // A = 1.0
        // B = 1.0 * 1.0 * 0.5 = 0.5
        // C = 0.5 * 1.0 * 0.5 = 0.25

        assert!((results_map[&b] - 0.5).abs() < 0.01, "B should be 0.5");
        assert!((results_map[&c] - 0.25).abs() < 0.01, "C should be 0.25");
    }
}
