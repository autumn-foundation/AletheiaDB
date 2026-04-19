//! Graph Context Exporter ("The Scribe")
//!
//! This module provides tools to generate rich, human-readable context descriptions
//! for nodes in the graph. It is designed specifically for **Retrieval-Augmented Generation (RAG)**
//! scenarios where an LLM needs to understand a node's state, history, and relationships.
//!
//! # Purpose
//!
//! When feeding graph data to an LLM, raw JSON or triples can be token-inefficient and hard
//! for models to reason about. The `GraphContextBuilder` converts a node's "ego network"
//! into a structured Markdown format that highlights:
//!
//! 1.  **Identity**: Who/what is this node? (Label, ID)
//! 2.  **State**: What are its properties?
//! 3.  **Evolution**: How has it changed over time? (Narrative history)
//! 4.  **Context**: Who is it connected to? (Immediate neighborhood)
//!
//! # Usage
//!
//! ```rust
//! use aletheiadb::AletheiaDB;
//! use aletheiadb::experimental::graph_context::GraphContextBuilder;
//! use aletheiadb::core::id::NodeId;
//!
//! # fn example(db: &AletheiaDB, node_id: NodeId) -> aletheiadb::core::error::Result<()> {
//! // Create a builder for a specific node
//! let context = GraphContextBuilder::new(db, node_id)
//!     .with_history_limit(3)   // Limit history to last 3 events
//!     .with_neighbor_limit(5)  // Limit neighbors to 5
//!     .build()?;
//!
//! println!("{}", context);
//! # Ok(())
//! # }
//! ```
//!
//! # Output Format
//!
//! The output is a Markdown string structured as follows:
//!
//! ```markdown
//! # Node Context: 42 (Person)
//!
//! ## Properties
//! - name: "Alice"
//! - role: "Engineer"
//!
//! ## Evolution
//! - 2023-10-27T10:00:00Z (v2): updated properties
//!   - Modified property 'role' from '"Intern"' to '"Engineer"'
//!
//! ## Neighborhood
//! - WORKS_AT -> Company (101)
//!   - Properties: { since: 2020 }
//! ```

use crate::AletheiaDB;
use crate::core::error::Result;
use crate::core::id::NodeId;
use crate::core::interning::{GLOBAL_INTERNER, InternedString};
use crate::experimental::temporal_narrative::NarrativeGenerator;
use std::fmt::Write;

/// Builder for generating a rich context description of a node.
///
/// This generates a Markdown-formatted string containing the node's current state,
/// recent history (evolution), and immediate neighborhood. It is designed for
/// injecting context into LLM prompts.
///
/// # Example
///
/// ```rust
/// # use aletheiadb::AletheiaDB;
/// # use aletheiadb::experimental::graph_context::GraphContextBuilder;
/// # fn example(db: &AletheiaDB, node_id: aletheiadb::core::id::NodeId) -> aletheiadb::core::error::Result<()> {
/// let context = GraphContextBuilder::new(db, node_id)
///     .with_history_limit(5)
///     .with_neighbor_limit(10)
///     .build()?;
/// # Ok(())
/// # }
/// ```
pub struct GraphContextBuilder<'a> {
    db: &'a AletheiaDB,
    center_node: NodeId,
    history_limit: usize,
    neighbor_limit: usize,
}

impl<'a> GraphContextBuilder<'a> {
    /// Create a new builder for the given node.
    ///
    /// # Arguments
    ///
    /// * `db` - Reference to the database instance.
    /// * `center_node` - The ID of the node to generate context for.
    pub fn new(db: &'a AletheiaDB, center_node: NodeId) -> Self {
        Self {
            db,
            center_node,
            history_limit: 5,
            neighbor_limit: 10,
        }
    }

    /// Set the maximum number of history events to include.
    ///
    /// # Trade-offs
    ///
    /// *   **Higher limit**: Provides more context on how the node evolved, useful for understanding causality.
    /// *   **Lower limit**: Saves tokens in the LLM prompt window.
    ///
    /// Defaults to 5.
    pub fn with_history_limit(mut self, limit: usize) -> Self {
        self.history_limit = limit;
        self
    }

    /// Set the maximum number of neighbors to include.
    ///
    /// # Trade-offs
    ///
    /// *   **Higher limit**: Provides a broader view of the node's connections.
    /// *   **Lower limit**: Saves tokens and prevents "context pollution" from irrelevant neighbors.
    ///
    /// Defaults to 10.
    pub fn with_neighbor_limit(mut self, limit: usize) -> Self {
        self.neighbor_limit = limit;
        self
    }

    fn resolve(s: InternedString) -> String {
        GLOBAL_INTERNER
            .resolve_with(s, |s| s.to_string())
            .unwrap_or_else(|| format!("<interned:{}>", s.as_u32()))
    }

