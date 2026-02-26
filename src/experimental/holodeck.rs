//! Holodeck: The Counterfactual Simulation Engine 🌌
//!
//! "Fork the universe. Test 'What If' scenarios. Inspect the divergence."
//!
//! The Holodeck allows you to create a lightweight, isolated simulation of the database state.
//! You can inject events (writes) into this simulation and query the result, without
//! affecting the actual database.
//!
//! # How it works
//!
//! 1. **Fork**: Creates a `Holodeck` instance which wraps a `WriteTransaction`.
//! 2. **Simulate**: Perform writes (`create_node`, `update_node`, etc.) on the Holodeck.
//!    These writes are buffered in memory and *never committed* to the real WAL.
//! 3. **Inspect**: Use `ReadOps` to query the simulated state. The Holodeck sees its own writes
//!    layered on top of the real database state (Read-Your-Writes).
//! 4. **Diff**: Call `diff()` to see a list of changes made in the simulation.
//! 5. **Dissolve**: Drop the Holodeck to discard the simulation.
//!
//! # Example
//!
//! ```rust,no_run
//! use aletheiadb::{AletheiaDB, properties, api::transaction::{ReadOps, WriteOps}};
//! use aletheiadb::experimental::holodeck::Holodeck;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let db = AletheiaDB::new()?;
//! // ... setup real data ...
//!
//! // Enter the Holodeck
//! let mut holodeck = Holodeck::new(&db)?;
//!
//! // Make a counterfactual change
//! let node_id = holodeck.create_node("SimulationNode", properties! { "status" => "draft" })?;
//!
//! // Query the simulation
//! let node = holodeck.get_node(node_id)?;
//! assert_eq!(node.label.resolve().unwrap(), "SimulationNode");
//!
//! // Real DB doesn't have it
//! assert!(db.get_node(node_id).is_err());
//!
//! // See what changed
//! for change in holodeck.diff() {
//!     println!("Simulation Event: {}", change);
//! }
//! # Ok(())
//! # }
//! ```

use crate::AletheiaDB;
use crate::api::transaction::{BufferedWrite, ReadOps, WriteOps, WriteTransaction};
use crate::core::error::Result;
use crate::core::graph::{Edge, Node};
use crate::core::id::{EdgeId, NodeId};
use crate::core::interning::GLOBAL_INTERNER;
use crate::core::property::{PropertyMap, PropertyValue};
use crate::core::temporal::Timestamp;

/// A simulation sandbox for counterfactual reasoning.
pub struct Holodeck {
    tx: WriteTransaction,
}

impl Holodeck {
    /// Create a new Holodeck simulation.
    ///
    /// This starts a new write transaction that serves as the sandbox.
    /// The transaction is never committed to the main database.
    pub fn new(db: &AletheiaDB) -> Result<Self> {
        Ok(Self {
            tx: db.write_transaction()?,
        })
    }

    /// Get a list of changes made in this simulation.
    ///
    /// Returns a list of human-readable strings describing the operations
    /// buffered in the simulation.
    pub fn diff(&self) -> Vec<String> {
        self.tx
            .buffer
            .operations()
            .iter()
            .map(|op| match op {
                BufferedWrite::CreateNode { node_id, label, .. } => {
                    let label_str = GLOBAL_INTERNER
                        .resolve_with(*label, |s| s.to_string())
                        .unwrap_or_else(|| "Unknown".into());
                    format!("Created Node {} ({})", node_id, label_str)
                }
                BufferedWrite::CreateEdge {
                    edge_id,
                    label,
                    source,
                    target,
                    ..
                } => {
                    let label_str = GLOBAL_INTERNER
                        .resolve_with(*label, |s| s.to_string())
                        .unwrap_or_else(|| "Unknown".into());
                    format!(
                        "Created Edge {} ({}) from {} to {}",
                        edge_id, label_str, source, target
                    )
                }
                BufferedWrite::UpdateNode { node_id, .. } => {
                    format!("Updated Node {}", node_id)
                }
                BufferedWrite::UpdateEdge { edge_id, .. } => {
                    format!("Updated Edge {}", edge_id)
                }
                BufferedWrite::DeleteNode { node_id, .. } => {
                    format!("Deleted Node {}", node_id)
                }
                BufferedWrite::DeleteEdge { edge_id, .. } => {
                    format!("Deleted Edge {}", edge_id)
                }
            })
            .collect()
    }

    /// Discard the simulation and release resources.
    ///
    /// This explicitly rolls back the underlying transaction.
    /// Dropping the Holodeck also rolls back, but this makes it explicit.
    pub fn dissolve(self) -> Result<()> {
        self.tx.rollback()
    }
}

// Proxy ReadOps to the inner transaction
impl ReadOps for Holodeck {
    fn get_node(&self, id: NodeId) -> Result<Node> {
        self.tx.get_node(id)
    }

