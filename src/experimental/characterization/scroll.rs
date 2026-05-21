//! Scroll: Semantic Graph to JSON Exporter.
//!
//! "Export the invisible."
//!
//! Scroll is an exporter that traverses the AletheiaDB knowledge graph
//! and generates a JSON snapshot of an ego-network. This is perfect for
//! sending a localized context window to an LLM, rendering graphs in web UI,
//! or archiving specific subgraphs.
//!
//! # Example
//! ```rust,no_run
//! use aletheiadb::AletheiaDB;
//! use aletheiadb::experimental::scroll::Scroll;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let db = AletheiaDB::new()?;
//! let scroll = Scroll::new(&db);
//!
//! # let start_node = aletheiadb::core::id::NodeId::new(1).unwrap();
//! // Export the ego-network around a specific node up to 2 hops, capped at 500 nodes
//! let json_chart = scroll.export_ego_graph_json(start_node, 2, Some(500))?;
//! println!("{}", json_chart);
//! # Ok(())
//! # }
//! ```

use crate::AletheiaDB;
use crate::core::error::Result;
use crate::core::id::NodeId;
use crate::core::interning::{GLOBAL_INTERNER, InternedString};
use crate::core::property::{PropertyMap, PropertyValue};
use serde_json::{Map, Value, json};
use std::collections::{HashSet, VecDeque};

/// Exports graph data to JSON formats.
pub struct Scroll<'a> {
    db: &'a AletheiaDB,
}

impl<'a> Scroll<'a> {
    /// Creates a new `Scroll` exporter.
    pub fn new(db: &'a AletheiaDB) -> Self {
        Self { db }
    }

    /// Helper to resolve interned keys to strings.
    fn resolve_key(key_id: InternedString) -> String {
        GLOBAL_INTERNER
            .resolve_with(key_id, |s| s.to_string())
            .unwrap_or_else(|| "unknown".to_string())
    }

    /// Converts a `PropertyValue` to a `serde_json::Value`.
    fn property_value_to_json(val: &PropertyValue) -> Value {
        match val {
            PropertyValue::Null => Value::Null,
            PropertyValue::Bool(b) => Value::Bool(*b),
            PropertyValue::Int(i) => json!(*i),
            PropertyValue::Float(f) => json!(*f),
            PropertyValue::String(s) => Value::String(s.to_string()),
            PropertyValue::Bytes(b) => json!(b.to_vec()),
            PropertyValue::Array(arr) => {
                Value::Array(arr.iter().map(Self::property_value_to_json).collect())
            }
            PropertyValue::Vector(vec) => Value::Array(vec.iter().map(|f| json!(*f)).collect()),
            PropertyValue::SparseVector(sv) => {
                json!({
                    "indices": sv.indices(),
                    "values": sv.values(),
                })
            }
        }
    }

    /// Converts a `PropertyMap` to a `serde_json::Map`.
    fn property_map_to_json(props: &PropertyMap) -> Map<String, Value> {
        let mut map = Map::new();
        for (key_id, val) in props.iter() {
            let key_str = Self::resolve_key(*key_id);
            map.insert(key_str, Self::property_value_to_json(val));
        }
        map
    }

    /// Traverses the ego-network starting from `start_node` up to `max_depth` hops
    /// and exports it as a JSON String.
    ///
    /// The resulting JSON has the format:
    /// `{ "nodes": [...], "edges": [...] }`
    ///
    /// # Arguments
    ///
    /// * `start_node` - The central node of the ego network.
    /// * `max_depth` - Maximum number of hops from the center.
    /// * `max_nodes` - Optional cap on the total number of nodes exported.
    pub fn export_ego_graph_json(
        &self,
        start_node: NodeId,
        max_depth: usize,
        max_nodes: Option<usize>,
    ) -> Result<String> {
        let mut visited_nodes = HashSet::new();
        let mut visited_edges = HashSet::new();
        let mut queue = VecDeque::new();

        let mut nodes_json = Vec::new();
        let mut edges_json = Vec::new();

        queue.push_back((start_node, 0));
        visited_nodes.insert(start_node);

        while let Some((current_node, depth)) = queue.pop_front() {
            // Read and format current node
            let node = self.db.get_node(current_node)?;
            let label_str = Self::resolve_key(node.label);

            nodes_json.push(json!({
                "id": current_node.as_u64(),
                "label": label_str,
                "properties": Self::property_map_to_json(&node.properties),
            }));

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
                    let edge_label_str = Self::resolve_key(edge.label);

                    let edge_json = json!({
                        "id": edge_id.as_u64(),
                        "source": current_node.as_u64(),
                        "target": target.as_u64(),
                        "label": edge_label_str,
                        "properties": Self::property_map_to_json(&edge.properties),
                    });

                    if is_new_target {
                        // Only include new nodes that are within the cap.
                        if max_nodes.is_none_or(|limit| visited_nodes.len() < limit) {
                            visited_nodes.insert(target);
                            queue.push_back((target, depth + 1));
                            edges_json.push(edge_json);
                        }
                    } else {
                        // Target already in the subgraph; record cross/back-edges.
                        edges_json.push(edge_json);
                    }
                }
            }
        }

        let result = json!({
            "nodes": nodes_json,
            "edges": edges_json,
        });

        Ok(result.to_string())
    }
}

#[cfg(all(test, feature = "semantic-characterization"))]
mod tests {
    use super::*;
    use crate::core::property::PropertyMapBuilder;

    #[test]
    fn test_export_ego_graph_json() {
        let db = AletheiaDB::new().unwrap();

        let props1 = PropertyMapBuilder::new()
            .insert("name", "Alice")
            .insert("age", 30)
            .build();
        let alice_id = db.create_node("Person", props1).unwrap();

        let props2 = PropertyMapBuilder::new()
            .insert("name", "Bob")
            .insert("age", 35)
            .build();
        let bob_id = db.create_node("Person", props2).unwrap();

        let edge_props = PropertyMapBuilder::new().insert("since", 2020).build();
        db.create_edge(alice_id, bob_id, "KNOWS", edge_props)
            .unwrap();

        let scroll = Scroll::new(&db);
        let json_str = scroll.export_ego_graph_json(alice_id, 1, None).unwrap();

        let parsed: serde_json::Value = serde_json::from_str(&json_str).unwrap();

        assert!(parsed.is_object());
        let obj = parsed.as_object().unwrap();

        assert!(obj.contains_key("nodes"));
        assert!(obj.contains_key("edges"));

        let nodes = obj.get("nodes").unwrap().as_array().unwrap();
        assert_eq!(nodes.len(), 2);

        let edges = obj.get("edges").unwrap().as_array().unwrap();
        assert_eq!(edges.len(), 1);

        // Find Alice
        let alice = nodes
            .iter()
            .find(|n| n["label"] == "Person" && n["properties"]["name"] == "Alice")
            .unwrap();
        assert_eq!(alice["id"], alice_id.as_u64());
        assert_eq!(alice["properties"]["age"], 30);

        // Find Edge
        let edge = &edges[0];
        assert_eq!(edge["source"], alice_id.as_u64());
        assert_eq!(edge["target"], bob_id.as_u64());
        assert_eq!(edge["label"], "KNOWS");
        assert_eq!(edge["properties"]["since"], 2020);
    }
}
