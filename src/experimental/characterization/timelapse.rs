//! TimeLapse: Semantic Graph Timeline Exporter.
//!
//! "How did the graph evolve over time?"
//!
//! TimeLapse exports a series of ego-graphs centered around a node over a given time range.
//! It returns an array of JSON objects, each representing the graph state at a specific point in time,
//! allowing visualization libraries to animate the graph's evolution.
//!
//! # Example
//! ```rust,no_run
//! use aletheiadb::AletheiaDB;
//! use aletheiadb::experimental::characterization::timelapse::TimeLapse;
//! use aletheiadb::core::temporal::TimeRange;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let db = AletheiaDB::new()?;
//! let timelapse = TimeLapse::new(&db);
//!
//! # let start_node = aletheiadb::core::id::NodeId::new(1).unwrap();
//! # let time_range = TimeRange::new(0.into(), 1.into()).unwrap();
//! let json = timelapse.export_ego_timeline(start_node, time_range, 10, 2, Some(500))?;
//! println!("{}", json);
//! # Ok(())
//! # }
//! ```

use crate::AletheiaDB;
use crate::core::error::Result;
use crate::core::id::NodeId;
use crate::core::interning::{GLOBAL_INTERNER, InternedString};
use crate::core::temporal::{TimeRange, Timestamp};
use serde_json::json;
use std::collections::{HashSet, VecDeque};

/// The TimeLapse Exporter Engine.
pub struct TimeLapse<'a> {
    db: &'a AletheiaDB,
}

#[cfg(feature = "semantic-characterization")]
impl<'a> TimeLapse<'a> {
    /// Create a new TimeLapse exporter.
    pub fn new(db: &'a AletheiaDB) -> Self {
        Self { db }
    }

    /// Exports a series of ego-graphs centered around `start_node` over `time_range`.
    ///
    /// The `time_range` is divided into `num_frames` equally spaced points in time.
    /// For each point, it generates an ego-graph up to `max_depth` hops.
    pub fn export_ego_timeline(
        &self,
        start_node: NodeId,
        time_range: TimeRange,
        num_frames: usize,
        max_depth: usize,
        max_nodes: Option<usize>,
    ) -> Result<String> {
        if num_frames == 0 {
            return Ok("[]".to_string());
        }

        let start_time = time_range.start().wallclock();
        let end_time = time_range.end().wallclock();
        let step = if num_frames > 1 {
            (end_time - start_time) / (num_frames as i64 - 1)
        } else {
            0
        };

        let mut frames_json = Vec::new();

        for i in 0..num_frames {
            let current_time: Timestamp = (start_time + step * (i as i64)).into();

            let mut visited_nodes = HashSet::new();
            let mut visited_edges = HashSet::new();
            let mut queue = VecDeque::new();

            let mut nodes_json = Vec::new();
            let mut links_json = Vec::new();

            // Check if start node exists at this time
            if self
                .db
                .get_node_at_time(start_node, current_time, current_time)
                .is_ok()
            {
                queue.push_back((start_node, 0));
                visited_nodes.insert(start_node);

                while let Some((current_node, depth)) = queue.pop_front() {
                    // Write node definition
                    let node =
                        self.db
                            .get_node_at_time(current_node, current_time, current_time)?;
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

                    nodes_json.push(json!({
                        "id": current_node.as_u64(),
                        "label": label,
                        "name": name,
                    }));

                    if depth >= max_depth {
                        continue;
                    }

                    // Get outgoing edges
                    let edges = self.db.get_outgoing_edges_at_time(
                        current_node,
                        current_time,
                        current_time,
                    );
                    for edge_id in edges {
                        if !visited_edges.insert(edge_id) {
                            continue;
                        }

                        if let Ok(edge) =
                            self.db
                                .get_edge_at_time(edge_id, current_time, current_time)
                        {
                            let target = edge.target;

                            // Check if target node exists at this time
                            if self.db.get_node_at_time(target, current_time, current_time).is_ok()
                            {
                                let is_new_target = !visited_nodes.contains(&target);

                                if is_new_target {
                                    // Only include new nodes that are within the cap.
                                    if max_nodes.is_none_or(|limit| visited_nodes.len() < limit) {
                                        visited_nodes.insert(target);
                                        queue.push_back((target, depth + 1));

                                        links_json.push(json!({
                                            "source": current_node.as_u64(),
                                            "target": target.as_u64(),
                                            "label": Self::resolve_str(edge.label),
                                        }));
                                    }
                                } else {
                                    // Target already in the subgraph
                                    links_json.push(json!({
                                        "source": current_node.as_u64(),
                                        "target": target.as_u64(),
                                        "label": Self::resolve_str(edge.label),
                                    }));
                                }
                            }
                        }
                    }
                }
            }

            frames_json.push(json!({
                "time": current_time.wallclock(),
                "nodes": nodes_json,
                "links": links_json,
            }));
        }

        serde_json::to_string(&frames_json)
            .map_err(|e| crate::core::error::Error::other(e.to_string()))
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
    use std::time::Duration;

    #[test]
    fn test_timelapse_export() {
        let db = AletheiaDB::new().unwrap();

        let t_start = crate::core::temporal::time::now();
        std::thread::sleep(Duration::from_millis(50));

        let mut node_a = NodeId::new(1).unwrap();

        let (_, _t_mid) = db
            .write_with_timestamp(|tx| {
                let props_a = PropertyMapBuilder::new().insert("name", "Alice").build();
                node_a = tx.create_node("Person", props_a).unwrap();
                Ok::<(), crate::core::error::Error>(())
            })
            .unwrap();

        std::thread::sleep(Duration::from_millis(50));

        let (_, t_end) = db
            .write_with_timestamp(|tx| {
                let props_b = PropertyMapBuilder::new().insert("name", "Bob").build();
                let node_b = tx.create_node("Person", props_b).unwrap();
                tx.create_edge(node_a, node_b, "KNOWS", Default::default())
                    .unwrap();
                Ok::<(), crate::core::error::Error>(())
            })
            .unwrap();

        // Give it a tiny bit of time after t_end to ensure it is inclusive
        let end_time: Timestamp = (t_end.wallclock() + 1).into();

        let timelapse = TimeLapse::new(&db);
        let time_range = TimeRange::new(t_start, end_time).unwrap();

        let json_str = timelapse
            .export_ego_timeline(node_a, time_range, 3, 1, None)
            .unwrap();

        let parsed: serde_json::Value = serde_json::from_str(&json_str).unwrap();
        let frames = parsed.as_array().unwrap();

        assert_eq!(frames.len(), 3);

        // Frame 1 (t_start): Node A doesn't exist yet
        let frame_1_nodes = frames[0]["nodes"].as_array().unwrap();
        assert_eq!(frame_1_nodes.len(), 0);

        // Frame 2 (t_mid): Node A exists, but no connections
        let frame_2_nodes = frames[1]["nodes"].as_array().unwrap();
        assert_eq!(frame_2_nodes.len(), 1);
        let frame_2_links = frames[1]["links"].as_array().unwrap();
        assert_eq!(frame_2_links.len(), 0);

        // Frame 3 (t_end): Node A and B exist, and are connected
        let frame_3_nodes = frames[2]["nodes"].as_array().unwrap();
        assert_eq!(frame_3_nodes.len(), 2);
        let frame_3_links = frames[2]["links"].as_array().unwrap();
        assert_eq!(frame_3_links.len(), 1);
    }
}
