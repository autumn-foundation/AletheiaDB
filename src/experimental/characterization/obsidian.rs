//! Obsidian Exporter: Semantic Graph to PKM Markdown Exporter.
//!
//! "Weave the web."
//!
//! Obsidian Exporter takes an AletheiaDB knowledge graph and exports it
//! as a collection of Markdown files optimized for Obsidian and other
//! Personal Knowledge Management (PKM) tools. Edges are represented as
//! `[[wikilinks]]`, allowing you to visually traverse your graph using Obsidian's
//! graph view.
//!
//! # Example
//! ```rust,no_run
//! use aletheiadb::AletheiaDB;
//! use aletheiadb::experimental::characterization::obsidian::ObsidianExporter;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let db = AletheiaDB::new()?;
//! let exporter = ObsidianExporter::new(&db);
//!
//! # let start_node = aletheiadb::core::id::NodeId::new(1).unwrap();
//! // Export an ego-graph to Markdown files (filename -> content map)
//! let files = exporter.export_ego_graph(start_node, 2, Some(500))?;
//!
//! for (filename, content) in files {
//!     println!("File: {}", filename);
//!     println!("{}", content);
//! }
//! # Ok(())
//! # }
//! ```

use crate::AletheiaDB;
use crate::core::error::Result;
use crate::core::id::NodeId;
use crate::core::interning::{GLOBAL_INTERNER, InternedString};
use std::collections::{HashMap, HashSet, VecDeque};
use std::fmt::Write;

/// The Obsidian Exporter Engine.
pub struct ObsidianExporter<'a> {
    db: &'a AletheiaDB,
}

#[cfg(feature = "semantic-characterization")]
impl<'a> ObsidianExporter<'a> {
    /// Create a new Obsidian exporter.
    pub fn new(db: &'a AletheiaDB) -> Self {
        Self { db }
    }

    /// Exports an ego-graph centered around `start_node` up to `max_depth` hops.
    ///
    /// Returns a mapping of suggested filenames (e.g. `Person_Alice.md`) to their
    /// Markdown content. Each file contains node properties as YAML frontmatter
    /// and outgoing edges as `[[wikilinks]]`.
    pub fn export_ego_graph(
        &self,
        start_node: NodeId,
        max_depth: usize,
        max_nodes: Option<usize>,
    ) -> Result<HashMap<String, String>> {
        let mut visited_nodes = HashSet::new();
        let mut queue = VecDeque::new();
        let mut files = HashMap::new();

        queue.push_back((start_node, 0));
        visited_nodes.insert(start_node);

        while let Some((current_node, depth)) = queue.pop_front() {
            let (filename, content) = self.build_markdown_for_node(current_node)?;
            files.insert(filename, content);

            if depth >= max_depth {
                continue;
            }

            // Get outgoing edges to continue traversal
            let edges = self.db.get_outgoing_edges(current_node);
            for edge_id in edges {
                if let Ok(edge) = self.db.get_edge(edge_id) {
                    let target = edge.target;
                    let is_new_target = !visited_nodes.contains(&target);

                    if is_new_target && max_nodes.is_none_or(|limit| visited_nodes.len() < limit) {
                        visited_nodes.insert(target);
                        queue.push_back((target, depth + 1));
                    }
                }
            }
        }

        Ok(files)
    }

