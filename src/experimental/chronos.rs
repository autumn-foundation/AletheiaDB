//! Chronos: Temporal Graph Analysis & Pathfinding.
//!
//! This module implements tools for analyzing the graph's evolution over time
//! and navigating it at specific historical snapshots.
//!
//! # Features
//! - **Snapshot Pathfinding**: Find paths as they existed at a specific point in time
//!   using BFS on a consistent graph snapshot.
//! - **Volatility Analysis**: Measure how frequently a node changes (updates/sec).
//! - **Path Stability**: Calculate the percentage of time a path remains fully valid
//!   over a given window by intersecting the validity intervals of all edges.
//!
//! # Algorithm: Path Stability
//!
//! To calculate path stability over a window `W`:
//! 1. For each edge `e` in the path:
//!    - Retrieve its history.
//!    - Compute the set of valid time intervals `I_e` where the edge existed within `W`.
//! 2. Compute the intersection of all interval sets: `I_path = ⋂ I_e`.
//!    - This results in a set of disjoint intervals where *every* edge in the path was valid.
//! 3. Sum the duration of intervals in `I_path` and divide by the duration of `W`.
//!
//! # Example
//!
//! ```rust
//! // Requires features = ["nova"]
//! use aletheiadb::AletheiaDB;
//! use aletheiadb::experimental::chronos::Chronos;
//! use aletheiadb::core::temporal::TimeRange;
//! use aletheiadb::core::temporal::time;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let db = AletheiaDB::new()?;
//! // ... setup graph and history ...
//! # let node_id = db.create_node("Person", Default::default())?;
//!
//! let chronos = Chronos::new(&db);
//! let start = time::from_secs(0);
//! let end = time::now();
//! let window = TimeRange::new(start, end)?;
//!
//! // Calculate how volatile a node is (updates per second)
//! let volatility = chronos.node_volatility(node_id, window)?;
//! println!("Node volatility: {:.2} updates/sec", volatility);
//! # Ok(())
//! # }
//! ```

use crate::AletheiaDB;
use crate::core::error::Result;
use crate::core::id::{EdgeId, NodeId};
use crate::core::temporal::{TimeRange, Timestamp};
use std::collections::{HashSet, VecDeque};

/// The Time Lord of the Graph.
pub struct Chronos<'a> {
    db: &'a AletheiaDB,
}

impl<'a> Chronos<'a> {
    /// Create a new Chronos instance.
    pub fn new(db: &'a AletheiaDB) -> Self {
        Self { db }
    }

