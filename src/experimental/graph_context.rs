use crate::GallifreyDB;
use crate::core::id::NodeId;
use crate::core::interning::{GLOBAL_INTERNER, InternedString};
use crate::experimental::temporal_narrative::NarrativeGenerator;
use crate::utils::error::Result;
use std::fmt::Write;

/// Builder for generating a rich context description of a node.
///
/// This generates a Markdown-formatted string containing the node's current state,
/// recent history (evolution), and immediate neighborhood. It is designed for
/// injecting context into LLM prompts.
pub struct GraphContextBuilder<'a> {
    db: &'a GallifreyDB,
    center_node: NodeId,
    history_limit: usize,
    neighbor_limit: usize,
}

impl<'a> GraphContextBuilder<'a> {
    /// Create a new builder for the given node.
    pub fn new(db: &'a GallifreyDB, center_node: NodeId) -> Self {
        Self {
            db,
            center_node,
            history_limit: 5,
            neighbor_limit: 10,
        }
    }

    /// Set the maximum number of history events to include.
    pub fn with_history_limit(mut self, limit: usize) -> Self {
        self.history_limit = limit;
        self
    }

    /// Set the maximum number of neighbors to include.
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
        let db = GallifreyDB::new().unwrap();

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
