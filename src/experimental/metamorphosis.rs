//! Metamorphosis: Schema Evolution via Graph Transformation 🦋
//!
//! "Change is the only constant."
//!
//! Metamorphosis allows you to migrate nodes from one label (schema) to another,
//! transforming properties and preserving relationships along the way.
//!
//! # Use Cases
//! - **Schema Evolution**: Move from `User` to `Customer` with new fields.
//! - **Data Cleaning**: Standardize entities.
//! - **LLM Integration**: Apply schema changes suggested by an LLM.

use crate::api::transaction::WriteOps;
use crate::core::error::{Error, Result};
use crate::core::id::NodeId;
use crate::core::interning::GLOBAL_INTERNER;
use crate::core::property::PropertyMapBuilder;
use std::collections::{HashMap, HashSet};

/// Defines how to transform properties during migration.
#[derive(Debug, Clone)]
pub enum PropertyMapping {
    /// Keep the property as is (default behavior).
    Keep(String),
    /// Rename the property key.
    Rename {
        /// The old property key.
        old: String,
        /// The new property key.
        new: String,
    },
    /// Drop the property.
    Drop(String),
}

/// A rule defining the migration from one label to another.
#[derive(Debug, Clone)]
pub struct TransformationRule {
    /// The label of nodes to migrate (must match).
    pub source_label: String,
    /// The new label to assign.
    pub target_label: String,
    /// Specific property mappings.
    /// By default, all properties are KEPT unless explicitly dropped or renamed.
    pub mappings: Vec<PropertyMapping>,
}

impl TransformationRule {
    /// Create a new rule.
    pub fn new(source: &str, target: &str) -> Self {
        Self {
            source_label: source.to_string(),
            target_label: target.to_string(),
            mappings: Vec::new(),
        }
    }

    /// Add a property rename mapping.
    pub fn rename(mut self, old: &str, new: &str) -> Self {
        self.mappings.push(PropertyMapping::Rename {
            old: old.to_string(),
            new: new.to_string(),
        });
        self
    }

    /// Add a property drop mapping.
    pub fn drop(mut self, key: &str) -> Self {
        self.mappings.push(PropertyMapping::Drop(key.to_string()));
        self
    }
}

/// The Engine for applying transformations.
pub struct MetamorphosisEngine<'a, T: WriteOps> {
    tx: &'a mut T,
}

impl<'a, T: WriteOps> MetamorphosisEngine<'a, T> {
    /// Create a new engine with a transaction handle.
    pub fn new(tx: &'a mut T) -> Self {
        Self { tx }
    }

