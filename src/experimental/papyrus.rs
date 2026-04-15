//! Papyrus: Semantic Graph to Mermaid JS Exporter.
//!
//! "Visualize the invisible."
//!
//! Papyrus is an exporter that traverses the AletheiaDB knowledge graph
//! and generates a Mermaid JS flowchart. This is perfect for rendering
//! interactive graphs in Markdown, GitHub READMEs, or LLM chat interfaces.
//!
//! # Example
//! ```rust,no_run
//! use aletheiadb::AletheiaDB;
//! use aletheiadb::experimental::papyrus::Papyrus;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let db = AletheiaDB::new()?;
//! let papyrus = Papyrus::new(&db);
//!
//! # let start_node = aletheiadb::core::id::NodeId::new(1).unwrap();
//! // Export the ego-network around a specific node up to 2 hops
//! let mermaid_chart = papyrus.export_ego_graph(start_node, 2)?;
//! println!("{}", mermaid_chart);
//! # Ok(())
//! # }
//! ```

use crate::AletheiaDB;
use crate::core::error::Result;
use crate::core::id::NodeId;
use crate::core::interning::{GLOBAL_INTERNER, InternedString};
use std::collections::{HashSet, VecDeque};
use std::fmt::Write;

/// The Papyrus Exporter Engine.
pub struct Papyrus<'a> {
    db: &'a AletheiaDB,
}

#[cfg(feature = "nova")]
impl<'a> Papyrus<'a> {
    /// Create a new Papyrus exporter.
    pub fn new(db: &'a AletheiaDB) -> Self {
        Self { db }
    }

    /// Exports an ego-graph centered around `start_node` up to `max_depth` hops.
    pub fn export_ego_graph(&self, start_node: NodeId, max_depth: usize) -> Result<String> {
        let mut output = String::new();
        writeln!(&mut output, "graph TD").unwrap();

        let mut visited_nodes = HashSet::new();
        let mut visited_edges = HashSet::new();
        let mut queue = VecDeque::new();

        queue.push_back((start_node, 0));
        visited_nodes.insert(start_node);

        while let Some((current_node, depth)) = queue.pop_front() {
            // Write node definition
            self.write_node(&mut output, current_node)?;

            if depth >= max_depth {
                continue;
            }

            // Get outgoing edges
            let edges = self.db.get_outgoing_edges(current_node);
            for edge_id in edges {
                if visited_edges.contains(&edge_id) {
                    continue;
                }
                visited_edges.insert(edge_id);

                if let Ok(edge) = self.db.get_edge(edge_id) {
                    let target = edge.target;

                    // Add target to queue if not visited
                    if !visited_nodes.contains(&target) {
                        visited_nodes.insert(target);
                        queue.push_back((target, depth + 1));
                    }

                    // Write edge
                    self.write_edge(&mut output, current_node, target, edge.label)?;
                }
            }
        }

        Ok(output)
    }

    fn write_node(&self, output: &mut String, node_id: NodeId) -> Result<()> {
        if let Ok(node) = self.db.get_node(node_id) {
            let label = Self::resolve_str(node.label);

            // Try to find a human-readable name property
            let name = if let Some(val) = node.get_property("name") {
                val.to_string()
            } else if let Some(val) = node.get_property("title") {
                val.to_string()
            } else if let Some(val) = node.get_property("id") {
                val.to_string()
            } else {
                label.clone()
            };

            // Format: N1["Person: Alice"]
            writeln!(
                output,
                "    N{}[\"{}: {}\"]",
                node_id.as_u64(),
                label,
                self.escape_mermaid(&name)
            )
            .unwrap();
        }
        Ok(())
    }

    fn write_edge(
        &self,
        output: &mut String,
        source: NodeId,
        target: NodeId,
        label: InternedString,
    ) -> Result<()> {
        let label_str = Self::resolve_str(label);
        // Format: N1 -->|"KNOWS"| N2
        writeln!(
            output,
            "    N{} -->|\"{}\"| N{}",
            source.as_u64(),
            label_str,
            target.as_u64()
        )
        .unwrap();
        Ok(())
    }

    fn resolve_str(s: InternedString) -> String {
        GLOBAL_INTERNER
            .resolve_with(s, |s| s.to_string())
            .unwrap_or_else(|| "Unknown".to_string())
    }

    fn escape_mermaid(&self, s: &str) -> String {
        s.replace('"', "'")
    }
}

#[cfg(all(test, feature = "nova"))]
mod tests {
    use super::*;
    use crate::core::property::PropertyMapBuilder;

    #[test]
    fn test_papyrus_mermaid_export() {
        let db = AletheiaDB::new().unwrap();

        // Node A: Alice
        let props_a = PropertyMapBuilder::new().insert("name", "Alice").build();
        let node_a = db.create_node("Person", props_a).unwrap();

        // Node B: Bob
        let props_b = PropertyMapBuilder::new().insert("name", "Bob").build();
        let node_b = db.create_node("Person", props_b).unwrap();

        // Edge A -> B
        db.create_edge(node_a, node_b, "KNOWS", Default::default())
            .unwrap();

        let papyrus = Papyrus::new(&db);
        let chart = papyrus.export_ego_graph(node_a, 1).unwrap();

        assert!(chart.contains("graph TD"));
        assert!(chart.contains(&format!("N{}[\"Person: 'Alice'\"]", node_a.as_u64())));
        assert!(chart.contains(&format!("N{}[\"Person: 'Bob'\"]", node_b.as_u64())));
        assert!(chart.contains(&format!(
            "N{} -->|\"KNOWS\"| N{}",
            node_a.as_u64(),
            node_b.as_u64()
        )));
    }

    #[test]
    fn test_papyrus_max_depth() {
        let db = AletheiaDB::new().unwrap();

        let a = db
            .create_node(
                "Node",
                PropertyMapBuilder::new().insert("name", "A").build(),
            )
            .unwrap();
        let b = db
            .create_node(
                "Node",
                PropertyMapBuilder::new().insert("name", "B").build(),
            )
            .unwrap();
        let c = db
            .create_node(
                "Node",
                PropertyMapBuilder::new().insert("name", "C").build(),
            )
            .unwrap();

        db.create_edge(a, b, "L1", Default::default()).unwrap();
        db.create_edge(b, c, "L2", Default::default()).unwrap();

        let papyrus = Papyrus::new(&db);

        // Depth 1: should only see A and B
        let chart1 = papyrus.export_ego_graph(a, 1).unwrap();
        assert!(chart1.contains("A"));
        assert!(chart1.contains("B"));
        assert!(!chart1.contains("C"), "Depth 1 should not include node C");

        // Depth 2: should see all
        let chart2 = papyrus.export_ego_graph(a, 2).unwrap();
        assert!(chart2.contains("C"));
    }
}
