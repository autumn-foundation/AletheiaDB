//! Cascade: Temporal Propagation Tracker.
//!
//! Tracks how a specific property state propagates through the graph over time,
//! similar to an epidemiological spread or rumor cascade.

use crate::AletheiaDB;
use crate::core::error::Result;
use crate::core::id::NodeId;
use crate::core::property::PropertyValue;
use crate::core::temporal::{TimeRange, Timestamp};
use std::collections::{HashMap, VecDeque};

/// Represents a single adoption event in a temporal cascade.
#[derive(Debug, Clone, PartialEq)]
pub struct CascadeEvent {
    /// The node that adopted the property.
    pub node_id: NodeId,
    /// When the node adopted the property.
    pub adopted_at: Timestamp,
    /// The node that propagated the property to this node (if any).
    pub parent_node: Option<NodeId>,
}

/// The Cascade Detector Engine.
pub struct CascadeDetector<'a> {
    db: &'a AletheiaDB,
}

#[cfg(feature = "semantic-temporal")]
impl<'a> CascadeDetector<'a> {
    /// Create a new CascadeDetector instance.
    pub fn new(db: &'a AletheiaDB) -> Self {
        Self { db }
    }

    /// Traces the propagation of a property value starting from a given node.
    pub fn trace_cascade(
        &self,
        start_node: NodeId,
        property_name: &str,
        target_value: &PropertyValue,
        time_window: TimeRange,
    ) -> Result<Vec<CascadeEvent>> {
        let mut cascade = Vec::new();
        let mut adopted_times = HashMap::new();
        let mut queue = VecDeque::new();

        // 1. Find when start_node adopted the property
        let start_time =
            self.find_adoption_time(start_node, property_name, target_value, time_window.start())?;
        if let Some(t) = start_time {
            if time_window.contains(t) {
                cascade.push(CascadeEvent {
                    node_id: start_node,
                    adopted_at: t,
                    parent_node: None,
                });
                adopted_times.insert(start_node, t);
                queue.push_back((start_node, t));
            } else {
                return Ok(cascade);
            }
        } else {
            return Ok(cascade);
        }

        // 2. BFS to trace propagation
        while let Some((current_node, current_time)) = queue.pop_front() {
            let edges = self.db.get_outgoing_edges(current_node);
            for edge_id in edges {
                if let Ok(target) = self.db.get_edge_target(edge_id) {
                    if adopted_times.contains_key(&target) {
                        continue;
                    }

                    // Check if target adopted AFTER current_node
                    if let Ok(Some(target_time)) =
                        self.find_adoption_time(target, property_name, target_value, current_time)
                        && target_time >= current_time
                        && time_window.contains(target_time)
                    {
                        cascade.push(CascadeEvent {
                            node_id: target,
                            adopted_at: target_time,
                            parent_node: Some(current_node),
                        });
                        adopted_times.insert(target, target_time);
                        queue.push_back((target, target_time));
                    }
                }
            }
        }

        // Sort chronologically
        cascade.sort_by_key(|e| e.adopted_at.wallclock());
        Ok(cascade)
    }

    fn find_adoption_time(
        &self,
        node: NodeId,
        property_name: &str,
        target_value: &PropertyValue,
        after: Timestamp,
    ) -> Result<Option<Timestamp>> {
        let history = self.db.get_node_history(node)?;
        for v in &history.versions {
            let vt_start = v.temporal.valid_time().start();
            if vt_start.wallclock() >= after.wallclock()
                && let Some(val) = v.properties.get(property_name)
                && val == target_value
            {
                return Ok(Some(vt_start));
            }
        }
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::transaction::WriteOps;
    use crate::core::property::PropertyMapBuilder;
    use crate::core::temporal::time;
    use std::time::Duration;

    #[test]
    fn test_cascade_detection() {
        let db = AletheiaDB::new().unwrap();

        let mut node_a = NodeId::new(0).unwrap();
        let mut node_b = NodeId::new(0).unwrap();
        let mut node_c = NodeId::new(0).unwrap();

        db.write(|tx| {
            node_a = tx
                .create_node_with_valid_time(
                    "Person",
                    PropertyMapBuilder::new().insert("infected", false).build(),
                    None,
                )
                .unwrap();
            node_b = tx
                .create_node_with_valid_time(
                    "Person",
                    PropertyMapBuilder::new().insert("infected", false).build(),
                    None,
                )
                .unwrap();
            node_c = tx
                .create_node_with_valid_time(
                    "Person",
                    PropertyMapBuilder::new().insert("infected", false).build(),
                    None,
                )
                .unwrap();

            tx.create_edge_with_valid_time(
                node_a,
                node_b,
                "KNOWS",
                PropertyMapBuilder::new().build(),
                None,
            )
            .unwrap();
            tx.create_edge_with_valid_time(
                node_b,
                node_c,
                "KNOWS",
                PropertyMapBuilder::new().build(),
                None,
            )
            .unwrap();
            Ok::<(), crate::core::error::Error>(())
        })
        .unwrap();

        let t_start = time::now();
        std::thread::sleep(Duration::from_millis(10));

        db.write(|tx| {
            tx.update_node_with_valid_time(
                node_a,
                PropertyMapBuilder::new().insert("infected", true).build(),
                None,
            )
        })
        .unwrap();
        std::thread::sleep(Duration::from_millis(10));

        db.write(|tx| {
            tx.update_node_with_valid_time(
                node_b,
                PropertyMapBuilder::new().insert("infected", true).build(),
                None,
            )
        })
        .unwrap();
        std::thread::sleep(Duration::from_millis(10));

        let t_end = time::now();

        let detector = CascadeDetector::new(&db);
        let window = TimeRange::new(t_start, t_end).unwrap();
        let events = detector
            .trace_cascade(node_a, "infected", &PropertyValue::Bool(true), window)
            .unwrap();

        assert_eq!(events.len(), 2);
        assert_eq!(events[0].node_id, node_a);
        assert_eq!(events[0].parent_node, None);
        assert_eq!(events[1].node_id, node_b);
        assert_eq!(events[1].parent_node, Some(node_a));
        assert!(events[0].adopted_at < events[1].adopted_at);
    }
}
