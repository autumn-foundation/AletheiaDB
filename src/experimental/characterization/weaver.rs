//! Weaver: Generates Mermaid.js flowcharts from a node's semantic context.
//!
//! Weaver traverses a node's ego-graph (its neighbors up to a certain depth)
//! and exports it as a Mermaid diagram string. This provides an instant visual
//! representation of a concept's local structure.

use crate::AletheiaDB;
use crate::core::error::Result;
use crate::core::id::NodeId;
use crate::core::interning::GLOBAL_INTERNER;
use std::collections::{HashSet, VecDeque};

/// Weaver generates visualizations of the graph context.
pub struct Weaver<'a> {
    db: &'a AletheiaDB,
}

impl<'a> Weaver<'a> {
    /// Create a new Weaver instance.
    pub fn new(db: &'a AletheiaDB) -> Self {
        Self { db }
    }

    /// Export a node's ego-graph (up to `max_depth`) as a Mermaid flowchart.
    pub fn export_ego_graph_mermaid(&self, start_node: NodeId, max_depth: usize) -> Result<String> {
        let mut mermaid = String::from("graph TD;\n");
        let mut visited_nodes = HashSet::new();
        let mut visited_edges = HashSet::new();
        let mut queue = VecDeque::new();

        queue.push_back((start_node, 0));
        visited_nodes.insert(start_node);

        while let Some((node_id, depth)) = queue.pop_front() {
            let node = self.db.get_node(node_id)?;

            // Resolve node label string
            let label = GLOBAL_INTERNER
                .resolve_with(node.label, |s| s.to_string())
                .unwrap_or_else(|| "Unknown".to_string());

            let name = node
                .properties
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("Unknown");

            mermaid.push_str(&format!("    N{}[{}: {}]\n", node_id.as_u64(), label, name));

            // Traverse outgoing edges even at max_depth to find cross/back edges
            for edge_id in self.db.get_outgoing_edges(node_id) {
                if !visited_edges.insert(edge_id) {
                    continue;
                }

                let edge = self.db.get_edge(edge_id)?;
                let target_id = edge.target;

                // Edge label
                let edge_label = GLOBAL_INTERNER
                    .resolve_with(edge.label, |s| s.to_string())
                    .unwrap_or_else(|| "UNKNOWN".to_string());

                mermaid.push_str(&format!(
                    "    N{} -->|{}| N{}\n",
                    node_id.as_u64(),
                    edge_label,
                    target_id.as_u64()
                ));

                if depth < max_depth && visited_nodes.insert(target_id) {
                    queue.push_back((target_id, depth + 1));
                }
            }
        }

        Ok(mermaid)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::transaction::WriteOps;
    use crate::core::property::PropertyMapBuilder;

    #[test]
    fn test_weaver_export_mermaid() {
        let db = AletheiaDB::new().unwrap();

        let mut n1 = NodeId::new(0).unwrap();
        let mut n2 = NodeId::new(0).unwrap();
        let mut n3 = NodeId::new(0).unwrap();

        db.write(|tx| {
            n1 = tx
                .create_node(
                    "Person",
                    PropertyMapBuilder::new().insert("name", "Alice").build(),
                )
                .unwrap();

            n2 = tx
                .create_node(
                    "Person",
                    PropertyMapBuilder::new().insert("name", "Bob").build(),
                )
                .unwrap();

            n3 = tx
                .create_node(
                    "Company",
                    PropertyMapBuilder::new().insert("name", "Acme").build(),
                )
                .unwrap();

            tx.create_edge(n1, n2, "KNOWS", Default::default()).unwrap();
            tx.create_edge(n2, n3, "WORKS_AT", Default::default())
                .unwrap();

            Ok::<(), crate::core::error::Error>(())
        })
        .unwrap();

        let weaver = Weaver::new(&db);
        let mermaid = weaver.export_ego_graph_mermaid(n1, 2).unwrap();

        assert!(mermaid.starts_with("graph TD;"));
        let n1_id = n1.as_u64();
        let n2_id = n2.as_u64();
        let n3_id = n3.as_u64();
        assert!(mermaid.contains(&format!("N{}[Person: Alice]", n1_id)));
        assert!(mermaid.contains(&format!("N{}[Person: Bob]", n2_id)));
        assert!(mermaid.contains(&format!("N{}[Company: Acme]", n3_id)));
        assert!(mermaid.contains(&format!("N{} -->|KNOWS| N{}", n1_id, n2_id)));
        assert!(mermaid.contains(&format!("N{} -->|WORKS_AT| N{}", n2_id, n3_id)));
    }
}
