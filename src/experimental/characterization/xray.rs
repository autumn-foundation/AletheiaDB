//! XRay: Semantic Dissonance Graph Exporter.
//!
//! "See the cracks in the structure."
//!
//! XRay combines the structural visualization of Papyrus with the anomaly
//! detection of the DissonanceEngine. It exports a Mermaid JS flowchart where
//! nodes are colored based on their semantic dissonance score.
//!
//! High dissonance nodes (outliers) are highlighted, allowing users to visually
//! identify semantic stress points in their knowledge graph.
//!
//! # Example
//! ```rust,no_run
//! use aletheiadb::AletheiaDB;
//! use aletheiadb::experimental::characterization::xray::XRay;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let db = AletheiaDB::new()?;
//! let xray = XRay::new(&db);
//!
//! # let start_node = aletheiadb::core::id::NodeId::new(1).unwrap();
//! // Export an ego-graph centered around `start_node`, coloring by dissonance on "embedding"
//! let mermaid_chart = xray.export_dissonance_graph(start_node, 2, "embedding", Some(500))?;
//! println!("{}", mermaid_chart);
//! # Ok(())
//! # }
//! ```

use crate::AletheiaDB;
use crate::core::error::Result;
use crate::core::id::NodeId;
use crate::core::interning::{GLOBAL_INTERNER, InternedString};
use crate::experimental::diagnostics::dissonance::DissonanceEngine;
use std::collections::{HashSet, VecDeque};
use std::fmt::Write;

/// The XRay Exporter Engine.
pub struct XRay<'a> {
    db: &'a AletheiaDB,
}

#[cfg(feature = "semantic-characterization")]
impl<'a> XRay<'a> {
    /// Create a new XRay exporter.
    pub fn new(db: &'a AletheiaDB) -> Self {
        Self { db }
    }

    /// Exports a Mermaid ego-graph with nodes styled based on their semantic dissonance.
    pub fn export_dissonance_graph(
        &self,
        start_node: NodeId,
        max_depth: usize,
        vector_property: &str,
        max_nodes: Option<usize>,
    ) -> Result<String> {
        let mut output = String::new();
        writeln!(&mut output, "graph TD").unwrap();

        // Setup styles
        writeln!(
            &mut output,
            "    classDef normal fill:#f9f9f9,stroke:#333,stroke-width:2px;"
        )
        .unwrap();
        writeln!(
            &mut output,
            "    classDef warning fill:#ffcc00,stroke:#333,stroke-width:2px;"
        )
        .unwrap();
        writeln!(
            &mut output,
            "    classDef danger fill:#ff4444,stroke:#333,stroke-width:2px,color:#fff;"
        )
        .unwrap();

        let mut visited_nodes = HashSet::new();
        let mut visited_edges = HashSet::new();
        let mut queue = VecDeque::new();

        let dissonance_engine = DissonanceEngine::new(self.db);

        queue.push_back((start_node, 0));
        visited_nodes.insert(start_node);

        while let Some((current_node, depth)) = queue.pop_front() {
            // Write node definition
            self.write_node(
                &mut output,
                current_node,
                &dissonance_engine,
                vector_property,
            )?;

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
                        if max_nodes.is_none_or(|limit| visited_nodes.len() < limit) {
                            visited_nodes.insert(target);
                            queue.push_back((target, depth + 1));
                            self.write_edge(&mut output, current_node, target, edge.label)?;
                        }
                    } else {
                        self.write_edge(&mut output, current_node, target, edge.label)?;
                    }
                }
            }
        }

        Ok(output)
    }

    fn write_node(
        &self,
        output: &mut String,
        node_id: NodeId,
        dissonance_engine: &DissonanceEngine<'_>,
        vector_property: &str,
    ) -> Result<()> {
        let node = self.db.get_node(node_id)?;
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

        // Calculate dissonance
        let dissonance_score = dissonance_engine
            .calculate_dissonance(node_id, vector_property)
            .unwrap_or(0.0);

        let style_class = if dissonance_score > 0.5 {
            "danger"
        } else if dissonance_score > 0.2 {
            "warning"
        } else {
            "normal"
        };

        // Format: N1["Person: Alice<br/>Dissonance: 0.12"]:::normal
        writeln!(
            output,
            "    N{}[\"{}: {}<br/>Dissonance: {:.2}\"]:::{}",
            node_id.as_u64(),
            label,
            Self::escape_mermaid(&name),
            dissonance_score,
            style_class
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
        let escaped_label = Self::escape_mermaid(&label_str);
        writeln!(
            output,
            "    N{} -->|\"{}\"| N{}",
            source.as_u64(),
            escaped_label,
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

    fn escape_mermaid(s: &str) -> String {
        s.replace('"', "'").replace('\n', "<br/>")
    }
}

#[cfg(all(test, feature = "semantic-characterization"))]
mod tests {
    use super::*;
    use crate::core::property::PropertyMapBuilder;
    use crate::index::vector::{DistanceMetric, HnswConfig};

    #[test]
    fn test_xray_mermaid_export() {
        let db = AletheiaDB::new().unwrap();
        db.enable_vector_index("vec", HnswConfig::new(2, DistanceMetric::Cosine))
            .unwrap();

        // Node A: [1.0, 0.0]
        let props_a = PropertyMapBuilder::new()
            .insert("name", "Alice")
            .insert_vector("vec", &[1.0, 0.0])
            .build();
        let node_a = db.create_node("Person", props_a).unwrap();

        // Node B: [0.0, 1.0] (Dissimilar to A, should cause high dissonance if connected)
        let props_b = PropertyMapBuilder::new()
            .insert("name", "Bob")
            .insert_vector("vec", &[0.0, 1.0])
            .build();
        let node_b = db.create_node("Person", props_b).unwrap();

        // Add distractors to make KNN meaningful
        for _ in 0..5 {
            db.create_node(
                "Person",
                PropertyMapBuilder::new()
                    .insert_vector("vec", &[0.99, 0.01])
                    .build(),
            )
            .unwrap();
        }

        // Edge A -> B
        db.create_edge(node_a, node_b, "KNOWS", Default::default())
            .unwrap();

        let xray = XRay::new(&db);
        let chart = xray
            .export_dissonance_graph(node_a, 1, "vec", None)
            .unwrap();

        assert!(chart.contains("graph TD"));
        assert!(chart.contains("classDef normal"));
        assert!(chart.contains("classDef warning"));
        assert!(chart.contains("classDef danger"));

        // A should be danger or warning because it connects to an orthogonal vector
        assert!(chart.contains(&format!("N{}", node_a.as_u64())));
        assert!(chart.contains("Dissonance:"));

        assert!(chart.contains(&format!(
            "N{} -->|\"KNOWS\"| N{}",
            node_a.as_u64(),
            node_b.as_u64()
        )));
    }
}