    fn get_edge(&self, id: EdgeId) -> Result<Edge> {
        self.tx.get_edge(id)
    }

    fn get_outgoing_edges(&self, node_id: NodeId) -> Vec<EdgeId> {
        self.tx.get_outgoing_edges(node_id)
    }

    fn get_incoming_edges(&self, node_id: NodeId) -> Vec<EdgeId> {
        self.tx.get_incoming_edges(node_id)
    }

    fn get_outgoing_edges_with_label(&self, node_id: NodeId, label: &str) -> Vec<EdgeId> {
        self.tx.get_outgoing_edges_with_label(node_id, label)
    }

    fn node_count(&self) -> usize {
        self.tx.node_count()
    }

    fn edge_count(&self) -> usize {
        self.tx.edge_count()
    }

    fn find_nodes_by_property(
        &self,
        label: &str,
        property_key: &str,
        property_value: &PropertyValue,
    ) -> Vec<NodeId> {
        self.tx
            .find_nodes_by_property(label, property_key, property_value)
    }
}

// Proxy WriteOps to the inner transaction
impl WriteOps for Holodeck {
    fn create_node_with_valid_time(
        &mut self,
        label: &str,
        properties: PropertyMap,
        valid_from: Option<Timestamp>,
    ) -> Result<NodeId> {
        self.tx
            .create_node_with_valid_time(label, properties, valid_from)
    }

    fn create_edge_with_valid_time(
        &mut self,
        source: NodeId,
        target: NodeId,
        label: &str,
        properties: PropertyMap,
        valid_from: Option<Timestamp>,
    ) -> Result<EdgeId> {
        self.tx
            .create_edge_with_valid_time(source, target, label, properties, valid_from)
    }

    fn update_node_with_valid_time(
        &mut self,
        node_id: NodeId,
        properties: PropertyMap,
        valid_from: Option<Timestamp>,
    ) -> Result<()> {
        self.tx
            .update_node_with_valid_time(node_id, properties, valid_from)
    }

    fn update_edge_with_valid_time(
        &mut self,
        edge_id: EdgeId,
        properties: PropertyMap,
        valid_from: Option<Timestamp>,
    ) -> Result<()> {
        self.tx
            .update_edge_with_valid_time(edge_id, properties, valid_from)
    }

    fn delete_node_with_valid_time(
        &mut self,
        node_id: NodeId,
        valid_from: Option<Timestamp>,
    ) -> Result<()> {
        self.tx.delete_node_with_valid_time(node_id, valid_from)
    }

    fn delete_node_cascade(&mut self, node_id: NodeId) -> Result<()> {
        self.tx.delete_node_cascade(node_id)
    }

    fn delete_edge_with_valid_time(
        &mut self,
        edge_id: EdgeId,
        valid_from: Option<Timestamp>,
    ) -> Result<()> {
        self.tx.delete_edge_with_valid_time(edge_id, valid_from)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::property::PropertyMapBuilder;

    #[test]
    fn test_holodeck_counterfactual() {
        let db = AletheiaDB::new().unwrap();

        // 1. Reality: Create Alice
        let real_props = PropertyMapBuilder::new()
            .insert("name", "Alice")
            .insert("status", "Alive")
            .build();
        let alice_id = db.write(|tx| tx.create_node("Person", real_props)).unwrap();

        // 2. Fork Reality into Holodeck
        let mut holodeck = Holodeck::new(&db).unwrap();

        // 3. Counterfactual: Update Alice to "Deceased" in simulation
        let sim_props = PropertyMapBuilder::new()
            .insert("status", "Deceased")
            .build();
        holodeck.update_node(alice_id, sim_props).unwrap();

        // 4. Verify Reality is unchanged
        let real_alice = db.get_node(alice_id).unwrap();
        assert_eq!(
            real_alice.get_property("status").unwrap().as_str().unwrap(),
            "Alive"
        );

        // 5. Verify Holodeck is changed
        let sim_alice = holodeck.get_node(alice_id).unwrap();
        assert_eq!(
            sim_alice.get_property("status").unwrap().as_str().unwrap(),
            "Deceased"
        );

        // 6. Verify Diff
        let diff = holodeck.diff();
        assert_eq!(diff.len(), 1);
        assert!(diff[0].contains("Updated Node"));

        // 7. Dissolve (Explicit rollback)
        holodeck.dissolve().unwrap();
    }

    #[test]
    fn test_holodeck_isolation() {
        let db = AletheiaDB::new().unwrap();
        let mut holodeck = Holodeck::new(&db).unwrap();

        // Create node in Holodeck
        let node_id = holodeck
            .create_node("Ghost", PropertyMapBuilder::new().build())
            .unwrap();

        // Should exist in Holodeck
        assert!(holodeck.get_node(node_id).is_ok());

        // Should NOT exist in Real DB
        assert!(db.get_node(node_id).is_err());
    }
}
