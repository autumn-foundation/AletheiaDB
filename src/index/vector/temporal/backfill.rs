//! Build the snapshots of a new temporal vector index from history (Issue #3888).
//!
//! The caller reads the vector changes of one property from historical
//! storage. [`TemporalVectorIndex::backfill`] replays them in commit order and
//! creates one snapshot at each commit that changed a vector. Only the newest
//! `max_snapshots` change points get a snapshot, so the work has a limit.

use std::collections::HashMap;
use std::sync::Arc;

use super::TemporalVectorIndex;
use crate::core::error::{Result, VectorError};
use crate::core::id::NodeId;
use crate::core::temporal::Timestamp;
use crate::index::vector::VectorIndex;

/// One change to the vector of one node, read from history.
#[derive(Debug, Clone, PartialEq)]
pub struct VectorChange {
    /// Commit (transaction) time of the change.
    pub at: Timestamp,
    /// The node that changed.
    pub node: NodeId,
    /// The vector after the change. `None` means the node has no vector.
    pub vector: Option<Arc<[f32]>>,
}

/// The result of a backfill.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BackfillReport {
    /// Number of changes replayed.
    pub changes: usize,
    /// Number of snapshots created.
    pub snapshots: usize,
    /// Number of vectors skipped (wrong dimension, NaN, or infinity).
    pub skipped: usize,
}

impl TemporalVectorIndex {
    /// Build the snapshots of this index from `changes`, then set the live
    /// state to `live`.
    ///
    /// - `changes` can be in any order. Changes with the same `at` are one
    ///   commit and give one snapshot at `at`. The snapshot holds the state
    ///   after that commit.
    /// - Only the newest `max_snapshots` commits get a snapshot. Older changes
    ///   are applied without one.
    /// - `live` is the current state (from current storage). It replaces the
    ///   replayed state as the live state. Differences go into the next
    ///   snapshot.
    /// - A vector with the wrong dimension, or with NaN or infinity, is
    ///   skipped: the live index can never hold it.
    ///
    /// # Errors
    ///
    /// Returns an error if the index is not new (it has vectors or snapshots),
    /// or if a snapshot build fails.
    pub fn backfill(
        &self,
        mut changes: Vec<VectorChange>,
        live: impl IntoIterator<Item = (NodeId, Arc<[f32]>)>,
    ) -> Result<BackfillReport> {
        self.ensure_new()?;
        let dimensions = self.dimensions();
        let usable = |v: &[f32]| v.len() == dimensions && v.iter().all(|x| x.is_finite());

        let mut report = BackfillReport::default();
        for change in &mut changes {
            if change.vector.as_deref().is_some_and(|v| !usable(v)) {
                change.vector = None;
                report.skipped += 1;
            }
        }
        changes.sort_by_key(|c| c.at);
        report.changes = changes.len();

        let commits = changes.chunk_by(|a, b| a.at == b.at).count();
        let first_snapshot = commits.saturating_sub(self.config.max_snapshots);

        for (i, commit) in changes.chunk_by(|a, b| a.at == b.at).enumerate() {
            {
                let mut state = self.current_state.write();
                for change in commit {
                    let old = match &change.vector {
                        Some(v) => state.vectors.insert(change.node, Arc::clone(v)),
                        None => state.vectors.remove(&change.node),
                    };
                    if old != change.vector {
                        state.metadata.record_change(change.node);
                    }
                }
            }
            if i >= first_snapshot {
                self.create_snapshot_internal(commit[0].at)?;
                report.snapshots += 1;
            }
        }

        self.set_live_state(live, &usable, &mut report)?;
        Ok(report)
    }

    /// Fail if this index has vectors or snapshots.
    fn ensure_new(&self) -> Result<()> {
        let has_snapshots = self.snapshot_data.read().len() > 0;
        let has_vectors = !self.current_state.read().vectors.is_empty() || self.current.len() > 0;
        if has_snapshots || has_vectors {
            return Err(VectorError::IndexError(
                "Backfill needs a new temporal vector index".to_string(),
            )
            .into());
        }
        Ok(())
    }

