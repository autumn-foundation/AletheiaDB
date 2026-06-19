//! Obsidian: Semantic Graph to Markdown Vault Exporter.
//!
//! "Build your personal knowledge base."
//!
//! Obsidian exports the AletheiaDB knowledge graph into a collection of Markdown files
//! compatible with Obsidian and other local knowledge base tools. Each node becomes a
//! Markdown file with YAML frontmatter for properties, and edges become wikilinks `[[Link]]`.
//!
//! # Example
//! ```rust,no_run
//! use aletheiadb::AletheiaDB;
//! use aletheiadb::experimental::characterization::obsidian::Obsidian;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let db = AletheiaDB::new()?;
//! let obsidian = Obsidian::new(&db);
//!
//! # let start_node = aletheiadb::core::id::NodeId::new(1).unwrap();
//! let vault_files = obsidian.export_ego_graph(start_node, 2, Some(500))?;
//! for (filename, content) in vault_files {
//!     println!("--- {} ---\n{}", filename, content);
//! }
//! # Ok(())
//! # }
//! ```

use crate::AletheiaDB;
use crate::core::error::Result;
use crate::core::id::NodeId;
use crate::core::interning::GLOBAL_INTERNER;
use std::collections::{HashMap, HashSet, VecDeque};
use std::fmt::Write;

/// The Obsidian Exporter Engine.
pub struct Obsidian<'a> {
    db: &'a AletheiaDB,
}

#[cfg(feature = "semantic-characterization")]
impl<'a> Obsidian<'a> {
    /// Create a new Obsidian exporter.
    pub fn new(db: &'a AletheiaDB) -> Self {
        Self { db }
    }

    /// Exports an ego-graph centered around `start_node` up to `max_depth` hops.
    /// Returns a map of filenames (e.g., "Node Name.md") to their markdown content.
    pub fn export_ego_graph(
        &self,
        start_node: NodeId,
        max_depth: usize,
        max_nodes: Option<usize>,
    ) -> Result<HashMap<String, String>> {
        let mut visited_nodes = HashSet::new();
        let mut queue = VecDeque::new();
        let mut vault_files = HashMap::new();

        queue.push_back((start_node, 0));
        visited_nodes.insert(start_node);

        while let Some((current_node, depth)) = queue.pop_front() {
            let mut content = String::new();

            // Get node properties
            let node = self.db.get_node(current_node)?;
            let label = Self::resolve_str(node.label);

            let name = if let Some(val) = node.get_property("name") {
                val.to_string()
                    .trim_matches('"')
                    .replace("/", "_")
                    .replace("\\", "_")
            } else if let Some(val) = node.get_property("title") {
                val.to_string()
                    .trim_matches('"')
                    .replace("/", "_")
                    .replace("\\", "_")
            } else {
                format!("{}_{}", label, current_node.as_u64())
            };

            let filename = format!("{}.md", name);

            // Write YAML frontmatter
            writeln!(&mut content, "---").unwrap();
            writeln!(&mut content, "id: {}", current_node.as_u64()).unwrap();
            writeln!(&mut content, "label: {}", label).unwrap();

            for (k, v) in node.properties.iter() {
                let key_str = Self::resolve_str(*k);
                if key_str != "name" && key_str != "title" {
                    writeln!(&mut content, "{}: {}", key_str, v).unwrap();
                }
            }
            writeln!(&mut content, "---\n").unwrap();
            writeln!(&mut content, "# {}", name).unwrap();
            writeln!(&mut content, "\n## Relationships\n").unwrap();

            // Get outgoing edges
            let edges = self.db.get_outgoing_edges(current_node);
            for edge_id in edges {
                if let Ok(edge) = self.db.get_edge(edge_id) {
                    let target = edge.target;

                    // Resolve target name for the wikilink
                    let target_name = if let Ok(t_node) = self.db.get_node(target) {
                        if let Some(val) = t_node.get_property("name") {
                            val.to_string()
                                .trim_matches('"')
                                .replace("/", "_")
                                .replace("\\", "_")
                        } else if let Some(val) = t_node.get_property("title") {
                            val.to_string()
                                .trim_matches('"')
                                .replace("/", "_")
                                .replace("\\", "_")
                        } else {
                            let t_label = Self::resolve_str(t_node.label);
                            format!("{}_{}", t_label, target.as_u64())
                        }
                    } else {
                        format!("Unknown_{}", target.as_u64())
                    };

                    let edge_label = Self::resolve_str(edge.label);
                    writeln!(&mut content, "- **{}** [[{}]]", edge_label, target_name).unwrap();

                    #[allow(clippy::unnecessary_map_or)]
                    let is_new_target = !visited_nodes.contains(&target);
                    if is_new_target
                        && depth < max_depth
                        && max_nodes.is_none_or(|limit| visited_nodes.len() < limit)
                    {
                        visited_nodes.insert(target);
                        queue.push_back((target, depth + 1));
                    }
                }
            }

            vault_files.insert(filename, content);
        }

        Ok(vault_files)
    }

    fn resolve_str(interned: crate::core::interning::InternedString) -> String {
        GLOBAL_INTERNER
            .resolve_with(interned, |s| s.to_string())
            .unwrap_or_else(|| "Unknown".to_string())
    }
}

#[cfg(all(test, feature = "semantic-characterization"))]
mod tests {
    use super::*;
    use crate::api::transaction::WriteOps;
    use crate::core::property::PropertyMapBuilder;

    #[test]
    fn test_obsidian_export_ego_graph() -> Result<()> {
        let db = AletheiaDB::new()?;

        let alice = db.write(|tx| {
            let alice = tx.create_node(
                "Person",
                PropertyMapBuilder::new()
                    .insert("name", "Alice")
                    .insert("age", 30)
                    .build(),
            )?;
            let bob = tx.create_node(
                "Person",
                PropertyMapBuilder::new().insert("name", "Bob").build(),
            )?;
            tx.create_edge(alice, bob, "KNOWS", Default::default())?;
            Ok::<_, crate::core::error::Error>(alice)
        })?;

        let obsidian = Obsidian::new(&db);
        let files = obsidian.export_ego_graph(alice, 1, None)?;

        assert_eq!(files.len(), 2);

        let alice_content = files.get("Alice.md").expect("Alice.md should exist");
        assert!(alice_content.contains("---"));
        assert!(alice_content.contains("label: Person"));
        assert!(alice_content.contains("age: 30"));
        assert!(alice_content.contains("# Alice"));
        assert!(alice_content.contains("- **KNOWS** [[Bob]]"));

        let bob_content = files.get("Bob.md").expect("Bob.md should exist");
        assert!(bob_content.contains("---"));
        assert!(bob_content.contains("label: Person"));
        assert!(bob_content.contains("# Bob"));

        Ok(())
    }
}
