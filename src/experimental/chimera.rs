//! Chimera: Hybrid Entity Synthesis Engine.
//!
//! "What if we merged these two concepts?"
//!
//! Chimera allows you to generate a new node that is the semantic and structural
//! fusion of two existing nodes. It blends their properties (using configurable strategies)
//! and inherits their relationships.
//!
//! # Use Cases
//! - **Scenario Planning**: "Merge Dept A and Dept B".
//! - **Hypothetical Reasoning**: "What if Product X had the features of Product Y?"
//! - **Data Augmentation**: Generating synthetic training data.
//!
//! # Example
//! ```rust,no_run
//! use aletheiadb::AletheiaDB;
//! use aletheiadb::experimental::chimera::{ChimeraEngine, SynthesisConfig, PropertyMergeStrategy};
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let db = AletheiaDB::new()?;
//! let engine = ChimeraEngine::new(&db);
//!
//! let config = SynthesisConfig {
//!     alpha: 0.5, // 50/50 blend
//!     default_strategy: PropertyMergeStrategy::Mean,
//!     ..Default::default()
//! };
//!
//! // Create a chimera of node_a and node_b
//! // let new_id = engine.synthesize(node_a, node_b, config)?;
//! # Ok(())
//! # }
//! ```

use crate::AletheiaDB;
use crate::api::transaction::{ReadOps, WriteOps};
use crate::core::error::Result;
use crate::core::id::NodeId;
use crate::core::interning::GLOBAL_INTERNER;
use crate::core::property::{PropertyMapBuilder, PropertyValue};
use std::collections::{HashMap, HashSet};

/// Strategy for merging property values.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PropertyMergeStrategy {
    /// Keep the value from Node A.
    KeepA,
    /// Keep the value from Node B.
    KeepB,
    /// Calculate the mean (for numerics). Fallback to Lerp for vectors, KeepA for others.
    Mean,
    /// Calculate the sum (for numerics). Fallback to KeepA.
    Sum,
    /// Take the minimum value.
    Min,
    /// Take the maximum value.
    Max,
    /// Concatenate strings (A + separator + B). Fallback to KeepA for others.
    Concatenate,
    /// Linear Interpolation (for Vectors/Numbers). Fallback to KeepA.
    /// Value = (1 - alpha) * A + alpha * B
    Lerp,
}

/// Configuration for entity synthesis.
#[derive(Debug, Clone)]
pub struct SynthesisConfig {
    /// Blend factor (0.0 to 1.0). Used for Lerp.
    /// 0.0 means "Like Node A", 1.0 means "Like Node B".
    pub alpha: f32,
    /// Default merge strategy for properties.
    pub default_strategy: PropertyMergeStrategy,
    /// Specific strategies for named properties.
    pub property_strategies: HashMap<String, PropertyMergeStrategy>,
    /// String separator for concatenation.
    pub string_separator: String,
    /// Label for the new node. If None, inherits from A.
    pub new_label: Option<String>,
}

impl Default for SynthesisConfig {
    fn default() -> Self {
        Self {
            alpha: 0.5,
            default_strategy: PropertyMergeStrategy::KeepA, // Safe default
            property_strategies: HashMap::new(),
            string_separator: " / ".to_string(),
            new_label: Some("Chimera".to_string()),
        }
    }
}

/// The Chimera Engine.
pub struct ChimeraEngine<'a> {
    db: &'a AletheiaDB,
}

impl<'a> ChimeraEngine<'a> {
    /// Create a new ChimeraEngine.
    pub fn new(db: &'a AletheiaDB) -> Self {
        Self { db }
    }

