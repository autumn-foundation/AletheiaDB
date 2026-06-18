//! Cartographer: Semantic Graph to DOT Exporter.
//!
//! "Map the territory."
//!
//! Cartographer exports the AletheiaDB knowledge graph into Graphviz DOT format.
//!
//! # Example
//! ```rust,no_run
//! use aletheiadb::AletheiaDB;
//! use aletheiadb::experimental::characterization::cartographer::Cartographer;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let db = AletheiaDB::new()?;
//! let cartographer = Cartographer::new(&db);
//!
//! # let start_node = aletheiadb::core::id::NodeId::new(1).unwrap();
//! let dot = cartographer.export_ego_graph(start_node, 2, Some(500))?;
//! println!("{}", dot);
//! # Ok(())
//! # }
//! ```

use crate::AletheiaDB;
use crate::core::error::Result;
use crate::core::id::NodeId;
use crate::core::interning::{GLOBAL_INTERNER, InternedString};
use std::collections::{HashSet, VecDeque};
use std::fmt::Write;

/// The Cartographer Exporter Engine.
pub struct Cartographer<'a> {
    db: &'a AletheiaDB,
}

#[cfg(feature = "semantic-characterization")]
impl<'a> Cartographer<'a> {
    /// Create a new Cartographer exporter.
    pub fn new(db: &'a AletheiaDB) -> Self {
        Self { db }
    }

    /// Exports an ego-graph centered around `start_node` up to `max_depth` hops in DOT format.
    pub fn export_ego_graph(
        &self,
        start_node: NodeId,
        max_depth: usize,
        max_nodes: Option<usize>,
    ) -> Result<String> {
        let mut output = String::new();
        writeln!(&mut output, "digraph G {{").unwrap();

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
                if !visited_edges.insert(edge_id) {
                    continue;
                }

                if let Ok(edge) = self.db.get_edge(edge_id) {
                    let target = edge.target;
                    let is_new_target = !visited_nodes.contains(&target);

                    if is_new_target {
                        // Only include new nodes that are within the cap.
                        if max_nodes.is_none_or(|limit| visited_nodes.len() < limit) {
                            visited_nodes.insert(target);
                            queue.push_back((target, depth + 1));
                            self.write_edge(&mut output, current_node, target, edge.label)?;
                        }
                    } else {
                        // Target already in the subgraph; record cross/back-edges.
                        self.write_edge(&mut output, current_node, target, edge.label)?;
                    }
                }
            }
        }

        writeln!(&mut output, "}}").unwrap();
        Ok(output)
    }

    fn write_node(&self, output: &mut String, node_id: NodeId) -> Result<()> {
        let node = self.db.get_node(node_id)?;
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

        // Format: 1 [label="Person: Alice"];
        writeln!(
            output,
            "    {} [label=\"{}: {}\"];",
            node_id.as_u64(),
            label,
            Self::escape_dot(&name)
        )
        .unwrap();
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
        let escaped_label = Self::escape_dot(&label_str);
        // Format: 1 -> 2 [label="KNOWS"];
        writeln!(
            output,
            "    {} -> {} [label=\"{}\"];",
            source.as_u64(),
            target.as_u64(),
            escaped_label
        )
        .unwrap();
        Ok(())
    }

    fn resolve_str(s: InternedString) -> String {
        GLOBAL_INTERNER
            .resolve_with(s, |s| s.to_string())
            .unwrap_or_else(|| "Unknown".to_string())
    }

    fn escape_dot(s: &str) -> String {
        s.replace('"', "\\\"").replace('\n', "\\n")
    }
}

#[cfg(all(test, feature = "semantic-characterization"))]
mod tests {
    use super::*;
    use crate::PropertyMapBuilder;
    use crate::WriteOps;

    #[test]
    fn test_cartographer_dot_export() {
        let db = AletheiaDB::new().unwrap();

        let mut node_a = NodeId::new(1).unwrap();
        let mut node_b = NodeId::new(2).unwrap();

        db.write(|tx| {
            let props_a = PropertyMapBuilder::new().insert("name", "Alice").build();
            node_a = tx.create_node("Person", props_a).unwrap();

            let props_b = PropertyMapBuilder::new().insert("name", "Bob").build();
            node_b = tx.create_node("Person", props_b).unwrap();

            tx.create_edge(node_a, node_b, "KNOWS", Default::default())
                .unwrap();
            Ok::<(), crate::core::error::Error>(())
        })
        .unwrap();

        let cartographer = Cartographer::new(&db);
        let dot_str = cartographer.export_ego_graph(node_a, 1, None).unwrap();

        assert!(dot_str.contains("digraph G {"));
        assert!(dot_str.contains(&format!(
            "{} [label=\"Person: \\\"Alice\\\"\"];",
            node_a.as_u64()
        )));
        assert!(dot_str.contains(&format!(
            "{} [label=\"Person: \\\"Bob\\\"\"];",
            node_b.as_u64()
        )));
        assert!(dot_str.contains(&format!(
            "{} -> {} [label=\"KNOWS\"];",
            node_a.as_u64(),
            node_b.as_u64()
        )));
        assert!(dot_str.contains("}"));
    }
}
