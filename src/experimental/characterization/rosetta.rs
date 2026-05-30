//! Rosetta: Semantic Graph to CSV Exporter.
//!
//! "Turn the graph into tables."
//!
//! Rosetta exports the AletheiaDB knowledge graph into a CSV format
//! compatible with data science tools like Pandas, R, or Gephi.
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
//! let (nodes_csv, edges_csv) = rosetta.export_ego_graph(start_node, 2, Some(500))?;
//! println!("{}", nodes_csv);
//! # Ok(())
//! # }
//! ```

use crate::AletheiaDB;
use crate::core::error::Result;
use crate::core::id::NodeId;
use crate::core::interning::{GLOBAL_INTERNER, InternedString};
use std::collections::{HashSet, VecDeque};
use std::fmt::Write;

/// The Rosetta Exporter Engine.
#[cfg(feature = "nova")]
pub struct Rosetta<'a> {
    db: &'a AletheiaDB,
}

#[cfg(feature = "nova")]
impl<'a> Rosetta<'a> {
    /// Create a new Rosetta exporter.
    pub fn new(db: &'a AletheiaDB) -> Self {
        Self { db }
    }

    /// Exports an ego-graph centered around `start_node` up to `max_depth` hops in CSV.
    /// Returns a tuple of `(nodes_csv, edges_csv)`.
    pub fn export_ego_graph(
        &self,
        start_node: NodeId,
        max_depth: usize,
        max_nodes: Option<usize>,
    ) -> Result<(String, String)> {
        let mut visited_nodes = HashSet::new();
        let mut visited_edges = HashSet::new();
        let mut queue = VecDeque::new();

        let mut nodes_csv = String::new();
        let mut edges_csv = String::new();

        writeln!(&mut nodes_csv, "id,label,name").unwrap();
        writeln!(&mut edges_csv, "source,target,label").unwrap();

        queue.push_back((start_node, 0));
        visited_nodes.insert(start_node);

        while let Some((current_node, depth)) = queue.pop_front() {
            // Write node definition
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

            writeln!(
                &mut nodes_csv,
                "{},\"{}\",\"{}\"",
                current_node.as_u64(),
                Self::escape_csv(&label),
                Self::escape_csv(&name)
            )
            .unwrap();

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

                            writeln!(
                                &mut edges_csv,
                                "{},{},\"{}\"",
                                current_node.as_u64(),
                                target.as_u64(),
                                Self::escape_csv(&Self::resolve_str(edge.label))
                            )
                            .unwrap();
                        }
                    } else {
                        // Target already in the subgraph
                        writeln!(
                            &mut edges_csv,
                            "{},{},\"{}\"",
                            current_node.as_u64(),
                            target.as_u64(),
                            Self::escape_csv(&Self::resolve_str(edge.label))
                        )
                        .unwrap();
                    }
                }
            }
        }

        Ok((nodes_csv, edges_csv))
    }

    fn resolve_str(s: InternedString) -> String {
        GLOBAL_INTERNER
            .resolve_with(s, |s| s.to_string())
            .unwrap_or_else(|| "Unknown".to_string())
    }

    fn escape_csv(s: &str) -> String {
        s.replace('"', "\"\"")
    }
}

#[cfg(all(test, feature = "semantic-characterization"))]
mod tests {
    use super::*;
    use crate::PropertyMapBuilder;
    use crate::WriteOps;

    #[test]
    fn test_rosetta_csv_export() {
        let db = AletheiaDB::new().unwrap();

        let mut node_a = NodeId::new(1).unwrap();
        let mut node_b = NodeId::new(2).unwrap();

        db.write(|tx| {
            // Node A: Alice
            let props_a = PropertyMapBuilder::new().insert("name", "Alice").build();
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

        let rosetta = Rosetta::new(&db);
        let (nodes_csv, edges_csv) = rosetta.export_ego_graph(node_a, 1, None).unwrap();

        assert!(nodes_csv.contains("id,label,name"));
        println!("{}", nodes_csv);
        assert!(nodes_csv.contains(&format!(
            "{node},\"Person\",\"\"\"Alice\"\"\"",
            node = node_a.as_u64()
        )));
        assert!(nodes_csv.contains(&format!(
            "{node},\"Person\",\"\"\"Bob\"\"\"",
            node = node_b.as_u64()
        )));

        assert!(edges_csv.contains("source,target,label"));
        assert!(edges_csv.contains(&format!(
            "{},{},\"KNOWS\"",
            node_a.as_u64(),
            node_b.as_u64()
        )));
    }
}
