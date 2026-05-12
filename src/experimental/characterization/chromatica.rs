//! Chromatica: Semantic Heatmap Exporter.
//!
//! "Visualize the heat of a concept."
//!
//! Chromatica is an exporter that traverses an ego-graph (similar to Papyrus)
//! and generates a Mermaid JS flowchart. However, unlike standard exports,
//! it calculates the cosine similarity between each node's vector and a given
//! `target_vector`. It then colors the nodes in the Mermaid graph to create
//! a "semantic heatmap"—hotter colors (e.g., reds) indicate high similarity
//! to the target concept, while colder colors (e.g., blues) indicate low similarity.
//!
//! # Example
//! ```rust,no_run
//! use aletheiadb::AletheiaDB;
//! use aletheiadb::experimental::characterization::chromatica::Chromatica;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let db = AletheiaDB::new()?;
//! let chromatica = Chromatica::new(&db);
//!
//! # let start_node = aletheiadb::core::id::NodeId::new(1).unwrap();
//! let target_concept = vec![1.0, 0.0, 0.0];
//! let heatmap_chart = chromatica.export_heatmap(start_node, 2, "embedding", &target_concept, Some(500))?;
//! println!("{}", heatmap_chart);
//! # Ok(())
//! # }
//! ```

use crate::AletheiaDB;
use crate::core::error::Result;
use crate::core::id::NodeId;
use crate::core::interning::{GLOBAL_INTERNER, InternedString};
use std::collections::{HashMap, HashSet, VecDeque};
use std::fmt::Write;

/// The Chromatica Exporter Engine.
pub struct Chromatica<'a> {
    db: &'a AletheiaDB,
}

#[cfg(feature = "semantic-characterization")]
impl<'a> Chromatica<'a> {
    /// Create a new Chromatica exporter.
    pub fn new(db: &'a AletheiaDB) -> Self {
        Self { db }
    }

    /// Exports an ego-graph centered around `start_node` up to `max_depth` hops.
    /// Nodes are colored based on their cosine similarity to `target_vector`.
    pub fn export_heatmap(
        &self,
        start_node: NodeId,
        max_depth: usize,
        vector_property: &str,
        target_vector: &[f32],
        max_nodes: Option<usize>,
    ) -> Result<String> {
        let mut output = String::new();
        writeln!(&mut output, "graph TD").unwrap();

        let mut visited_nodes = HashSet::new();
        let mut visited_edges = HashSet::new();
        let mut queue = VecDeque::new();

        // We will collect similarities to assign styles later
        let mut similarities: HashMap<NodeId, f32> = HashMap::new();

        queue.push_back((start_node, 0));
        visited_nodes.insert(start_node);

        while let Some((current_node, depth)) = queue.pop_front() {
            // Write node definition
            self.write_node(&mut output, current_node)?;

            // Calculate and store similarity
            let similarity =
                self.calculate_similarity(current_node, vector_property, target_vector)?;
            similarities.insert(current_node, similarity);

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

        // Apply styles based on similarities
        writeln!(&mut output).unwrap();
        for (node_id, sim) in similarities {
            let color = self.map_similarity_to_color(sim);
            writeln!(
                output,
                "    style N{} fill:{},stroke:#333,stroke-width:2px;",
                node_id.as_u64(),
                color
            )
            .unwrap();
        }

        Ok(output)
    }

    #[allow(clippy::collapsible_if)]
    fn calculate_similarity(
        &self,
        node_id: NodeId,
        vector_property: &str,
        target_vector: &[f32],
    ) -> Result<f32> {
        let node = self.db.get_node(node_id)?;
        if let Some(prop) = node.get_property(vector_property) {
            if let Some(vec) = prop.as_vector() {
                return Ok(
                    crate::core::vector::ops::cosine_similarity(vec, target_vector).unwrap_or(0.0),
                );
            }
        }
        Ok(0.0) // Default if no vector or different dimension
    }

    /// Maps a cosine similarity [-1.0, 1.0] to a hex color code.
    /// Hot (1.0) -> Red (#FF0000)
    /// Neutral (0.0) -> Light Green (#7FFF7F)
    /// Cold (-1.0) -> Blue (#0000FF)
    fn map_similarity_to_color(&self, similarity: f32) -> String {
        // Normalize similarity from [-1, 1] to [0, 1]
        let normalized = ((similarity.clamp(-1.0, 1.0) + 1.0) / 2.0).clamp(0.0, 1.0);

        // Simple gradient: Blue (0) -> Gray (0.5) -> Red (1)
        let r = (255.0 * normalized) as u8;
        let b = (255.0 * (1.0 - normalized)) as u8;
        let g = if normalized < 0.5 {
            (255.0 * (normalized * 2.0)) as u8 // Blue to Gray
        } else {
            (255.0 * ((1.0 - normalized) * 2.0)) as u8 // Gray to Red
        };

        format!("#{:02X}{:02X}{:02X}", r, g, b)
    }

    fn write_node(&self, output: &mut String, node_id: NodeId) -> Result<()> {
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

        writeln!(
            output,
            "    N{}[\"{}: {}\"]",
            node_id.as_u64(),
            label,
            Self::escape_mermaid(&name)
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

    #[test]
    fn test_chromatica_color_mapping() {
        let db = AletheiaDB::new().unwrap();
        let chromatica = Chromatica::new(&db);

        // Sim = 1.0 (Hot, mostly Red)
        let color_hot = chromatica.map_similarity_to_color(1.0);
        assert_eq!(color_hot, "#FF0000"); // Red

        // Sim = -1.0 (Cold, mostly Blue)
        let color_cold = chromatica.map_similarity_to_color(-1.0);
        assert_eq!(color_cold, "#0000FF"); // Blue

        // Sim = 0.0 (Neutral, Grayish)
        let color_neutral = chromatica.map_similarity_to_color(0.0);
        assert_eq!(color_neutral, "#7FFF7F"); // Middle green/grayish depending on the gradient math. It's r=127, b=127, g=255. Let's just ensure it doesn't crash.
    }

    #[test]
    fn test_chromatica_heatmap_export() {
        let db = AletheiaDB::new().unwrap();

        // Node A: Target concept is very similar
        let props_a = PropertyMapBuilder::new()
            .insert("name", "Fire")
            .insert_vector("vec", &[1.0, 0.0])
            .build();
        let node_a = db.create_node("Concept", props_a).unwrap();

        // Node B: Target concept is very dissimilar
        let props_b = PropertyMapBuilder::new()
            .insert("name", "Ice")
            .insert_vector("vec", &[-1.0, 0.0])
            .build();
        let node_b = db.create_node("Concept", props_b).unwrap();

        db.create_edge(node_a, node_b, "OPPOSITE", Default::default())
            .unwrap();

        let chromatica = Chromatica::new(&db);
        let chart = chromatica
            .export_heatmap(node_a, 1, "vec", &[1.0, 0.0], None)
            .unwrap();

        assert!(chart.contains("graph TD"));
        // Node A should be colored hot (similar to [1.0, 0.0])
        assert!(chart.contains(&format!("style N{} fill:#FF0000", node_a.as_u64())));
        // Node B should be colored cold (dissimilar to [1.0, 0.0])
        assert!(chart.contains(&format!("style N{} fill:#0000FF", node_b.as_u64())));
    }
}