    /// Find a path from `start` to `end` that existed at `valid_time`.
    ///
    /// This performs a Breadth-First Search (BFS) on the graph snapshot at the given time.
    /// It traverses edges that were valid at `valid_time` and visible to `tx_time`.
    ///
    /// # Arguments
    /// * `start` - The starting node.
    /// * `end` - The destination node.
    /// * `valid_time` - The valid time coordinate (when the path existed in reality).
    /// * `tx_time` - The transaction time coordinate (what we knew at that time).
    ///
    /// # Returns
    /// * `Ok(Some(path))` - A vector of NodeIds representing the path.
    /// * `Ok(None)` - If no path exists.
    ///
    /// # Examples
    ///
    /// ```rust
    /// # use aletheiadb::AletheiaDB;
    /// # use aletheiadb::experimental::chronos::Chronos;
    /// # use aletheiadb::core::temporal::time;
    /// # use aletheiadb::core::property::PropertyMapBuilder;
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// let db = AletheiaDB::new()?;
    /// let t0 = time::now();
    ///
    /// // Create A -> B
    /// let a = db.create_node("Node", Default::default())?;
    /// let b = db.create_node("Node", Default::default())?;
    /// db.create_edge(a, b, "LINK", Default::default())?;
    ///
    /// // Find path at t0
    /// let chronos = Chronos::new(&db);
    /// let path = chronos.find_path_at_time(a, b, t0, t0)?;
    /// assert_eq!(path, Some(vec![a, b]));
    /// # Ok(())
    /// # }
    /// ```
    pub fn find_path_at_time(
        &self,
        start: NodeId,
        end: NodeId,
        valid_time: Timestamp,
        tx_time: Timestamp,
    ) -> Result<Option<Vec<NodeId>>> {
        if start == end {
            return Ok(Some(vec![start]));
        }

        let mut queue = VecDeque::new();
        queue.push_back(vec![start]);

        let mut visited = HashSet::new();
        visited.insert(start);

        while let Some(path) = queue.pop_front() {
            let current = *path.last().unwrap();

            if current == end {
                return Ok(Some(path));
            }

            // Get edges valid at this specific time
            let edge_ids = self
                .db
                .get_outgoing_edges_at_time(current, valid_time, tx_time);

            for edge_id in edge_ids {
                // We need to resolve the target node of this edge.
                // Since `get_outgoing_edges_at_time` returns EdgeIds, we need to fetch the edge
                // to see its target.
                // Optimization: In a real implementation, we might want `get_neighbors_at_time`.
                if let Ok(edge) = self.db.get_edge_at_time(edge_id, valid_time, tx_time) {
                    let neighbor = edge.target;
                    if !visited.contains(&neighbor) {
                        visited.insert(neighbor);
                        let mut new_path = path.clone();
                        new_path.push(neighbor);
                        queue.push_back(new_path);
                    }
                }
            }
        }

        Ok(None)
    }

    /// Calculate the volatility of a node over a time window.
    ///
    /// Volatility is defined as the number of versions valid within the window,
    /// divided by the window duration in seconds.
    ///
    /// # Returns
    /// * `f32` - Updates per second.
    ///
    /// # Examples
    ///
    /// ```rust
    /// # use aletheiadb::AletheiaDB;
    /// # use aletheiadb::experimental::chronos::Chronos;
    /// # use aletheiadb::core::temporal::{TimeRange, time};
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let db = AletheiaDB::new()?;
    /// # let node = db.create_node("Node", Default::default())?;
    /// let chronos = Chronos::new(&db);
    /// let window = TimeRange::new(time::from_secs(0), time::now())?;
    /// let volatility = chronos.node_volatility(node, window)?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn node_volatility(&self, node_id: NodeId, window: TimeRange) -> Result<f32> {
        let history = self.db.get_node_history(node_id)?;

        let mut count = 0;
        for version in &history.versions {
            // Check if the version's valid time overlaps with the window
            // Note: VersionInfo.temporal is a BiTemporalInterval.
            // We care about valid time.
            let valid_interval = version.temporal.valid_time();

            // Overlap check: version_start < window_end && version_end > window_start
            if valid_interval.start() < window.end() && valid_interval.end() > window.start() {
                count += 1;
            }
        }

        let duration_micros = window.end().wallclock() - window.start().wallclock();
        if duration_micros == 0 {
            return Ok(0.0);
        }