    /// Synthesize a new node from two parent nodes.
    ///
    /// This creates a new node in the graph.
    pub fn synthesize(
        &self,
        node_a: NodeId,
        node_b: NodeId,
        config: SynthesisConfig,
    ) -> Result<NodeId> {
        self.db.write(|tx| {
            let a = tx.get_node(node_a)?;
            let b = tx.get_node(node_b)?;

            // 1. Merge Properties
            let mut props_builder = PropertyMapBuilder::new();
            let mut processed_keys = HashSet::new();

            // Collect all unique keys from both nodes
            let mut all_keys = Vec::new();
            for k in a.properties.keys() {
                if processed_keys.insert(*k) {
                    all_keys.push(*k);
                }
            }
            for k in b.properties.keys() {
                if processed_keys.insert(*k) {
                    all_keys.push(*k);
                }
            }

            for key in all_keys {
                // Resolve string key for strategy lookup
                let key_str = GLOBAL_INTERNER
                    .resolve_with(key, |s| s.to_string())
                    .unwrap_or_default();

                let val_a = a.properties.get_by_interned_key(&key);
                let val_b = b.properties.get_by_interned_key(&key);

                let strategy = config
                    .property_strategies
                    .get(&key_str)
                    .unwrap_or(&config.default_strategy);

                let merged_val = self.merge_value(val_a, val_b, *strategy, &config);

                if let Some(val) = merged_val {
                    props_builder = props_builder.insert(&key_str, val);
                }
            }

            // 2. Create Node
            let label_string = if let Some(lbl) = &config.new_label {
                lbl.clone()
            } else {
                GLOBAL_INTERNER
                    .resolve_with(a.label, |s| s.to_string())
                    .unwrap_or_else(|| "Unknown".to_string())
            };

            let new_node = tx.create_node(&label_string, props_builder.build())?;

            // 3. Inherit Edges
            // Outgoing: A -> X becomes New -> X
            // We do this for both A and B
            for &source in &[node_a, node_b] {
                let edges = tx.get_outgoing_edges(source);
                for edge_id in edges {
                    let edge = tx.get_edge(edge_id)?;
                    let label_str = GLOBAL_INTERNER
                        .resolve_with(edge.label, |s| s.to_string())
                        .unwrap_or_default();
                    tx.create_edge(
                        new_node,
                        edge.target,
                        &label_str,
                        edge.properties.clone(), // Clone edge properties for now
                    )?;
                }
            }

            // Incoming: X -> A becomes X -> New
            for &target in &[node_a, node_b] {
                let edges = tx.get_incoming_edges(target);
                for edge_id in edges {
                    let edge = tx.get_edge(edge_id)?;
                    let label_str = GLOBAL_INTERNER
                        .resolve_with(edge.label, |s| s.to_string())
                        .unwrap_or_default();
                    tx.create_edge(
                        edge.source,
                        new_node,
                        &label_str,
                        edge.properties.clone(),
                    )?;
                }
            }

            Ok(new_node)
        })
    }

    fn merge_value(
        &self,
        val_a: Option<&PropertyValue>,
        val_b: Option<&PropertyValue>,
        strategy: PropertyMergeStrategy,
        config: &SynthesisConfig,
    ) -> Option<PropertyValue> {
        match (val_a, val_b) {
            (Some(a), Some(b)) => match strategy {
                PropertyMergeStrategy::KeepA => Some(a.clone()),
                PropertyMergeStrategy::KeepB => Some(b.clone()),
                PropertyMergeStrategy::Mean => {
                    self.merge_numeric(a, b, |x, y| (x + y) / 2.0, config)
                }
                PropertyMergeStrategy::Sum => self.merge_numeric(a, b, |x, y| x + y, config),
                PropertyMergeStrategy::Min => {
                    self.merge_comparable(a, b, |x, y| if x < y { x } else { y })
                }
                PropertyMergeStrategy::Max => {
                    self.merge_comparable(a, b, |x, y| if x > y { x } else { y })
                }
                PropertyMergeStrategy::Concatenate => {
                    if let (Some(sa), Some(sb)) = (a.as_str(), b.as_str()) {
                        Some(PropertyValue::from(format!(
                            "{}{}{}",
                            sa, config.string_separator, sb
                        )))
                    } else {
                        Some(a.clone())
                    }
                }
                PropertyMergeStrategy::Lerp => self.merge_numeric(
                    a,
                    b,
                    |x, y| (1.0 - config.alpha as f64) * x + (config.alpha as f64) * y,
                    config,
                ),
            },
            (Some(a), None) => Some(a.clone()),
            (None, Some(b)) => Some(b.clone()),
            (None, None) => None,
        }
    }

    fn merge_numeric<F>(
        &self,
        a: &PropertyValue,
        b: &PropertyValue,
        op: F,
        _config: &SynthesisConfig,
    ) -> Option<PropertyValue>
    where
        F: Fn(f64, f64) -> f64,
    {
        // Try Vectors first
        if let (Some(va), Some(vb)) = (a.as_vector(), b.as_vector()) {
            if va.len() != vb.len() {
                return Some(a.clone()); // Dimension mismatch fallback
            }
            let res: Vec<f32> = va
                .iter()
                .zip(vb.iter())
                .map(|(x, y)| op(*x as f64, *y as f64) as f32)
                .collect();
            return Some(PropertyValue::from(res));
        }

        // Try scalars
        let na = a.as_float().or_else(|| a.as_int().map(|i| i as f64));
        let nb = b.as_float().or_else(|| b.as_int().map(|i| i as f64));

        if let (Some(x), Some(y)) = (na, nb) {
            let res = op(x, y);
            Some(PropertyValue::from(res))
        } else {
            // Fallback for non-numerics under numeric strategy
            Some(a.clone())
        }
    }

