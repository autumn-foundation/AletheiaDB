//! Hologram: Semantic Graph to Interactive 3D Exporter.
//!
//! "Visualize the invisible in 3D."

#![cfg(feature = "semantic-characterization")]

use crate::AletheiaDB;
use crate::core::error::Result;
use crate::core::id::NodeId;
use crate::core::interning::{GLOBAL_INTERNER, InternedString};
use serde_json::json;
use std::collections::{HashSet, VecDeque};

/// The Hologram 3D Exporter Engine.
pub struct Hologram<'a> {
    db: &'a AletheiaDB,
}

impl<'a> Hologram<'a> {
    /// Create a new Hologram exporter.
    pub fn new(db: &'a AletheiaDB) -> Self {
        Self { db }
    }

    /// Exports an ego-graph centered around `start_node` up to `max_depth` hops
    /// as a self-contained interactive 3D HTML page.
    pub fn export_interactive_3d(
        &self,
        start_node: NodeId,
        max_depth: usize,
        max_nodes: Option<usize>,
    ) -> Result<String> {
        let mut visited_nodes = HashSet::new();
        let mut visited_edges = HashSet::new();
        let mut queue = VecDeque::new();

        queue.push_back((start_node, 0));
        visited_nodes.insert(start_node);

        let mut nodes_data = Vec::new();
        let mut links_data = Vec::new();

        while let Some((current_node, depth)) = queue.pop_front() {
            let node = self.db.get_node(current_node)?;
            let label = Self::resolve_str(node.label);

            let name = if let Some(val) = node.get_property("name") {
                val.to_string()
            } else if let Some(val) = node.get_property("title") {
                val.to_string()
            } else if let Some(val) = node.get_property("id") {
                val.to_string()
            } else {
                label.clone()
            };

            nodes_data.push(json!({
                "id": current_node.as_u64(),
                "name": name,
                "label": label,
                "val": 1
            }));

            if depth >= max_depth {
                continue;
            }

            let edges = self.db.get_outgoing_edges(current_node);
            for edge_id in edges {
                if !visited_edges.insert(edge_id) {
                    continue;
                }

                if let Ok(edge) = self.db.get_edge(edge_id) {
                    let target = edge.target;
                    let is_new_target = !visited_nodes.contains(&target);

                    let edge_label = Self::resolve_str(edge.label);

                    if is_new_target {
                        if max_nodes.is_none_or(|limit| visited_nodes.len() < limit) {
                            visited_nodes.insert(target);
                            queue.push_back((target, depth + 1));

                            links_data.push(json!({
                                "source": current_node.as_u64(),
                                "target": target.as_u64(),
                                "name": edge_label
                            }));
                        }
                    } else {
                        links_data.push(json!({
                            "source": current_node.as_u64(),
                            "target": target.as_u64(),
                            "name": edge_label
                        }));
                    }
                }
            }
        }

        let graph_data = json!({
            "nodes": nodes_data,
            "links": links_data
        });

        // Ensure JSON output is sanitized to prevent XSS.
        let mut json_str = serde_json::to_string(&graph_data).unwrap();
        json_str = json_str.replace("<", "\\u003c");

        let html = format!(
            r#"
<!DOCTYPE html>
<html>
<head>
    <title>Hologram 3D Graph</title>
    <style> body {{ margin: 0; }} </style>
    <script src="https://unpkg.com/3d-force-graph"></script>
</head>
<body>
    <div id="3d-graph"></div>
    <script>
        const gData = {};

        const Graph = ForceGraph3D()
            (document.getElementById('3d-graph'))
            .graphData(gData)
            .nodeLabel('name')
            .nodeAutoColorBy('label')
            .linkLabel('name');
    </script>
</body>
</html>
"#,
            json_str
        );

        Ok(html)
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
    use crate::core::property::PropertyMapBuilder;

    #[test]
    fn test_hologram_export() {
        let db = AletheiaDB::new().unwrap();
        let props = PropertyMapBuilder::new().insert("name", "Alice").build();
        let node = db.create_node("Person", props).unwrap();

        let hologram = Hologram::new(&db);
        let chart = hologram.export_interactive_3d(node, 1, None).unwrap();

        assert!(chart.contains("3d-force-graph"));
        assert!(chart.contains("Alice"));
    }

    #[test]
    fn test_hologram_sanitization() {
        let db = AletheiaDB::new().unwrap();
        let props = PropertyMapBuilder::new()
            .insert("name", "<script>alert('xss')</script>")
            .build();
        let node = db.create_node("Person", props).unwrap();

        let hologram = Hologram::new(&db);
        let chart = hologram.export_interactive_3d(node, 1, None).unwrap();

        assert!(chart.contains("\\u003cscript"));
        assert!(!chart.contains("<script>alert('xss')</script>"));
    }
}
