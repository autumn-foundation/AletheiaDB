//! Cinematographer: Semantic Time-Lapse Exporter.
//!
//! "Watch the graph evolve over time."
//!
//! Cinematographer extends the single-frame JSON exporter (`Starlight`)
//! by taking multiple snapshots over a time range. It creates a series of
//! frames that can be stitched together to animate the graph's evolution
//! (e.g., in a front-end visualization tool like 3d-force-graph with a time slider).

#![cfg(feature = "semantic-characterization")]

use crate::AletheiaDB;
use crate::core::error::Result;
use crate::core::id::NodeId;
use crate::core::interning::GLOBAL_INTERNER;
use crate::core::temporal::{TimeRange, Timestamp};
use serde_json::json;
use std::collections::{HashSet, VecDeque};

/// The Cinematographer Engine.
pub struct Cinematographer<'a> {
    db: &'a AletheiaDB,
}

impl<'a> Cinematographer<'a> {
    /// Create a new Cinematographer.
    pub fn new(db: &'a AletheiaDB) -> Self {
        Self { db }
    }

    /// Helper to resolve interned strings.
    fn resolve_str(id: crate::core::interning::InternedString) -> String {
        GLOBAL_INTERNER
            .resolve_with(id, |s| s.to_string())
            .unwrap_or_else(|| format!("Unknown_{}", id.as_u32()))
    }

    /// Exports an ego-graph as a single frame at a specific point in time.
    fn export_frame(
        &self,
        start_node: NodeId,
        max_depth: usize,
        valid_time: Timestamp,
    ) -> Result<serde_json::Value> {
        let mut visited_nodes = HashSet::new();
        let mut visited_edges = HashSet::new();
        let mut queue = VecDeque::new();

        let mut nodes_json = Vec::new();
        let mut links_json = Vec::new();

        queue.push_back((start_node, 0));
        visited_nodes.insert(start_node);

        // We use the same valid_time for transaction_time to see what the state was
        // effectively at that moment.
        let tx_time = valid_time;

        while let Some((current_node, depth)) = queue.pop_front() {
            // Write node definition
            // Skip nodes that don't exist at this time
            let node = match self.db.get_node_at_time(current_node, valid_time, tx_time) {
                Ok(n) => n,
                Err(_) => continue, // Node doesn't exist at this time, skip it and its edges
            };

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

            // Stop traversal if we've reached max depth
            if depth >= max_depth {
                continue;
            }

            // Write edges
            let outgoing_edges =
                self.db
                    .get_outgoing_edges_at_time(current_node, valid_time, tx_time);

            for edge_id in outgoing_edges {
                if !visited_edges.contains(&edge_id) {
                    visited_edges.insert(edge_id);

                    if let Ok(edge) = self.db.get_edge_at_time(edge_id, valid_time, tx_time) {
                        let edge_label = Self::resolve_str(edge.label);
                        links_json.push(json!({
                            "id": edge_id.as_u64(),
                            "source": edge.source.as_u64(),
                            "target": edge.target.as_u64(),
                            "label": edge_label,
                        }));

                        if !visited_nodes.contains(&edge.target) {
                            visited_nodes.insert(edge.target);
                            queue.push_back((edge.target, depth + 1));
                        }
                    }
                }
            }
        }

        Ok(json!({
            "timestamp": valid_time.wallclock() as u64,
            "nodes": nodes_json,
            "links": links_json,
        }))
    }

    /// Exports an ego-graph as a series of JSON frames over time.
    ///
    /// # Arguments
    ///
    /// * `start_node` - The center node of the ego network.
    /// * `max_depth` - How many hops out to include.
    /// * `time_range` - The start and end timestamps to capture.
    /// * `num_frames` - How many snapshots to take evenly spaced across the range.
    ///
    /// # Returns
    ///
    /// A JSON array string containing multiple frame objects. Each frame contains
    /// `timestamp`, `nodes`, and `links` arrays.
    pub fn export_timelapse(
        &self,
        start_node: NodeId,
        max_depth: usize,
        time_range: TimeRange,
        num_frames: usize,
    ) -> Result<String> {
        if num_frames == 0 {
            return Ok("[]".to_string());
        }

        let start_ts = time_range.start().wallclock();
        let end_ts = time_range.end().wallclock();

        let mut frames = Vec::with_capacity(num_frames);

        if num_frames == 1 {
            frames.push(self.export_frame(start_node, max_depth, Timestamp::from(start_ts))?);
        } else {
            let step = (end_ts - start_ts) / (num_frames as i64 - 1);

            for i in 0..num_frames {
                let ts = std::cmp::min(start_ts + (step * i as i64), end_ts);
                frames.push(self.export_frame(start_node, max_depth, Timestamp::from(ts))?);
            }
        }

        serde_json::to_string_pretty(&frames).map_err(|e| {
            crate::core::error::Error::other(format!("JSON serialization failed: {}", e))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::AletheiaDB;
    use crate::core::temporal::{TimeRange, Timestamp};
    use serde_json::Value;

    #[test]
    fn test_cinematographer_timelapse() {
        let db = AletheiaDB::new().unwrap();
        let center_node = db.create_node("Center", Default::default()).unwrap();

        // Add a node that's only present for a bit, etc - standard graph setup.
        let time_range = TimeRange::between(Timestamp::from(100), Timestamp::from(200)).unwrap();

        let cinematographer = Cinematographer::new(&db);
        let num_frames = 5;

        let json_result = cinematographer
            .export_timelapse(center_node, 1, time_range, num_frames)
            .unwrap();
        let frames: Vec<Value> = serde_json::from_str(&json_result).unwrap();

        assert_eq!(
            frames.len(),
            num_frames,
            "Should generate the requested number of frames"
        );
    }
}
