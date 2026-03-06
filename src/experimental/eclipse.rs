//! Eclipse: Semantic Shadowing Detector.
//!
//! "When did 'Machine Learning' eclipse 'Expert Systems'?"
//!
//! Eclipse detects when a newer node functionally replaces an older node of similar meaning.
//! It identifies pairs of nodes that are semantically similar but structurally divergent
//! over a specific time window.
//!
//! # Concepts
//! - **Eclipsed Node**: An older concept that is losing structural activity (fewer new edges).
//! - **Eclipsing Node**: A newer concept with high semantic similarity to the eclipsed node,
//!   but with significantly higher structural activity.
//!
//! # How it works
//! 1. **Similarity Search**: Finds nodes that are semantically similar.
//! 2. **Temporal Activity**: Computes the number of new edges added within a specific time window.
//! 3. **Comparison**: Flags pairs where the newer node has substantially more activity than the older one.

use crate::AletheiaDB;
use crate::api::transaction::ReadOps;
use crate::core::error::Result;
use crate::core::id::NodeId;
use crate::core::temporal::Timestamp;

/// Represents a detected semantic eclipse.
#[derive(Debug, Clone, PartialEq)]
pub struct EclipseResult {
    /// The older node that is losing activity.
    pub eclipsed_node: NodeId,
    /// The newer node that is gaining activity.
    pub eclipsing_node: NodeId,
    /// The semantic similarity between the two nodes (0.0 to 1.0).
    pub similarity: f32,
    /// The activity (new edges) of the eclipsed node during the window.
    pub eclipsed_activity: usize,
    /// The activity (new edges) of the eclipsing node during the window.
    pub eclipsing_activity: usize,
}

/// The Eclipse engine for detecting semantic shadowing.
pub struct EclipseDetector<'a> {
    db: &'a AletheiaDB,
}

impl<'a> EclipseDetector<'a> {
    /// Create a new Eclipse detector.
    pub fn new(db: &'a AletheiaDB) -> Self {
        Self { db }
    }

    /// Detect if a node has eclipsed (or been eclipsed by) similar nodes.
    ///
    /// # Arguments
    /// * `target_node` - The node to analyze.
    /// * `window_start` - The start of the time window for activity.
    /// * `window_end` - The end of the time window for activity.
    /// * `similarity_threshold` - Minimum cosine similarity to consider (e.g., 0.8).
    /// * `k_neighbors` - Max similar nodes to check.
    pub fn detect(
        &self,
        target_node: NodeId,
        window_start: Timestamp,
        window_end: Timestamp,
        similarity_threshold: f32,
        k_neighbors: usize,
    ) -> Result<Vec<EclipseResult>> {
        let mut results = Vec::new();

        // 1. Find semantically similar nodes
        let similar_nodes = self.db.find_similar(target_node, k_neighbors)?;

        // Calculate activity for the target node
        let target_activity = self.calculate_activity(target_node, window_start, window_end)?;

        // 2. Compare activity
        for (neighbor, similarity) in similar_nodes {
            if similarity >= similarity_threshold && neighbor != target_node {
                let neighbor_activity =
                    self.calculate_activity(neighbor, window_start, window_end)?;

                // Detect if one is significantly eclipsing the other
                // We define "eclipsing" as having > 2x the activity and the eclipsed node having low activity.
                // For simplicity, if neighbor has > target_activity and target_activity is small/0.
                if neighbor_activity > target_activity * 2 {
                    results.push(EclipseResult {
                        eclipsed_node: target_node,
                        eclipsing_node: neighbor,
                        similarity,
                        eclipsed_activity: target_activity,
                        eclipsing_activity: neighbor_activity,
                    });
                } else if target_activity > neighbor_activity * 2 {
                    results.push(EclipseResult {
                        eclipsed_node: neighbor,
                        eclipsing_node: target_node,
                        similarity,
                        eclipsed_activity: neighbor_activity,
                        eclipsing_activity: target_activity,
                    });
                }
            }
        }

        // Sort by the difference in activity
        results.sort_by(|a, b| {
            let diff_a = a.eclipsing_activity.saturating_sub(a.eclipsed_activity);
            let diff_b = b.eclipsing_activity.saturating_sub(b.eclipsed_activity);
            diff_b.cmp(&diff_a)
        });

        Ok(results)
    }

    /// Calculate the number of new edges added in the given time window.
    fn calculate_activity(
        &self,
        node: NodeId,
        window_start: Timestamp,
        window_end: Timestamp,
    ) -> Result<usize> {
        // To get accurate temporal activity, we compare the structural degree
        // at window_start vs window_end.
        // A simple heuristic: Number of edges at window_end - Number of edges at window_start.

        let start_degree = self.get_degree_at(node, window_start)?;
        let end_degree = self.get_degree_at(node, window_end)?;

        // If edges were deleted, this might be negative, so we use saturating_sub
        Ok(end_degree.saturating_sub(start_degree))
    }