    fn build_markdown_for_node(&self, node_id: NodeId) -> Result<(String, String)> {
        let node = self.db.get_node(node_id)?;
        let label = Self::resolve_str(node.label);

        // Determine node "name" for the filename and title
        let name = if let Some(val) = node.get_property("name") {
            val.to_string()
        } else if let Some(val) = node.get_property("title") {
            val.to_string()
        } else if let Some(val) = node.get_property("id") {
            val.to_string()
        } else {
            format!("Node_{}", node_id.as_u64())
        };

        // Clean filename: remove quotes and spaces for cleaner files
        let clean_name = name.trim_matches('"').replace([' ', '/'], "_");
        let filename = format!("{}_{}.md", label, clean_name);

        let mut output = String::new();

        // 1. YAML Frontmatter for properties
        writeln!(&mut output, "---").unwrap();
        writeln!(&mut output, "id: {}", node_id.as_u64()).unwrap();
        writeln!(&mut output, "label: {}", label).unwrap();

        for (key, val) in node.properties.iter() {
            let key_str = Self::resolve_str(*key);
            // Quick heuristic to skip printing massive vectors in frontmatter
            if val.as_vector().is_some() {
                writeln!(&mut output, "{}: [vector omitted]", key_str).unwrap();
            } else {
                writeln!(&mut output, "{}: {}", key_str, val).unwrap();
            }
        }
        writeln!(&mut output, "---").unwrap();
        writeln!(&mut output).unwrap();

        // 2. Title
        writeln!(&mut output, "# {} ({})", name.trim_matches('"'), label).unwrap();
        writeln!(&mut output).unwrap();

        // 3. Edges (Wikilinks)
        let mut outgoing = Vec::new();
        for edge_id in self.db.get_outgoing_edges(node_id) {
            if let Ok(edge) = self.db.get_edge(edge_id) {
                let target_name = self.get_node_name(edge.target);
                let edge_label = Self::resolve_str(edge.label);
                outgoing.push(format!("- **{}** [[{}]]", edge_label, target_name));
            }
        }

        let mut incoming = Vec::new();
        for edge_id in self.db.get_incoming_edges(node_id) {
            if let Ok(edge) = self.db.get_edge(edge_id) {
                let source_name = self.get_node_name(edge.source);
                let edge_label = Self::resolve_str(edge.label);
                incoming.push(format!("- **{}** [[{}]]", edge_label, source_name));
            }
        }

        if !outgoing.is_empty() {
            writeln!(&mut output, "## Outgoing Connections").unwrap();
            for link in outgoing {
                writeln!(&mut output, "{}", link).unwrap();
            }
            writeln!(&mut output).unwrap();
        }

        if !incoming.is_empty() {
            writeln!(&mut output, "## Incoming Connections").unwrap();
            for link in incoming {
                writeln!(&mut output, "{}", link).unwrap();
            }
            writeln!(&mut output).unwrap();
        }

        Ok((filename, output))
    }

    fn get_node_name(&self, node_id: NodeId) -> String {
        if let Ok(node) = self.db.get_node(node_id) {
            let label = Self::resolve_str(node.label);
            let name = if let Some(val) = node.get_property("name") {
                val.to_string()
            } else if let Some(val) = node.get_property("title") {
                val.to_string()
            } else if let Some(val) = node.get_property("id") {
                val.to_string()
            } else {
                format!("Node_{}", node_id.as_u64())
            };
            let clean_name = name.trim_matches('"').replace([' ', '/'], "_");
            format!("{}_{}", label, clean_name)
        } else {
            format!("Unknown_{}", node_id.as_u64())
        }
    }

    fn resolve_str(s: InternedString) -> String {
        GLOBAL_INTERNER
            .resolve_with(s, |s| s.to_string())
            .unwrap_or_else(|| "Unknown".to_string())
    }
}

#[cfg(all(test, feature = "semantic-characterization"))]
mod tests {
    use super::*;
    use crate::WriteOps;
    use crate::core::property::PropertyMapBuilder;

    #[test]
    fn test_obsidian_export() {
        let db = AletheiaDB::new().unwrap();

        let mut node_a = NodeId::new(1).unwrap();
        let mut node_b = NodeId::new(2).unwrap();

        db.write(|tx| {
            // Node A: Alice
            let props_a = PropertyMapBuilder::new()
                .insert("name", "Alice")
                .insert("age", 30)
                .build();
            node_a = tx.create_node("Person", props_a).unwrap();

            // Node B: Bob
            let props_b = PropertyMapBuilder::new().insert("name", "Bob").build();
            node_b = tx.create_node("Person", props_b).unwrap();

            // Edge A -> B
            tx.create_edge(node_a, node_b, "KNOWS", Default::default())
                .unwrap();
            Ok::<(), crate::core::error::Error>(())
        })
        .unwrap();

        let exporter = ObsidianExporter::new(&db);
        let files = exporter.export_ego_graph(node_a, 1, None).unwrap();

        // Should have 2 files (Alice and Bob)
        assert_eq!(files.len(), 2);

        let alice_content = files.get("Person_Alice.md").unwrap();
        let bob_content = files.get("Person_Bob.md").unwrap();

        // Check Frontmatter
        assert!(alice_content.contains("---"));
        assert!(alice_content.contains("age: 30"));
        assert!(alice_content.contains("label: Person"));

        // Check Markdown Links
        assert!(alice_content.contains("[[Person_Bob]]"));
        assert!(bob_content.contains("[[Person_Alice]]"));
    }
}
