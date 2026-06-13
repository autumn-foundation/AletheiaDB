use crate::AletheiaDB;
use crate::core::error::Result;
use crate::core::id::NodeId;
use crate::core::interning::{GLOBAL_INTERNER, InternedString};
use crate::core::property::PropertyValue;
use std::collections::{HashSet, VecDeque};
use std::fmt::Write;

/// A generated Markdown page representing a node and its context.
pub struct LorebookPage {
    /// The page title (e.g., "Person: Alice").
    pub title: String,
    /// The generated Markdown content including properties and WikiLinks.
    pub content: String,
}

/// The Lorebook Exporter Engine.
///
/// Traverses the AletheiaDB graph to generate a personal knowledge base
/// of interconnected Markdown pages.
pub struct Lorebook<'a> {
    db: &'a AletheiaDB,
}

#[cfg(feature = "semantic-characterization")]
impl<'a> Lorebook<'a> {
    /// Create a new Lorebook exporter.
    pub fn new(db: &'a AletheiaDB) -> Self {
        Self { db }
    }

    /// Exports an ego-graph centered around `start_node` up to `max_depth` hops
    /// into a vector of interconnected Markdown pages.
    pub fn export_ego_graph(
        &self,
        start_node: NodeId,
        max_depth: usize,
    ) -> Result<Vec<LorebookPage>> {
        let mut visited_nodes = HashSet::new();
        let mut queue = VecDeque::new();
        let mut pages = Vec::new();

        queue.push_back((start_node, 0));
        visited_nodes.insert(start_node);

        while let Some((current_node, depth)) = queue.pop_front() {
            let node = self.db.get_node(current_node)?;
            let label = Self::resolve_str(node.label);

            let name = if let Some(PropertyValue::String(val)) = node.get_property("name") {
                val.to_string()
            } else if let Some(PropertyValue::String(val)) = node.get_property("title") {
                val.to_string()
            } else if let Some(PropertyValue::String(val)) = node.get_property("id") {
                val.to_string()
            } else {
                label.clone()
            };

            let title = format!("{}: {}", label, name);
            let mut content = String::new();
            writeln!(&mut content, "# {}", title).unwrap();

            if !node.properties.is_empty() {
                writeln!(&mut content, "\n## Properties").unwrap();
                let mut props: Vec<_> = node.properties.iter().collect();
                props.sort_by_key(|(k, _)| *k);
                for (k, v) in props {
                    writeln!(&mut content, "- {}: {}", Self::resolve_str(*k), v).unwrap();
                }
            }

            let edges = self.db.get_outgoing_edges(current_node);
            let edges_count = edges.len();
            if edges_count > 0 {
                writeln!(&mut content, "\n## Connections").unwrap();
                for edge_id in edges {
                    if let Ok(edge) = self.db.get_edge(edge_id) {
                        let target = edge.target;

                        if depth < max_depth && !visited_nodes.contains(&target) {
                            visited_nodes.insert(target);
                            queue.push_back((target, depth + 1));
                        }

                        if let Ok(target_node) = self.db.get_node(target) {
                            let target_label = Self::resolve_str(target_node.label);
                            let target_name = if let Some(PropertyValue::String(val)) =
                                target_node.get_property("name")
                            {
                                val.to_string()
                            } else {
                                target_label.clone()
                            };
                            writeln!(
                                &mut content,
                                "- {} [[{}: {}]]",
                                Self::resolve_str(edge.label),
                                target_label,
                                target_name
                            )
                            .unwrap();
                        }
                    }
                }
            }

            pages.push(LorebookPage { title, content });
        }

        Ok(pages)
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
    use crate::PropertyMapBuilder;
    use crate::api::WriteOps;

    #[test]
    fn test_lorebook_export() {
        let db = AletheiaDB::new().unwrap();
        let mut node_a = NodeId::new(0).unwrap();
        let mut node_b = NodeId::new(0).unwrap();
        db.write(|tx| {
            let props_a = PropertyMapBuilder::new().insert("name", "Alice").build();
            node_a = tx.create_node("Person", props_a).unwrap();
            let props_b = PropertyMapBuilder::new().insert("name", "Bob").build();
            node_b = tx.create_node("Person", props_b).unwrap();
            tx.create_edge(node_a, node_b, "KNOWS", PropertyMapBuilder::new().build())
                .unwrap();
            Ok::<(), crate::core::error::Error>(())
        })
        .unwrap();

        let lorebook = Lorebook::new(&db);
        let pages = lorebook.export_ego_graph(node_a, 1).unwrap();
        assert_eq!(pages.len(), 2);

        let page_a = pages.iter().find(|p| p.title == "Person: Alice").unwrap();
        let page_b = pages.iter().find(|p| p.title == "Person: Bob").unwrap();

        assert!(page_a.content.contains("[[Person: Bob]]"));
        assert!(page_b.content.contains("Person: Bob"));
    }
}