    /// Replace the replayed state with `live` and fill the live HNSW.
    fn set_live_state(
        &self,
        live: impl IntoIterator<Item = (NodeId, Arc<[f32]>)>,
        usable: &impl Fn(&[f32]) -> bool,
        report: &mut BackfillReport,
    ) -> Result<()> {
        let mut target: HashMap<NodeId, Arc<[f32]>> = HashMap::new();
        for (node, vector) in live {
            if usable(&vector) {
                target.insert(node, vector);
            } else {
                report.skipped += 1;
            }
        }

        for (node, vector) in &target {
            self.current.add(*node, vector)?;
        }

        let mut state = self.current_state.write();
        let removed: Vec<NodeId> = state
            .vectors
            .keys()
            .filter(|id| !target.contains_key(id))
            .copied()
            .collect();
        for node in removed {
            state.vectors.remove(&node);
            state.metadata.record_change(node);
        }
        for (node, vector) in target {
            if state.vectors.get(&node) != Some(&vector) {
                state.vectors.insert(node, vector);
                state.metadata.record_change(node);
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::vector::temporal::{RetentionPolicy, SnapshotStrategy, TemporalVectorConfig};
    use crate::index::vector::{DistanceMetric, HnswConfig};

    fn index(max_snapshots: usize) -> TemporalVectorIndex {
        TemporalVectorIndex::new(TemporalVectorConfig {
            snapshot_strategy: SnapshotStrategy::TransactionInterval(1000),
            retention_policy: RetentionPolicy::KeepAll,
            max_snapshots,
            full_snapshot_interval: 2,
            hnsw_config: Some(HnswConfig::new(2, DistanceMetric::Cosine)),
        })
        .unwrap()
    }

    fn ts(t: i64) -> Timestamp {
        Timestamp::from(t)
    }

    fn id(n: u64) -> NodeId {
        NodeId::new(n).unwrap()
    }

    fn change(t: i64, n: u64, v: Option<[f32; 2]>) -> VectorChange {
        VectorChange {
            at: ts(t),
            node: id(n),
            vector: v.map(|v| Arc::from(&v[..])),
        }
    }

    fn ids_at(index: &TemporalVectorIndex, t: i64) -> Vec<u64> {
        let mut ids: Vec<u64> = index
            .find_similar_as_of(&[1.0, 0.0], 100, ts(t))
            .unwrap()
            .into_iter()
            .map(|(n, _)| n.as_u64())
            .collect();
        ids.sort_unstable();
        ids
    }

    #[test]
    fn replays_changes_in_commit_order() {
        let index = index(100);
        // Unsorted input; the same `at` is one commit.
        let changes = vec![
            change(30, 1, None),
            change(10, 1, Some([1.0, 0.0])),
            change(20, 2, Some([1.0, 1.0])),
            change(20, 3, Some([1.0, 2.0])),
        ];
        let live = vec![(id(2), Arc::from(&[1.0f32, 1.0][..]))];
        let live = live
            .into_iter()
            .chain([(id(3), Arc::from(&[1.0f32, 2.0][..]))]);
        let report = index.backfill(changes, live).unwrap();

        assert_eq!(report.snapshots, 3);
        assert_eq!(report.changes, 4);
        assert!(
            ids_at(&index, 5).is_empty(),
            "no snapshot before the first commit"
        );
        assert_eq!(ids_at(&index, 10), vec![1]);
        assert_eq!(ids_at(&index, 25), vec![1, 2, 3]);
        assert_eq!(ids_at(&index, 30), vec![2, 3]);
        assert_eq!(index.current_index().len(), 2);
    }

    #[test]
    fn keeps_only_newest_max_snapshots() {
        let index = index(2);
        let changes = (1..=5)
            .map(|t| change(t * 10, t as u64, Some([1.0, 0.0])))
            .collect();
        let report = index.backfill(changes, std::iter::empty()).unwrap();
        assert_eq!(report.snapshots, 2);
        assert_eq!(index.snapshot_count(), 2);
        // The first kept snapshot still holds the older changes.
        assert_eq!(ids_at(&index, 40), vec![1, 2, 3, 4]);
        assert!(ids_at(&index, 30).is_empty());
    }

    #[test]
    fn skips_unusable_vectors() {
        let index = index(10);
        let changes = vec![
            VectorChange {
                at: ts(10),
                node: id(1),
                vector: Some(Arc::from(&[1.0f32, 0.0, 0.0][..])),
            },
            change(10, 2, Some([f32::NAN, 0.0])),
            change(10, 3, Some([1.0, 0.0])),
        ];
        let live = vec![(id(4), Arc::from(&[f32::INFINITY, 0.0][..]))];
        let report = index.backfill(changes, live).unwrap();
        assert_eq!(report.skipped, 3);
        assert_eq!(ids_at(&index, 10), vec![3]);
        assert_eq!(index.current_index().len(), 0);
    }

    #[test]
    fn live_state_differences_go_into_next_snapshot() {
        let index = index(10);
        let changes = vec![change(10, 1, Some([1.0, 0.0]))];
        // Current storage also holds node 2, which history did not show.
        let live = vec![
            (id(1), Arc::from(&[1.0f32, 0.0][..])),
            (id(2), Arc::from(&[1.0f32, 1.0][..])),
        ];
        index.backfill(changes, live).unwrap();
        assert_eq!(ids_at(&index, 10), vec![1]);
        index.on_transaction_at(ts(20)).unwrap();
        index.create_manual_snapshot().unwrap();
        assert_eq!(ids_at(&index, i64::MAX / 2), vec![1, 2]);
    }

    #[test]
    fn refuses_an_index_that_is_not_new() {
        let index = index(10);
        index.add(id(1), &[1.0, 0.0], ts(1)).unwrap();
        let err = index.backfill(Vec::new(), std::iter::empty()).unwrap_err();
        assert!(err.to_string().contains("new temporal vector index"));
    }

    #[test]
    fn empty_history_gives_no_snapshots() {
        let index = index(10);
        let report = index.backfill(Vec::new(), std::iter::empty()).unwrap();
        assert_eq!(report, BackfillReport::default());
        assert_eq!(index.snapshot_count(), 0);
    }
}
