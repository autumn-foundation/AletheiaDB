//! Rosetta: Semantic Graph to Cypher Exporter.
//!
//! "Speak their language."
//!
//! Rosetta exports an AletheiaDB knowledge graph into standard Cypher
//! `CREATE` statements. This allows you to easily transport subgraphs
//! from AletheiaDB into Neo4j, Memgraph, or simply share reproducible
//! graph structures in documentation.
//!
//! # Example
//! ```rust,no_run
//! use aletheiadb::AletheiaDB;
//! use aletheiadb::experimental::characterization::rosetta::Rosetta;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let db = AletheiaDB::new()?;
//! let rosetta = Rosetta::new(&db);
//!
//! # let start_node = aletheiadb::core::id::NodeId::new(1).unwrap();
//! // Export the ego-network around a specific node up to 2 hops
//! let cypher_script = rosetta.export_ego_graph(start_node, 2, Some(500))?;
//! println!("{}", cypher_script);
//! # Ok(())
//! # }
//! ```

use crate::AletheiaDB;
use crate::core::error::Result;
use crate::core::id::NodeId;
use crate::core::interning::{GLOBAL_INTERNER, InternedString};
use crate::core::property::PropertyValue;
use std::collections::{HashSet, VecDeque};
use std::fmt::Write;

/// The Rosetta Exporter Engine.
pub struct Rosetta<'a> {
    db: &'a AletheiaDB,
}

#[cfg(feature = "semantic-characterization")]
impl<'a> Rosetta<'a> {
    /// Create a new Rosetta exporter.
    pub fn new(db: &'a AletheiaDB) -> Self {
        Self { db }
    }

    /// Exports an ego-graph centered around `start_node` up to `max_depth` hops
    /// into a single Cypher `CREATE` statement.
    pub fn export_ego_graph(
        &self,
        start_node: NodeId,
        max_depth: usize,
        max_nodes: Option<usize>,
    ) -> Result<String> {
        let mut output = String::new();
        writeln!(&mut output, "CREATE").unwrap();

        let mut visited_nodes = HashSet::new();
        let mut visited_edges = HashSet::new();
        let mut queue = VecDeque::new();

        let mut edges_to_write = Vec::new();
        let mut first_item = true;

        queue.push_back((start_node, 0));
        visited_nodes.insert(start_node);

        while let Some((current_node, depth)) = queue.pop_front() {
            if !first_item {
                writeln!(&mut output, ",").unwrap();
            }
            first_item = false;

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
                        if max_nodes.is_none_or(|limit| visited_nodes.len() < limit) {
                            visited_nodes.insert(target);
                            queue.push_back((target, depth + 1));
                            edges_to_write.push((current_node, target, edge.label));
                        }
                    } else {
                        edges_to_write.push((current_node, target, edge.label));
                    }
                }
            }
        }

        for (source, target, label) in edges_to_write {
            if !first_item {
                writeln!(&mut output, ",").unwrap();
            }
            first_item = false;
            self.write_edge(&mut output, source, target, label)?;
        }

        writeln!(&mut output, ";").unwrap();
        Ok(output)
    }

    fn write_node(&self, output: &mut String, node_id: NodeId) -> Result<()> {
        let node = self.db.get_node(node_id)?;
        let label = Self::resolve_str(node.label);

        write!(output, "  (n{}:`{}`", node_id.as_u64(), label).unwrap();

        let mut props = Vec::new();
        for (k, v) in node.properties.iter() {
            let key_str = GLOBAL_INTERNER
                .resolve_with(*k, |s| s.to_string())
                .unwrap_or_else(|| "unknown".to_string());

            let val_str = match v {
                PropertyValue::String(s) => format!("\"{}\"", s.replace("\"", "\\\"")),
                PropertyValue::Int(i) => format!("{}", i),
                PropertyValue::Float(f) => format!("{}", f),
                PropertyValue::Bool(b) => format!("{}", b),
                _ => continue, // skip vectors and bytes for cypher simplicity
            };
            props.push(format!("`{}`: {}", key_str, val_str));
        }

        if !props.is_empty() {
            // Sort to ensure deterministic output for testing
            props.sort();
            write!(output, " {{{}}}", props.join(", ")).unwrap();
        }

        write!(output, ")").unwrap();
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
        write!(
            output,
            "  (n{})-[:`{}`]->(n{})",
            source.as_u64(),
            label_str,
            target.as_u64()
        ).unwrap();
        Ok(())
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
    use crate::api::transaction::WriteOps;

    #[test]
    fn test_rosetta_cypher_export() {
        let db = AletheiaDB::new().unwrap();

        let mut node_a = NodeId::new(1).unwrap();
        let mut node_b = NodeId::new(2).unwrap();

        db.write(|tx| {
            let props_a = PropertyMapBuilder::new()
                .insert("name", "Alice")
                .insert("age", 30)
                .build();
            node_a = tx.create_node("Person", props_a).unwrap();

            let props_b = PropertyMapBuilder::new().insert("name", "Bob").build();
            node_b = tx.create_node("Person", props_b).unwrap();

            tx.create_edge(node_a, node_b, "KNOWS", Default::default())
                .unwrap();
            Ok::<(), crate::core::error::Error>(())
        })
        .unwrap();

        let rosetta = Rosetta::new(&db);
        let cypher = rosetta.export_ego_graph(node_a, 1, None).unwrap();

        let node_a_str = format!("(n{}:`Person`", node_a.as_u64());
        let node_b_str = format!("(n{}:`Person`", node_b.as_u64());

        assert!(cypher.contains("CREATE"));
        assert!(cypher.contains(&node_a_str));
        assert!(cypher.contains("`name`: \"Alice\""));
        assert!(cypher.contains("`age`: 30"));
        assert!(cypher.contains(&node_b_str));
        assert!(cypher.contains("`name`: \"Bob\""));
        assert!(cypher.contains(&format!(
            "(n{})-[:`KNOWS`]->(n{})",
            node_a.as_u64(),
            node_b.as_u64()
        )));
        assert!(cypher.ends_with(";\n"));
    }
}