    /// Migrate a single node according to the rule.
    ///
    /// This creates a new node, copies/transforms properties, moves edges,
    /// and deletes the old node.
    ///
    /// # Returns
    /// The ID of the new node.
    pub fn migrate_node(&mut self, node_id: NodeId, rule: &TransformationRule) -> Result<NodeId> {
        // 1. Validate Source Node
        let old_node = self.tx.get_node(node_id)?;
        if !old_node.has_label_str(&rule.source_label) {
            // TODO: Use a specific error type if available, or generic StorageError
            return Err(Error::Other(format!(
                "Node {} does not have source label '{}'",
                node_id, rule.source_label
            )));
        }

        // 2. Prepare New Properties
        let mut new_props = PropertyMapBuilder::new();

        // Build a map of what to do with each property
        let mut drops = HashSet::new();
        let mut renames = HashMap::new();

        for mapping in &rule.mappings {
            match mapping {
                PropertyMapping::Drop(k) => { drops.insert(k.clone()); },
                PropertyMapping::Rename { old, new } => { renames.insert(old.clone(), new.clone()); },
                PropertyMapping::Keep(_) => {}, // Implicit default
            }
        }

        // Iterate old properties
        for (key_id, val) in old_node.properties.iter() {
            let key = GLOBAL_INTERNER.resolve_with(*key_id, |s| s.to_string())
                .unwrap_or_else(|| "unknown".to_string()); // Should never happen

            if drops.contains(&key) {
                continue;
            }

            if let Some(new_key) = renames.get(&key) {
                new_props = new_props.insert(new_key, val.clone());
            } else {
                // Keep as is
                new_props = new_props.insert(&key, val.clone());
            }
        }

        // 3. Create New Node
        let new_node_id = self.tx.create_node(&rule.target_label, new_props.build())?;

        // 4. Move Edges
        // We must handle self-loops: edges where source == target == old_node.
        // We track processed edges to avoid duplicating self-loops (which appear in both lists).
        let mut processed_edges = HashSet::new();

        // 4a. Outgoing Edges (Old -> Target)
        let outgoing = self.tx.get_outgoing_edges(node_id);
        for edge_id in outgoing {
            if processed_edges.contains(&edge_id) { continue; }

            let edge = self.tx.get_edge(edge_id)?;
            let label = GLOBAL_INTERNER.resolve_with(edge.label, |s| s.to_string())
                .unwrap_or_default();

            // If target is old_node (Self-loop), point to new_node
            let target = if edge.target == node_id {
                new_node_id
            } else {
                edge.target
            };

            self.tx.create_edge(new_node_id, target, &label, edge.properties.clone())?;
            self.tx.delete_edge(edge_id)?;
            processed_edges.insert(edge_id);
        }

        // 4b. Incoming Edges (Source -> Old)
        let incoming = self.tx.get_incoming_edges(node_id);
        for edge_id in incoming {
            if processed_edges.contains(&edge_id) { continue; }

            let edge = self.tx.get_edge(edge_id)?;
            let label = GLOBAL_INTERNER.resolve_with(edge.label, |s| s.to_string())
                .unwrap_or_default();

            // If source is old_node (Self-loop), point from new_node
            // (But self-loops should have been caught in Outgoing pass!)
            // Let's be safe.
            let source = if edge.source == node_id {
                new_node_id
            } else {
                edge.source
            };

            self.tx.create_edge(source, new_node_id, &label, edge.properties.clone())?;
            self.tx.delete_edge(edge_id)?;
            processed_edges.insert(edge_id);
        }

        // 5. Delete Old Node (We already deleted edges)
        self.tx.delete_node(node_id)?;

        Ok(new_node_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::AletheiaDB;
    use crate::api::transaction::{ReadOps, WriteOps};
    use crate::core::property::PropertyMapBuilder;

    #[test]
    fn test_basic_migration() {
        let db = AletheiaDB::new().unwrap();

        // Setup: Create User and Commit
        let old_id = db.write(|tx| {
            let props = PropertyMapBuilder::new()
                .insert("name", "Alice")
                .insert("age", 30i64)
                .build();
            tx.create_node("User", props)
        }).unwrap();

        // Migrate to Customer in new transaction
        let rule = TransformationRule::new("User", "Customer")
            .rename("name", "full_name")
            .drop("age");

        let new_id = db.write(|tx| {
            let mut engine = MetamorphosisEngine::new(tx);
            engine.migrate_node(old_id, &rule)
        }).unwrap();

        // Verify
        db.read(|tx| {
            let new_node = tx.get_node(new_id)?;
            assert_eq!(new_node.has_label_str("Customer"), true);
            assert_eq!(new_node.get_property("full_name").unwrap().as_str(), Some("Alice"));
            assert_eq!(new_node.get_property("age"), None);

            // Verify old node is gone
            assert!(tx.get_node(old_id).is_err());
            Ok::<(), crate::core::error::Error>(())
        }).unwrap();
    }

    #[test]
    fn test_edge_migration() {
        let db = AletheiaDB::new().unwrap();

        // Setup
        let (alice, bob) = db.write(|tx| {
            let alice = tx.create_node("User", PropertyMapBuilder::new().insert("name", "Alice").build())?;
            let bob = tx.create_node("User", PropertyMapBuilder::new().insert("name", "Bob").build())?;
            // Alice -> Bob
            tx.create_edge(alice, bob, "KNOWS", PropertyMapBuilder::new().build())?;
            Ok::<_, crate::core::error::Error>((alice, bob))
        }).unwrap();

        // Migrate Alice -> Customer
        let rule = TransformationRule::new("User", "Customer");
        let new_alice = db.write(|tx| {
            let mut engine = MetamorphosisEngine::new(tx);
            engine.migrate_node(alice, &rule)
        }).unwrap();

        // Verify Edge: NewAlice -> Bob
        db.read(|tx| {
            let outgoing = tx.get_outgoing_edges(new_alice);
            assert_eq!(outgoing.len(), 1);
            let edge = tx.get_edge(outgoing[0])?;
            assert_eq!(edge.target, bob);
            assert_eq!(edge.has_label_str("KNOWS"), true);
            Ok::<(), crate::core::error::Error>(())
        }).unwrap();
    }

    #[test]
    fn test_incoming_edge_migration() {
        let db = AletheiaDB::new().unwrap();

        let (alice, bob) = db.write(|tx| {
            let alice = tx.create_node("User", PropertyMapBuilder::new().insert("name", "Alice").build())?;
            let bob = tx.create_node("User", PropertyMapBuilder::new().insert("name", "Bob").build())?;
            // Bob -> Alice
            tx.create_edge(bob, alice, "LIKES", PropertyMapBuilder::new().build())?;
            Ok::<_, crate::core::error::Error>((alice, bob))
        }).unwrap();

        // Migrate Alice -> Customer
        let rule = TransformationRule::new("User", "Customer");
        let new_alice = db.write(|tx| {
            let mut engine = MetamorphosisEngine::new(tx);
            engine.migrate_node(alice, &rule)
        }).unwrap();

        // Verify Edge: Bob -> NewAlice
        db.read(|tx| {
            let incoming = tx.get_incoming_edges(new_alice);
            assert_eq!(incoming.len(), 1);
            let edge = tx.get_edge(incoming[0])?;
            assert_eq!(edge.source, bob);
            assert_eq!(edge.has_label_str("LIKES"), true);
            Ok::<(), crate::core::error::Error>(())
        }).unwrap();
    }

    #[test]
    fn test_self_loop_migration() {
        let db = AletheiaDB::new().unwrap();

        let node = db.write(|tx| {
            let node = tx.create_node("Node", PropertyMapBuilder::new().build())?;
            // Node -> Node
            tx.create_edge(node, node, "SELF", PropertyMapBuilder::new().build())?;
            Ok::<_, crate::core::error::Error>(node)
        }).unwrap();

        // Migrate
        let rule = TransformationRule::new("Node", "NewNode");
        let new_node = db.write(|tx| {
            let mut engine = MetamorphosisEngine::new(tx);
            engine.migrate_node(node, &rule)
        }).unwrap();

        // Verify: NewNode -> NewNode
        db.read(|tx| {
            let outgoing = tx.get_outgoing_edges(new_node);
            assert_eq!(outgoing.len(), 1);
            let edge = tx.get_edge(outgoing[0])?;
            assert_eq!(edge.source, new_node);
            assert_eq!(edge.target, new_node);
            assert_eq!(edge.has_label_str("SELF"), true);
            Ok::<(), crate::core::error::Error>(())
        }).unwrap();
    }

    #[test]
    fn test_fail_on_wrong_label() {
        let db = AletheiaDB::new().unwrap();
        let mut tx = db.write_transaction().unwrap();

        let node = tx.create_node("User", PropertyMapBuilder::new().build()).unwrap();

        // Rule expects "Customer"
        let rule = TransformationRule::new("Customer", "User");
        let mut engine = MetamorphosisEngine::new(&mut tx);

        let result = engine.migrate_node(node, &rule);
        assert!(result.is_err());
    }
}