    /// Build the context string (Markdown).
    ///
    /// # Returns
    ///
    /// A formatted Markdown string containing the node's context.
    ///
    /// # Errors
    ///
    /// Returns an error if the `center_node` does not exist in the database.
    pub fn build(&self) -> Result<String> {
        let mut output = String::new();
        let node = self.db.get_node(self.center_node)?;
        let label = Self::resolve(node.label);

        // 1. Header
        writeln!(
            &mut output,
            "# Node Context: {} ({})",
            self.center_node.as_u64(),
            label
        )
        .unwrap();

        // 2. Properties
        writeln!(&mut output, "\n## Properties").unwrap();
        if node.properties.is_empty() {
            writeln!(&mut output, "- (No properties)").unwrap();
        } else {
            // Sort keys for deterministic output
            let mut props: Vec<_> = node.properties.iter().collect();
            props.sort_by_key(|(k, _)| *k);

            for (key_id, val) in props {
                let key = Self::resolve(*key_id);
                writeln!(&mut output, "- {}: {}", key, val).unwrap();
            }
        }

        // 3. Evolution (History)
        writeln!(&mut output, "\n## Evolution").unwrap();
        let generator = NarrativeGenerator::new(self.db);
        match generator.generate_node_narrative(self.center_node) {
            Ok(events) => {
                if events.is_empty() {
                    writeln!(&mut output, "- No history available.").unwrap();
                } else {
                    for event in events.iter().rev().take(self.history_limit) {
                        writeln!(
                            &mut output,
                            "- {} (v{}): {}",
                            event.timestamp, event.version_number, event.description
                        )
                        .unwrap();
                        for change in &event.changes {
                            writeln!(&mut output, "  - {}", change).unwrap();
                        }
                    }
                    if events.len() > self.history_limit {
                        writeln!(
                            &mut output,
                            "- ... ({} more versions)",
                            events.len() - self.history_limit
                        )
                        .unwrap();
                    }
                }
            }
            Err(e) => {
                writeln!(&mut output, "- Error retrieving history: {}", e).unwrap();
            }
        }

        // 4. Neighborhood
        writeln!(&mut output, "\n## Neighborhood").unwrap();
        let edges = self.db.get_outgoing_edges(self.center_node);
        if edges.is_empty() {
            writeln!(&mut output, "- (No outgoing edges)").unwrap();
        } else {
            writeln!(
                &mut output,
                "{} outgoing edges (showing max {}):",
                edges.len(),
                self.neighbor_limit
            )
            .unwrap();
            for edge_id in edges.iter().take(self.neighbor_limit) {
                if let Ok(edge) = self.db.get_edge(*edge_id) {
                    let edge_label = Self::resolve(edge.label);
                    // Try to get target node label if possible, otherwise just ID
                    let target_desc = if let Ok(target_node) = self.db.get_node(edge.target) {
                        format!(
                            "{} ({})",
                            Self::resolve(target_node.label),
                            edge.target.as_u64()
                        )
                    } else {
                        format!("Node {}", edge.target.as_u64())
                    };

                    writeln!(&mut output, "- {} -> {}", edge_label, target_desc).unwrap();

                    // Edge properties (compact)
                    if !edge.properties.is_empty() {
                        let mut props_str: Vec<String> = edge
                            .properties
                            .iter()
                            .map(|(k, v)| format!("{}: {}", Self::resolve(*k), v))
                            .collect();
                        // Sort for deterministic output
                        props_str.sort();
                        writeln!(
                            &mut output,
                            "  - Properties: {{ {} }}",
                            props_str.join(", ")
                        )
                        .unwrap();
                    }
                }
            }
        }

        Ok(output)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::transaction::WriteOps;
    use crate::core::property::PropertyMapBuilder;

    #[test]
    fn test_graph_context_generation() {
        let db = AletheiaDB::new().unwrap();

        // 1. Create Node A (Center)
        let props1 = PropertyMapBuilder::new()
            .insert("name", "Alice")
            .insert("role", "Engineer")
            .build();
        let node_a = db.create_node("Person", props1).unwrap();

        // 2. Update Node A (History)
        db.write(|tx| {
            let props2 = PropertyMapBuilder::new()
                .insert("name", "Alice")
                .insert("role", "Senior Engineer")
                .build();
            tx.update_node(node_a, props2)
        })
        .unwrap();

        // 3. Create Node B (Neighbor)
        let props_b = PropertyMapBuilder::new()
            .insert("name", "Gallifrey Inc")
            .build();
        let node_b = db.create_node("Company", props_b).unwrap();

        // 4. Create Edge A -> B
        let props_edge = PropertyMapBuilder::new().insert("since", 2020i64).build();
        db.create_edge(node_a, node_b, "WORKS_AT", props_edge)
            .unwrap();

        // 5. Build Context
        let context = GraphContextBuilder::new(&db, node_a)
            .with_history_limit(5)
            .build()
            .unwrap();

        println!("{}", context);

        // 6. Assertions
        assert!(context.contains("# Node Context:"));
        assert!(context.contains("Person"));

        // Check Properties
        assert!(context.contains("name: \"Alice\""));
        assert!(context.contains("role: \"Senior Engineer\""));

        // Check Evolution
        assert!(context.contains("## Evolution"));
        assert!(context.contains("updated properties")); // Description
        assert!(
            context
                .contains("Modified property 'role' from '\"Engineer\"' to '\"Senior Engineer\"'")
        );

        // Check Neighborhood
        assert!(context.contains("## Neighborhood"));
        assert!(context.contains("WORKS_AT -> Company"));
        assert!(context.contains("since: 2020"));
    }
}