        let duration_secs = duration_micros as f32 / 1_000_000.0;
        Ok(count as f32 / duration_secs)
    }

    /// Calculate the stability of a path over a time window.
    ///
    /// Stability is the fraction of the time window where *all* edges in the path
    /// were simultaneously valid.
    ///
    /// # Returns
    /// * `f32` - A value between 0.0 (never valid) and 1.0 (always valid).
    ///
    /// # Examples
    ///
    /// ```rust
    /// # use aletheiadb::AletheiaDB;
    /// # use aletheiadb::experimental::chronos::Chronos;
    /// # use aletheiadb::core::temporal::{TimeRange, time};
    /// # use aletheiadb::core::id::EdgeId;
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// # let db = AletheiaDB::new()?;
    /// # let n1 = db.create_node("N", Default::default())?;
    /// # let n2 = db.create_node("N", Default::default())?;
    /// # let edge = db.create_edge(n1, n2, "E", Default::default())?;
    /// let chronos = Chronos::new(&db);
    /// let window = TimeRange::new(time::from_secs(0), time::now())?;
    /// let stability = chronos.path_stability(&[edge], window)?;
    /// println!("Path stability: {:.2}", stability);
    /// # Ok(())
    /// # }
    /// ```
    pub fn path_stability(&self, path: &[EdgeId], window: TimeRange) -> Result<f32> {
        if path.is_empty() {
            return Ok(1.0);
        }

        // We want to find the intersection of validity intervals for all edges,
        // clipped to the window.
        // Since an edge can have multiple valid intervals (multiple versions),
        // this is actually "find the union of valid intervals for edge E, then intersect across edges".
        //
        // However, `get_edge_history` returns a linear history.
        // Let's simplify: an edge is valid at T if ANY version covers T.
        // We need to calculate the total duration where ALL edges cover T.

        // Approach:
        // 1. For each edge, compute a set of disjoint intervals within `window` where it is valid.
        // 2. Intersect these interval sets across all edges.
        // 3. Sum duration of resulting intervals / window duration.

        let mut common_intervals = vec![window];

        for &edge_id in path {
            let history = self.db.get_edge_history(edge_id)?;
            let mut edge_intervals = Vec::new();

            for version in &history.versions {
                let valid = version.temporal.valid_time();
                // Clip to window
                let start = valid.start().max(window.start());
                let end = valid.end().min(window.end());

                if start < end {
                    edge_intervals.push(TimeRange::new(start, end)?);
                }
            }

            // Merge overlapping/adjacent intervals for this edge (though versions shouldn't overlap usually)
            // But let's be safe.
            edge_intervals = Self::merge_intervals(edge_intervals);

            // Intersect current common_intervals with edge_intervals
            common_intervals = Self::intersect_interval_sets(&common_intervals, &edge_intervals)?;

            if common_intervals.is_empty() {
                return Ok(0.0);
            }
        }

        let total_valid_duration: i64 = common_intervals
            .iter()
            .map(|r| r.end().wallclock() - r.start().wallclock())
            .sum();

        let window_duration = window.end().wallclock() - window.start().wallclock();
        if window_duration == 0 {
            return Ok(0.0);
        }

        Ok(total_valid_duration as f32 / window_duration as f32)
    }

    /// Helper: Merge overlapping or adjacent intervals into disjoint intervals.
    ///
    /// # Arguments
    /// * `intervals` - A vector of TimeRanges to merge.
    ///
    /// # Returns
    /// A sorted vector of disjoint TimeRanges covering the same total time.
    fn merge_intervals(mut intervals: Vec<TimeRange>) -> Vec<TimeRange> {
        if intervals.is_empty() {
            return intervals;
        }
        intervals.sort_by_key(|r| r.start());

        let mut merged = Vec::new();
        let mut current = intervals[0];

        for next in intervals.into_iter().skip(1) {
            if next.start() <= current.end() {
                // Overlap or adjacent, extend current
                if next.end() > current.end() {
                    // We need to reconstruct TimeRange because fields are private/immutable
                    // This unwrap is safe because start <= end is guaranteed if next.end > current.end >= current.start
                    current = TimeRange::new(current.start(), next.end()).unwrap();
                }
            } else {
                merged.push(current);
                current = next;
            }
        }
        merged.push(current);
        merged
    }

    /// Helper: Intersect two sets of disjoint sorted intervals.
    ///
    /// Finds the time periods present in BOTH sets.
    ///
    /// # Arguments
    /// * `set_a` - First sorted set of disjoint TimeRanges.
    /// * `set_b` - Second sorted set of disjoint TimeRanges.
    ///
    /// # Returns
    /// A sorted vector of disjoint TimeRanges representing the intersection.
    fn intersect_interval_sets(set_a: &[TimeRange], set_b: &[TimeRange]) -> Result<Vec<TimeRange>> {
        let mut result = Vec::new();
        let mut i = 0;
        let mut j = 0;

        while i < set_a.len() && j < set_b.len() {
            let a = set_a[i];
            let b = set_b[j];

            let start = a.start().max(b.start());
            let end = a.end().min(b.end());

            if start < end {
                result.push(TimeRange::new(start, end)?);
            }

            if a.end() < b.end() {
                i += 1;
            } else {
                j += 1;
            }
        }
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::transaction::WriteOps;
    use crate::core::property::PropertyMapBuilder;
    use crate::core::temporal::time;

    #[test]
    fn test_snapshot_pathfinding() {
        let db = AletheiaDB::new().unwrap();

        let _t0 = time::now();
        // Wait a bit to ensure t1 > t0
        std::thread::sleep(std::time::Duration::from_millis(10));
        let _t1 = time::now();

        let props = PropertyMapBuilder::new().build();

        // A -> B -> C created at T1
        // Create A, B, C
        let a = db.create_node("Node", props.clone()).unwrap();
        let b = db.create_node("Node", props.clone()).unwrap();
        let c = db.create_node("Node", props.clone()).unwrap();

        // Edges
        let e1 = db.create_edge(a, b, "NEXT", props.clone()).unwrap();
        let _e2 = db.create_edge(b, c, "NEXT", props.clone()).unwrap(); // Keep e2 alive

        std::thread::sleep(std::time::Duration::from_millis(10));
        let t2 = time::now();

        // Delete A->B at T2
        db.write(|tx| tx.delete_edge(e1)).unwrap();

        std::thread::sleep(std::time::Duration::from_millis(10));
        let t3 = time::now();

        let chronos = Chronos::new(&db);

        // Path should exist at T2 (between creation and deletion?)
        // Create edge happened "at" some time between t1 and t2 (when create_edge returned).
        // Let's use the exact returned time if we could, but here we bracket it.
        // Actually, create_edge uses `time::now()` internally for tx_time.
        // We need to query *after* creation but *before* deletion.

        // Path at T3 (after deletion): Should NOT exist
        let path_t3 = chronos.find_path_at_time(a, c, t3, t3).unwrap();
        assert!(path_t3.is_none(), "Path should be broken at T3");

        // Path at T2 (before deletion): Should exist.
        // We need a time between creation and deletion.
        // Since we slept, `t2` is *before* the deletion transaction started?
        // Wait, `t2` was captured *before* `db.write(...)`. So T2 is safe.
        // `create_edge` was called before T2.

        let path_t2 = chronos.find_path_at_time(a, c, t2, t2).unwrap();
        assert!(path_t2.is_some(), "Path should exist at T2");
        assert_eq!(path_t2.unwrap(), vec![a, b, c]);
    }

    #[test]
    fn test_node_volatility() {
        let db = AletheiaDB::new().unwrap();
        let props = PropertyMapBuilder::new().insert("val", 0).build();
        let node = db.create_node("Node", props.clone()).unwrap();

        let t_start = time::now();

        // Update 5 times
        for i in 1..=5 {
            std::thread::sleep(std::time::Duration::from_millis(10));
            db.write(|tx| tx.update_node(node, PropertyMapBuilder::new().insert("val", i).build()))
                .unwrap();
        }

        let t_end = time::now();

        let chronos = Chronos::new(&db);
        let window = TimeRange::new(t_start, t_end).unwrap();

        let vol = chronos.node_volatility(node, window).unwrap();

        // Duration ~50ms = 0.05s.
        // Updates: 1 (creation) + 5 (updates) = 6 versions.
        // Volatility = 6 / 0.05 = 120.

        assert!(vol > 0.0);
        // Loose check because time is non-deterministic
        assert!(vol > 10.0, "Volatility should be high (got {})", vol);
    }
}