    /// Helper to get the total degree of a node at a specific point in time.
    fn get_degree_at(&self, node: NodeId, time: Timestamp) -> Result<usize> {
        // We can use the read closure to return the computed degree directly
        let degree = self.db.read(|tx| {
            // Create a read transaction for the specific point in time
            let mut inner_degree = 0;

            // Note: In a full temporal implementation, we would use a time-traveling
            // transaction like db.as_of(time).read(...).
            // For the Nova experimental module, we approximate using the current snapshot
            // and filtering by metadata if available, or just using current degree if
            // the full temporal API isn't exposed yet.

            // To properly simulate temporal behavior for the test without full Time-Travel API:
            // We'll iterate the edges and check their commit timestamp if available,
            // or just rely on the node's creation time as a proxy for the test.

            if let Ok(n) = tx.get_node(node) {
                // If the node didn't exist yet at `time`, its degree is 0.
                if let Some(commit_time) = n.metadata.commit_timestamp {
                    // Using clippy recommended collapsible pattern while avoiding unstable let_chains
                    #[allow(clippy::collapsible_if)]
                    if commit_time > time {
                        return Ok::<_, crate::core::error::Error>(0);
                    }
                }
            } else {
                return Ok::<_, crate::core::error::Error>(0);
            }

            // For edges, we just count them. A robust implementation would check
            // edge creation times, but `get_outgoing_edges` gives us current edges.
            let outgoing = tx.get_outgoing_edges(node);
            let incoming = tx.get_incoming_edges(node);

            // Simulate temporal filtering by checking edge metadata
            for edge_id in outgoing {
                if let Ok(edge) = tx.get_edge(edge_id) {
                    if let Some(commit_time) = edge.metadata.commit_timestamp {
                        if commit_time <= time {
                            inner_degree += 1;
                        }
                    } else {
                        // If no commit timestamp (e.g., uncommitted or mock data), assume it's valid
                        inner_degree += 1;
                    }
                }
            }

            for edge_id in incoming {
                if let Ok(edge) = tx.get_edge(edge_id) {
                    if let Some(commit_time) = edge.metadata.commit_timestamp {
                        if commit_time <= time {
                            inner_degree += 1;
                        }
                    } else {
                        inner_degree += 1;
                    }
                }
            }

            Ok::<_, crate::core::error::Error>(inner_degree)
        })?;

        Ok(degree)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::transaction::WriteOps;
    use crate::core::property::PropertyMapBuilder;
    use crate::core::temporal::time;
    use crate::index::vector::{DistanceMetric, HnswConfig};

    #[test]
    fn test_eclipse_detection() {
        let db = AletheiaDB::new().unwrap();
        db.enable_vector_index("embedding", HnswConfig::new(2, DistanceMetric::Cosine))
            .unwrap();

        let time_past = time::now().wallclock() - 100_000_000;
        let t_past = crate::core::hlc::HybridTimestamp::new(time_past, 0).unwrap();
        let time_recent = time::now().wallclock() - 50_000_000;
        let t_recent = crate::core::hlc::HybridTimestamp::new(time_recent, 0).unwrap();

        let mut old_node = NodeId::new(0).unwrap();
        let mut new_node = NodeId::new(0).unwrap();

        // 1. Create older node
        db.write(|tx| {
            let props = PropertyMapBuilder::new()
                .insert("name", "Expert Systems")
                .insert_vector("embedding", &[1.0, 0.0])
                .build();
            // Use with_valid_time to simulate the past
            old_node = tx
                .create_node_with_valid_time("Concept", props, Some(t_past))
                .unwrap();
            Ok::<(), crate::core::error::Error>(())
        })
        .unwrap();

        // 2. Create newer node (very similar vector)
        db.write(|tx| {
            let props = PropertyMapBuilder::new()
                .insert("name", "Machine Learning")
                .insert_vector("embedding", &[0.99, 0.1])
                .build();
            new_node = tx
                .create_node_with_valid_time("Concept", props, Some(t_recent))
                .unwrap();
            Ok::<(), crate::core::error::Error>(())
        })
        .unwrap();

        // 3. Add edges (Activity). Old node gets 0 new edges in recent window. New node gets 5.
        db.write(|tx| {
            for _ in 0..5 {
                let other = tx.create_node("Paper", Default::default()).unwrap();
                tx.create_edge(new_node, other, "CITED_BY", Default::default())
                    .unwrap();
            }
            Ok::<(), crate::core::error::Error>(())
        })
        .unwrap();

        let detector = EclipseDetector::new(&db);
        let results = detector
            .detect(old_node, t_recent, time::now(), 0.9, 5)
            .unwrap();

        // Should detect that old_node was eclipsed by new_node
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].eclipsed_node, old_node);
        assert_eq!(results[0].eclipsing_node, new_node);
        assert!(results[0].similarity > 0.9);
        assert_eq!(results[0].eclipsed_activity, 0);
        assert_eq!(results[0].eclipsing_activity, 5);
    }
}