    fn merge_comparable<F>(&self, a: &PropertyValue, b: &PropertyValue, op: F) -> Option<PropertyValue>
    where
        F: Fn(f64, f64) -> f64,
    {
        // For Min/Max, we can reuse numeric logic if both are numbers.
        // If they are strings, we can compare strings.
        if let (Some(_sa), Some(_sb)) = (a.as_str(), b.as_str()) {
            // String comparison - fallback to A for now as 'op' is numeric only.
            // TODO: Implement string comparison logic if needed.
            return Some(a.clone());
        }
        self.merge_numeric(a, b, op, &SynthesisConfig::default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::vector::{DistanceMetric, HnswConfig};
    use tempfile::tempdir;

    fn create_test_db() -> (AletheiaDB, tempfile::TempDir) {
        let dir = tempdir().unwrap();
        let wal_path = dir.path().join("wal");
        let data_path = dir.path().join("data");
        std::fs::create_dir_all(&wal_path).unwrap();
        std::fs::create_dir_all(&data_path).unwrap();

        let wal_config = crate::config::WalConfigBuilder::new()
            .wal_dir(wal_path)
            .build();

        let persistence_config = crate::storage::index_persistence::PersistenceConfig {
            data_dir: data_path,
            enabled: false,
            ..Default::default()
        };

        let config = crate::AletheiaDBConfig::builder()
            .wal(wal_config)
            .persistence(persistence_config)
            .build();

        (AletheiaDB::with_unified_config(config).unwrap(), dir)
    }

    #[test]
    fn test_chimera_vector_blend() {
        let (db, _dir) = create_test_db();
        db.enable_vector_index("vec", HnswConfig::new(2, DistanceMetric::Cosine))
            .unwrap();

        // Node A: [0.0, 0.0]
        let a = db
            .create_node(
                "Node",
                PropertyMapBuilder::new()
                    .insert_vector("vec", &[0.0, 0.0])
                    .build(),
            )
            .unwrap();
        // Node B: [10.0, 10.0]
        let b = db
            .create_node(
                "Node",
                PropertyMapBuilder::new()
                    .insert_vector("vec", &[10.0, 10.0])
                    .build(),
            )
            .unwrap();

        let engine = ChimeraEngine::new(&db);
        let config = SynthesisConfig {
            alpha: 0.5,
            default_strategy: PropertyMergeStrategy::Lerp,
            ..Default::default()
        };

        let chimera = engine.synthesize(a, b, config).unwrap();
        let node = db.get_node(chimera).unwrap();
        let vec = node.get_property("vec").unwrap().as_vector().unwrap();

        // Expect mean: [5.0, 5.0]
        assert_eq!(vec, &[5.0, 5.0]);
    }

    #[test]
    fn test_chimera_property_merge() {
        let (db, _dir) = create_test_db();

        let a = db
            .create_node(
                "Person",
                PropertyMapBuilder::new()
                    .insert("age", 20)
                    .insert("name", "Alice")
                    .insert("skill", "Rust")
                    .build(),
            )
            .unwrap();

        let b = db
            .create_node(
                "Person",
                PropertyMapBuilder::new()
                    .insert("age", 40)
                    .insert("name", "Bob")
                    .insert("city", "London")
                    .build(),
            )
            .unwrap();

        let mut strategies = HashMap::new();
        strategies.insert("age".to_string(), PropertyMergeStrategy::Mean);
        strategies.insert("name".to_string(), PropertyMergeStrategy::Concatenate);
        strategies.insert("skill".to_string(), PropertyMergeStrategy::KeepA);
        strategies.insert("city".to_string(), PropertyMergeStrategy::KeepB);

        let config = SynthesisConfig {
            alpha: 0.5,
            default_strategy: PropertyMergeStrategy::KeepA,
            property_strategies: strategies,
            string_separator: "&".to_string(),
            new_label: Some("Hybrid".to_string()),
        };

        let engine = ChimeraEngine::new(&db);
        let chimera = engine.synthesize(a, b, config).unwrap();
        let node = db.get_node(chimera).unwrap();

        assert!(node.has_label_str("Hybrid"));
        assert_eq!(
            node.get_property("age").unwrap().as_float(),
            Some(30.0)
        );
        assert_eq!(
            node.get_property("name").unwrap().as_str(),
            Some("Alice&Bob")
        );
        assert_eq!(
            node.get_property("skill").unwrap().as_str(),
            Some("Rust")
        );
        assert_eq!(
            node.get_property("city").unwrap().as_str(),
            Some("London")
        );
    }

    #[test]
    fn test_chimera_edge_inheritance() {
        let (db, _dir) = create_test_db();

        let a = db.create_node("A", Default::default()).unwrap();
        let b = db.create_node("B", Default::default()).unwrap();
        let target = db.create_node("Target", Default::default()).unwrap();
        let source = db.create_node("Source", Default::default()).unwrap();

        // A -> Target
        db.create_edge(a, target, "OUT", Default::default())
            .unwrap();
        // Source -> B
        db.create_edge(source, b, "IN", Default::default())
            .unwrap();

        let engine = ChimeraEngine::new(&db);
        let chimera = engine.synthesize(a, b, Default::default()).unwrap();

        // Check Outgoing: Chimera -> Target
        let out_edges = db.get_outgoing_edges(chimera);
        assert_eq!(out_edges.len(), 1);
        let edge = db.get_edge(out_edges[0]).unwrap();
        assert_eq!(edge.target, target);

        // Check Incoming: Source -> Chimera
        let in_edges = db.get_incoming_edges(chimera);
        assert_eq!(in_edges.len(), 1);
        let edge = db.get_edge(in_edges[0]).unwrap();
        assert_eq!(edge.source, source);
    }
}
