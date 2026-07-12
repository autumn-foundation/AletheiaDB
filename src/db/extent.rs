//! Queryable bi-temporal extent of the dataset (Issue #3238).
//!
//! AletheiaDB lets a caller ask `AS OF <t>`, but nothing tells the caller what
//! time range the data actually covers: an `AS OF` before the earliest
//! recorded valid time returns an empty result that is indistinguishable from
//! "nothing existed then". [`AletheiaDB::temporal_extent`] closes that gap by
//! reporting the earliest and latest valid-time and transaction-time
//! coordinates observed across recorded history — including
//! expired/superseded versions and delete tombstones — so a caller (notably
//! an LLM over MCP) can calibrate `AS OF` queries to land inside real data.
//!
//! # Semantics
//!
//! - **Coverage**: bounds are computed over every version recorded during
//!   the current process lifetime, plus the hot-tier history restored at
//!   startup — not just the current state. A fact written for 2019 and
//!   later corrected still counts toward `earliest`. This is a calendar
//!   *range*, not a current-state count.
//! - **Cold storage (Issue #3389)**: on databases with cold-storage migration
//!   enabled, versions migrated to the cold tier are also reflected across
//!   restarts. The cold store persists min/max extent bounds per dimension in
//!   its metadata (maintained incrementally as versions migrate), and those
//!   bounds are merged back into the extent aggregate at startup — so a fact
//!   migrated to cold before a restart still counts toward `earliest`.
//! - **`earliest`**: the minimum interval start observed in that dimension.
//! - **`latest`**: the maximum *finite* coordinate observed in that
//!   dimension — the max over every interval start and every **closed**
//!   interval end. Open-ended intervals (still-valid facts / still-current
//!   records, `end == TIMESTAMP_MAX`) contribute only their start, so
//!   `latest` reports the newest recorded event coordinate and never the
//!   open-interval sentinel (+infinity).
//! - **Empty database**: all bounds are `None` — never `0`/epoch — so "no
//!   data" cannot be misread as "data starting at 1970".

use crate::core::error::Result;
use crate::core::interning::{GLOBAL_INTERNER, InternedString};
use crate::core::temporal::{TIMESTAMP_MAX, TimeRange, Timestamp};
use crate::db::AletheiaDB;
use std::collections::HashMap;

/// Earliest/latest bounds observed in one temporal dimension.
///
/// Both fields are `None` when no versions have ever been recorded (empty
/// database); otherwise both are `Some`. See the [module docs](self) for the
/// exact `earliest`/`latest` convention.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TimeBounds {
    /// Minimum interval start observed, or `None` if no data was recorded.
    pub earliest: Option<Timestamp>,
    /// Maximum finite coordinate observed (max of interval starts and closed
    /// interval ends; open intervals contribute only their start), or `None`
    /// if no data was recorded.
    pub latest: Option<Timestamp>,
}

/// Bi-temporal bounds for a single node label or edge type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LabelExtent {
    /// The node label or edge/relationship type.
    pub label: String,
    /// Valid-time bounds across all recorded versions with this label/type.
    pub valid_time: TimeBounds,
    /// Transaction-time bounds across all recorded versions with this
    /// label/type.
    pub transaction_time: TimeBounds,
}

/// The dataset's queryable bi-temporal extent.
///
/// Returned by [`AletheiaDB::temporal_extent`] (overall bounds only) and
/// [`AletheiaDB::temporal_extent_by_label`] (overall bounds plus a per-label
/// / per-edge-type breakdown). See the [module docs](self) for field
/// semantics.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TemporalExtent {
    /// Valid-time bounds across all recorded history.
    pub valid_time: TimeBounds,
    /// Transaction-time bounds across all recorded history.
    pub transaction_time: TimeBounds,
    /// Per-node-label bounds, sorted by label. `None` unless requested via
    /// [`AletheiaDB::temporal_extent_by_label`].
    pub node_labels: Option<Vec<LabelExtent>>,
    /// Per-edge-type bounds, sorted by type. `None` unless requested via
    /// [`AletheiaDB::temporal_extent_by_label`].
    pub edge_types: Option<Vec<LabelExtent>>,
}

/// Accumulates min/max bounds for one temporal dimension.
#[derive(Debug, Clone, Copy, Default)]
struct DimAccumulator {
    earliest: Option<Timestamp>,
    latest: Option<Timestamp>,
}

impl DimAccumulator {
    /// Fold one interval into the accumulator, applying the documented
    /// convention: `earliest` tracks the min start; `latest` tracks the max
    /// of starts and closed ends, never the `TIMESTAMP_MAX` open sentinel.
    fn observe(&mut self, range: TimeRange) {
        let start = range.start();
        self.earliest = Some(self.earliest.map_or(start, |e| e.min(start)));

        // TimeRange guarantees end >= start, so a closed end is itself the
        // latest candidate; no max(start, end) needed.
        let end = range.end();
        let candidate = if end == TIMESTAMP_MAX { start } else { end };
        self.latest = Some(self.latest.map_or(candidate, |l| l.max(candidate)));
    }

    fn into_bounds(self) -> TimeBounds {
        TimeBounds {
            earliest: self.earliest,
            latest: self.latest,
        }
    }
}

/// Accumulates both temporal dimensions for one label/type (or overall).
#[derive(Debug, Clone, Copy, Default)]
struct BoundsAccumulator {
    valid: DimAccumulator,
    tx: DimAccumulator,
}

impl BoundsAccumulator {
    fn observe(&mut self, temporal: &crate::core::temporal::BiTemporalInterval) {
        self.valid.observe(temporal.valid_time());
        self.tx.observe(temporal.transaction_time());
    }
}

/// Convert a per-label accumulator map into a sorted `Vec<LabelExtent>`.
fn into_label_extents(acc: HashMap<InternedString, BoundsAccumulator>) -> Vec<LabelExtent> {
    let mut extents: Vec<LabelExtent> = acc
        .into_iter()
        .map(|(label, bounds)| LabelExtent {
            label: GLOBAL_INTERNER.resolve_or_else(label, String::new),
            valid_time: bounds.valid.into_bounds(),
            transaction_time: bounds.tx.into_bounds(),
        })
        .collect();
    extents.sort_unstable_by(|a, b| a.label.cmp(&b.label));
    extents
}

impl AletheiaDB {
    /// Report the dataset's queryable bi-temporal extent: the earliest and
    /// latest valid-time and transaction-time coordinates across recorded
    /// history, including expired/superseded versions and delete tombstones.
    ///
    /// Use this to calibrate `AS OF` queries: an `AS OF` before
    /// `valid_time.earliest` (or after reconstructing a transaction-time
    /// coordinate outside `transaction_time`) is guaranteed to land outside
    /// recorded data, so its empty result means "out of recorded range", not
    /// "the fact never existed".
    ///
    /// # Conventions (see [module docs](self) for details)
    ///
    /// - `earliest` = min interval start per dimension.
    /// - `latest` = max of interval starts and *closed* interval ends;
    ///   open-ended (still-valid / still-current) intervals contribute only
    ///   their start, so the open-interval sentinel never leaks out.
    /// - An empty database yields `None` for every bound — never epoch 0.
    ///
    /// Bounds are read O(1) from a write-time maintained aggregate in the
    /// temporal indexes; no index traversal, no storage I/O. Bounds only
    /// ever widen while the process runs, so callers may cache the result
    /// for the duration of a session.
    ///
    /// # Coverage across restarts
    ///
    /// Bounds cover all history recorded during the current process lifetime
    /// plus the hot-tier history restored at startup. On databases with
    /// cold-storage migration enabled, history migrated to the cold tier is
    /// covered across restarts too (Issue #3389): the cold store persists its
    /// extent bounds and they are merged into the aggregate at startup, so a
    /// fact migrated to cold before a restart still bounds the extent. See the
    /// [module docs](self) for details.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # use aletheiadb::AletheiaDB;
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// let db = AletheiaDB::new()?;
    /// let extent = db.temporal_extent()?;
    /// if extent.valid_time.earliest.is_none() {
    ///     println!("no data recorded yet");
    /// }
    /// # Ok(())
    /// # }
    /// ```
    #[must_use = "this Result must be used; ignoring errors can lead to silent failures"]
    pub fn temporal_extent(&self) -> Result<TemporalExtent> {
        let (valid_time, transaction_time) = self.overall_extent_bounds();
        Ok(TemporalExtent {
            valid_time,
            transaction_time,
            node_labels: None,
            edge_types: None,
        })
    }

    /// Like [`temporal_extent`](Self::temporal_extent), but additionally
    /// breaks the bounds down per node label and per edge/relationship type,
    /// so a caller can scope `AS OF` calibration to exactly the labels it
    /// queries.
    ///
    /// The overall bounds are identical to [`temporal_extent`](Self::temporal_extent)
    /// and, as of Issue #3389, reflect cold-migrated history across restarts.
    /// The **per-label breakdown**, however, is computed from the historical
    /// version store, which retains only versions **still resident in the hot
    /// tier**: on databases with cold-storage migration enabled, versions
    /// whose payload has been migrated to the cold tier are not attributed to
    /// a label (the persisted cold bounds are aggregate-only, not per-label).
    /// So after a restart a label's per-label bound can be narrower than the
    /// overall bound if that label's older history lives only in cold. On a
    /// hot-only database every per-label bound lies within the overall bounds.
    #[must_use = "this Result must be used; ignoring errors can lead to silent failures"]
    pub fn temporal_extent_by_label(&self) -> Result<TemporalExtent> {
        // Take the historical read lock FIRST, then read the overall bounds
        // while holding it: commits mutate the temporal indexes while
        // holding `historical.write()`, so holding the read lock across
        // both phases yields one consistent snapshot (a write can no longer
        // slip between "read overall bounds" and "scan per-label versions"
        // and make a per-label bound exceed the overall bounds). Lock order
        // is respected: `historical` (3) is acquired before the temporal
        // indexes' internal extent aggregate (4), and `extent()` acquires
        // nothing ordered before `historical`.
        let historical = self.historical.read();

        let (valid_time, transaction_time) = self.overall_extent_bounds();

        let mut node_acc: HashMap<InternedString, BoundsAccumulator> = HashMap::new();
        let mut edge_acc: HashMap<InternedString, BoundsAccumulator> = HashMap::new();
        historical.visit_node_versions(|version| {
            node_acc
                .entry(version.label)
                .or_default()
                .observe(&version.temporal);
        });
        historical.visit_edge_versions(|version| {
            edge_acc
                .entry(version.label)
                .or_default()
                .observe(&version.temporal);
        });
        drop(historical);

        Ok(TemporalExtent {
            valid_time,
            transaction_time,
            node_labels: Some(into_label_extents(node_acc)),
            edge_types: Some(into_label_extents(edge_acc)),
        })
    }

    /// Read the temporal indexes' O(1) extent aggregate as overall
    /// per-dimension bounds.
    ///
    /// The temporal indexes observe every committed write (and interval
    /// close) at write time and never remove entries while the process
    /// runs, making them the authoritative in-process source for recorded
    /// history. An empty database yields all-`None` bounds.
    fn overall_extent_bounds(&self) -> (TimeBounds, TimeBounds) {
        match self.temporal_indexes.extent() {
            Some(extent) => (
                TimeBounds {
                    earliest: Some(extent.valid_earliest),
                    latest: Some(extent.valid_latest),
                },
                TimeBounds {
                    earliest: Some(extent.tx_earliest),
                    latest: Some(extent.tx_latest),
                },
            ),
            None => (TimeBounds::default(), TimeBounds::default()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::PropertyMapBuilder;
    use crate::core::temporal::{BiTemporalInterval, TIMESTAMP_MAX, TimeRange, time};

    fn ts(micros: i64) -> Timestamp {
        Timestamp::from(micros)
    }

    fn props(key: &str, value: &str) -> crate::core::PropertyMap {
        PropertyMapBuilder::new().insert(key, value).build()
    }

    #[test]
    fn empty_database_returns_all_none_bounds() {
        let db = AletheiaDB::new().expect("db init");
        let extent = db.temporal_extent().expect("extent");

        assert_eq!(extent.valid_time.earliest, None);
        assert_eq!(extent.valid_time.latest, None);
        assert_eq!(extent.transaction_time.earliest, None);
        assert_eq!(extent.transaction_time.latest, None);
        assert_eq!(extent.node_labels, None);
        assert_eq!(extent.edge_types, None);

        // The by-label variant on an empty DB: explicit empty breakdown,
        // still all-None overall bounds.
        let extent = db.temporal_extent_by_label().expect("extent");
        assert_eq!(extent.valid_time.earliest, None);
        assert_eq!(extent.transaction_time.latest, None);
        assert_eq!(extent.node_labels, Some(Vec::new()));
        assert_eq!(extent.edge_types, Some(Vec::new()));
    }

    #[test]
    fn populated_db_bounds_match_min_max_across_dimensions() {
        let db = AletheiaDB::new().expect("db init");
        let before = time::now();

        // Backdated node: valid time well before "now".
        let t1 = ts(1_000_000_000_000_000); // ~2001-09-09
        let t2 = ts(1_200_000_000_000_000); // ~2008-01-10
        db.create_node_with_valid_time("Person", props("name", "Alice"), Some(t1))
            .expect("create node");
        db.create_node_with_valid_time("Company", props("name", "Acme"), Some(t2))
            .expect("create node");

        let after = time::now();
        let extent = db.temporal_extent().expect("extent");

        // Valid time: earliest is the backdated t1; latest is the max valid
        // event (t2 -- both intervals are open-ended so only starts count).
        assert_eq!(extent.valid_time.earliest, Some(t1));
        assert_eq!(extent.valid_time.latest, Some(t2));

        // Transaction time: system-assigned, bracketed by the test run;
        // must never be the open-interval sentinel or epoch. The upper
        // bound compares wallclocks with a small slack: a commit HLC and
        // the `after` capture can share a microsecond, in which case the
        // commit's logical counter makes it compare greater than `after`.
        let tx_earliest = extent.transaction_time.earliest.expect("tx earliest");
        let tx_latest = extent.transaction_time.latest.expect("tx latest");
        assert!(tx_earliest >= before, "tx earliest within test window");
        assert!(
            tx_latest.wallclock() <= after.wallclock() + 10,
            "tx latest within test window (with HLC same-microsecond slack)"
        );
        assert!(tx_earliest <= tx_latest);
        assert!(tx_latest < TIMESTAMP_MAX, "sentinel must not leak");
    }

    #[test]
    fn superseded_version_still_counts_toward_earliest() {
        let db = AletheiaDB::new().expect("db init");

        let t1 = ts(1_000_000_000_000_000);
        let node_id = db
            .create_node_with_valid_time("Person", props("role", "engineer"), Some(t1))
            .expect("create node");

        // Supersede the original version: the first version's valid interval
        // is closed, but its backdated start must still bound `earliest`.
        db.update_node_with_valid_time(node_id, props("role", "manager"), None)
            .expect("update node");

        let extent = db.temporal_extent().expect("extent");
        assert_eq!(
            extent.valid_time.earliest,
            Some(t1),
            "expired/superseded version must still count toward earliest"
        );
        // The update happened at ~now, so latest moved past t1.
        assert!(extent.valid_time.latest.expect("latest") > t1);
    }

    #[test]
    fn open_valid_to_contributes_start_not_sentinel_to_latest() {
        let db = AletheiaDB::new().expect("db init");

        let t1 = ts(1_000_000_000_000_000);
        db.create_node_with_valid_time("Person", props("name", "Alice"), Some(t1))
            .expect("create node");

        let extent = db.temporal_extent().expect("extent");
        // Single open-ended fact: latest == its start, never TIMESTAMP_MAX.
        assert_eq!(extent.valid_time.latest, Some(t1));
        assert!(extent.valid_time.latest.expect("latest") < TIMESTAMP_MAX);
    }

    #[test]
    fn closed_valid_to_extends_latest_beyond_any_start() {
        // Public write paths always close an interval at another version's
        // start, so exercise the closed-end arm of the convention directly
        // through the temporal index (the authority temporal_extent reads).
        let db = AletheiaDB::new().expect("db init");

        let t1 = ts(1_000_000_000_000_000);
        let t_end = ts(1_500_000_000_000_000); // closed end later than any start
        let t_tx = ts(1_100_000_000_000_000);

        let interval = BiTemporalInterval::new(
            TimeRange::new(t1, t_end).expect("valid range"),
            TimeRange::from(t_tx),
        );
        db.__test_temporal_indexes()
            .insert_node_version(
                crate::core::NodeId::new(1).expect("node id"),
                crate::core::VersionId::new(1).expect("version id"),
                interval,
            )
            .expect("insert version");

        let extent = db.temporal_extent().expect("extent");
        assert_eq!(extent.valid_time.earliest, Some(t1));
        assert_eq!(
            extent.valid_time.latest,
            Some(t_end),
            "closed valid_to must extend latest beyond every start"
        );
        assert_eq!(extent.transaction_time.earliest, Some(t_tx));
        assert_eq!(extent.transaction_time.latest, Some(t_tx));
    }

    /// Assert every per-label bound lies within the overall bounds, and
    /// that the overall bounds equal the fold of the per-label breakdown.
    /// Both properties hold on hot-only databases (no cold-tier migration),
    /// which is what `AletheiaDB::new()` provides.
    fn assert_breakdown_consistent_with_overall(extent: &TemporalExtent) {
        let mut folded_valid = DimAccumulator::default();
        let mut folded_tx = DimAccumulator::default();

        let all_labels = extent
            .node_labels
            .as_deref()
            .expect("node labels")
            .iter()
            .chain(extent.edge_types.as_deref().expect("edge types"));

        for label_extent in all_labels {
            for (bounds, overall, folded) in [
                (
                    &label_extent.valid_time,
                    &extent.valid_time,
                    &mut folded_valid,
                ),
                (
                    &label_extent.transaction_time,
                    &extent.transaction_time,
                    &mut folded_tx,
                ),
            ] {
                let earliest = bounds.earliest.expect("label earliest");
                let latest = bounds.latest.expect("label latest");
                assert!(
                    earliest >= overall.earliest.expect("overall earliest"),
                    "label '{}' earliest must be within overall bounds",
                    label_extent.label
                );
                assert!(
                    latest <= overall.latest.expect("overall latest"),
                    "label '{}' latest must be within overall bounds",
                    label_extent.label
                );
                folded.earliest = Some(folded.earliest.map_or(earliest, |e| e.min(earliest)));
                folded.latest = Some(folded.latest.map_or(latest, |l| l.max(latest)));
            }
        }

        // On a hot-only DB the breakdown covers every version the overall
        // bounds cover, so folding it back must reproduce them exactly —
        // guarding against the write-time aggregate and the per-label fold
        // ever drifting apart in convention.
        assert_eq!(
            folded_valid.into_bounds(),
            extent.valid_time,
            "overall valid bounds must equal fold of the by_label breakdown"
        );
        assert_eq!(
            folded_tx.into_bounds(),
            extent.transaction_time,
            "overall tx bounds must equal fold of the by_label breakdown"
        );
    }

    #[test]
    fn by_label_breakdown_partitions_bounds_per_label_and_edge_type() {
        let db = AletheiaDB::new().expect("db init");

        let t1 = ts(1_000_000_000_000_000);
        let t2 = ts(1_200_000_000_000_000);
        let t3 = ts(1_300_000_000_000_000);

        let alice = db
            .create_node_with_valid_time("Person", props("name", "Alice"), Some(t1))
            .expect("create node");
        let acme = db
            .create_node_with_valid_time("Company", props("name", "Acme"), Some(t2))
            .expect("create node");
        db.create_edge_with_valid_time(alice, acme, "WORKS_AT", props("role", "eng"), Some(t3))
            .expect("create edge");

        let extent = db.temporal_extent_by_label().expect("extent");

        // Overall bounds cover all labels.
        assert_eq!(extent.valid_time.earliest, Some(t1));
        assert_eq!(extent.valid_time.latest, Some(t3));

        let node_labels = extent.node_labels.as_deref().expect("node labels");
        assert_eq!(node_labels.len(), 2);
        // Sorted by label: Company before Person.
        assert_eq!(node_labels[0].label, "Company");
        assert_eq!(node_labels[0].valid_time.earliest, Some(t2));
        assert_eq!(node_labels[0].valid_time.latest, Some(t2));
        assert_eq!(node_labels[1].label, "Person");
        assert_eq!(node_labels[1].valid_time.earliest, Some(t1));
        assert_eq!(node_labels[1].valid_time.latest, Some(t1));

        let edge_types = extent.edge_types.as_deref().expect("edge types");
        assert_eq!(edge_types.len(), 1);
        assert_eq!(edge_types[0].label, "WORKS_AT");
        assert_eq!(edge_types[0].valid_time.earliest, Some(t3));
        assert_eq!(edge_types[0].valid_time.latest, Some(t3));
        // Per-label transaction bounds are real timestamps too.
        assert!(edge_types[0].transaction_time.earliest.is_some());
        assert!(edge_types[0].transaction_time.latest.is_some());

        // Hot-only DB: every per-label bound lies within the overall
        // bounds, and folding the breakdown reproduces the overall bounds.
        assert_breakdown_consistent_with_overall(&extent);
    }

    #[test]
    fn by_label_multi_version_label_bounds_span_backdated_start_and_closed_end() {
        // Kills two DimAccumulator mutants that a single-version-per-label
        // dataset cannot distinguish:
        //   (b) earliest computed with max(start) instead of min(start)
        //       would report t2 instead of the backdated t1;
        //   and exercises the superseded (closed) first version's interval
        //   alongside the open second one.
        let db = AletheiaDB::new().expect("db init");

        let t1 = ts(1_000_000_000_000_000); // backdated original
        let t2 = ts(1_400_000_000_000_000); // later correction
        let node_id = db
            .create_node_with_valid_time("Person", props("role", "engineer"), Some(t1))
            .expect("create node");
        db.update_node_with_valid_time(node_id, props("role", "manager"), Some(t2))
            .expect("update node");

        let extent = db.temporal_extent_by_label().expect("extent");
        let node_labels = extent.node_labels.as_deref().expect("node labels");
        assert_eq!(node_labels.len(), 1);
        let person = &node_labels[0];
        assert_eq!(person.label, "Person");

        // The superseded version's backdated start still bounds earliest
        // (min of starts, not max).
        assert_eq!(
            person.valid_time.earliest,
            Some(t1),
            "per-label earliest must be the min start (backdated t1), not the newest"
        );
        // #3504: the superseded version's valid interval stays OPEN
        // (append-only), so `latest` reaches t2 via the UPDATE version's valid
        // start (the max of interval starts), not via a closed end.
        assert_eq!(
            person.valid_time.latest,
            Some(t2),
            "per-label latest must reach the update version's valid start"
        );

        assert_breakdown_consistent_with_overall(&extent);
    }

    #[test]
    fn by_label_closed_end_beyond_every_start_extends_label_latest() {
        // Kills the DimAccumulator mutant that drops the closed-end
        // contribution to `latest`: public write paths always close an
        // interval at another same-label version's start (so max(starts)
        // masks the mutant); force a closed transaction-time end strictly
        // greater than every start via the historical storage API.
        let db = AletheiaDB::new().expect("db init");

        let t1 = ts(1_000_000_000_000_000);
        let node_id = db
            .create_node_with_valid_time("Person", props("name", "Alice"), Some(t1))
            .expect("create node");

        let version_id = db
            .historical
            .read()
            .get_current_node_version(node_id)
            .expect("current version");
        // A finite close strictly after every recorded coordinate.
        let t_close = ts(time::now().wallclock() + 3_600_000_000); // now + 1h
        db.historical
            .write()
            .close_node_version_transaction_time(version_id, t_close)
            .expect("close tx time");

        let extent = db.temporal_extent_by_label().expect("extent");
        let node_labels = extent.node_labels.as_deref().expect("node labels");
        assert_eq!(node_labels.len(), 1);
        let person = &node_labels[0];

        // Without the closed-end contribution, latest would be the max tx
        // *start* (the creation commit), not t_close.
        assert_eq!(
            person.transaction_time.latest,
            Some(t_close),
            "per-label latest must include closed ends beyond every start"
        );
        // The close path also extends the overall (write-time) aggregate.
        assert_eq!(
            extent.transaction_time.latest,
            Some(t_close),
            "overall latest must include closed ends beyond every start"
        );

        assert_breakdown_consistent_with_overall(&extent);
    }

    #[test]
    fn deleted_backdated_node_still_bounds_extent() {
        // Delete tombstones count toward the extent: a node backdated to t1
        // and then deleted must still report valid_time.earliest == t1, and
        // both dimensions' latest must reflect the deletion event.
        let db = AletheiaDB::new().expect("db init");

        let t1 = ts(1_600_000_000_000_000); // ~2020-09-13, backdated creation
        let node_id = db
            .create_node_with_valid_time("Person", props("name", "Alice"), Some(t1))
            .expect("create node");

        let before_delete = time::now();
        db.delete_node_with_valid_time(node_id, None)
            .expect("delete node");

        let extent = db.temporal_extent().expect("extent");
        assert_eq!(
            extent.valid_time.earliest,
            Some(t1),
            "a deleted node's backdated start must still bound earliest"
        );
        // The deletion closed the version's valid interval at ~now and
        // recorded a tombstone; latest in both dimensions reaches it.
        assert!(
            extent.valid_time.latest.expect("valid latest") >= before_delete,
            "valid latest must reflect the deletion coordinate"
        );
        assert!(
            extent.transaction_time.latest.expect("tx latest") >= before_delete,
            "tx latest must reflect the deletion commit"
        );
        assert!(extent.valid_time.latest.expect("valid latest") < TIMESTAMP_MAX);
    }

    #[test]
    fn as_of_before_extent_is_empty_inside_extent_has_data() {
        // The answerability contract: an AS OF query strictly before
        // valid_time.earliest returns nothing, while the same query inside
        // the extent returns data.
        let db = AletheiaDB::new().expect("db init");

        let t1 = ts(1_000_000_000_000_000);
        db.create_node_with_valid_time("Person", props("name", "Alice"), Some(t1))
            .expect("create node");

        let extent = db.temporal_extent().expect("extent");
        let earliest = extent.valid_time.earliest.expect("earliest");

        // Use the extent's own transaction-time latest as the tx coordinate:
        // it is exactly the commit HLC, so — unlike a fresh `time::now()`
        // capture, which can land in the same microsecond as the commit and
        // compare *less* via the HLC logical counter — it is guaranteed to
        // see the committed version.
        let tx_at = extent.transaction_time.latest.expect("tx latest");
        let before = ts(earliest.wallclock() - 1_000_000);

        let schema_before = db.schema_as_of(before, tx_at).expect("schema before");
        assert!(
            schema_before.node_labels.is_empty(),
            "AS OF before valid_time.earliest must be empty"
        );

        let schema_inside = db.schema_as_of(earliest, tx_at).expect("schema inside");
        assert_eq!(schema_inside.node_labels.len(), 1);
        assert_eq!(schema_inside.node_labels[0].label, "Person");
    }

    /// Regression for Issue #3389: on a cold-storage-enabled database, history
    /// migrated to the cold tier before a restart must still be reflected in
    /// `temporal_extent` after the restart. Previously the temporal-index
    /// extent aggregate rebuilt from the hot tier only, so a backdated fact
    /// migrated to cold silently vanished from the reported extent — making an
    /// in-range `AS OF` misread as "never existed".
    #[test]
    fn cold_migrated_extent_survives_restart() {
        use crate::config::{AletheiaDBConfig, HistoricalConfigBuilder, WalConfigBuilder};
        use crate::core::id::{NodeId, VersionId};
        use crate::core::interning::GLOBAL_INTERNER;
        use crate::core::property::PropertyMap;
        use crate::core::version::NodeVersion;
        use crate::storage::redb_cold_storage::RedbColdStorage;

        let temp = tempfile::tempdir().expect("tempdir");
        let cold_path = temp.path().join("cold.redb");

        // A fact valid since ~2001, recorded ~2004, that lives ONLY in the
        // cold tier (as if migrated out of the hot maps before a restart).
        let old_valid = ts(1_000_000_000_000_000);
        let tx_at = ts(1_100_000_000_000_000);

        {
            let cold = RedbColdStorage::with_default_config(&cold_path).expect("cold open");
            let version = NodeVersion::new_anchor(
                VersionId::new(1).expect("vid"),
                NodeId::new(1).expect("nid"),
                BiTemporalInterval::new(TimeRange::from(old_valid), TimeRange::from(tx_at)),
                GLOBAL_INTERNER.intern("Person").expect("intern"),
                PropertyMap::new(),
            );
            cold.store_node_version(&version)
                .expect("store cold version");
            // `cold` is dropped here, releasing the redb file — simulating a
            // process exit with this history durable only in the cold tier.
        }

        // Reopen the database against the same cold file (simulated restart).
        let config = AletheiaDBConfig::builder()
            .wal(
                WalConfigBuilder::new()
                    .wal_dir(temp.path().join("wal"))
                    .build(),
            )
            .persistence(crate::storage::index_persistence::PersistenceConfig {
                enabled: false,
                ..Default::default()
            })
            .historical(
                HistoricalConfigBuilder::new()
                    .enable_cold_storage(true)
                    .cold_storage_path(&cold_path)
                    .build(),
            )
            .build();
        let db = AletheiaDB::with_unified_config(config).expect("db reopen");

        let extent = db.temporal_extent().expect("extent");
        assert_eq!(
            extent.valid_time.earliest,
            Some(old_valid),
            "cold-migrated backdated valid start must survive restart"
        );
        assert_eq!(
            extent.transaction_time.earliest,
            Some(tx_at),
            "cold-migrated transaction start must survive restart"
        );
        // The single migrated fact has an open valid_to; `latest` must be its
        // start, never the open-interval sentinel.
        assert_eq!(extent.valid_time.latest, Some(old_valid));
        assert!(
            extent.valid_time.latest.expect("latest") < TIMESTAMP_MAX,
            "open-interval sentinel must never leak into latest"
        );
    }

    /// Store a single anchor `Person` node version into a fresh cold `.redb`
    /// file at `cold_path` with the given valid/transaction ranges, then drop
    /// the store — simulating history durable only in the cold tier before a
    /// restart.
    fn seed_cold_node(cold_path: &std::path::Path, valid: TimeRange, tx: TimeRange) {
        use crate::core::id::{NodeId, VersionId};
        use crate::core::interning::GLOBAL_INTERNER;
        use crate::core::property::PropertyMap;
        use crate::core::version::NodeVersion;
        use crate::storage::redb_cold_storage::RedbColdStorage;

        let cold = RedbColdStorage::with_default_config(cold_path).expect("cold open");
        let version = NodeVersion::new_anchor(
            VersionId::new(1).expect("vid"),
            NodeId::new(1).expect("nid"),
            BiTemporalInterval::new(valid, tx),
            GLOBAL_INTERNER.intern("Person").expect("intern"),
            PropertyMap::new(),
        );
        cold.store_node_version(&version)
            .expect("store cold version");
        // `cold` drops here, releasing the redb file.
    }

    /// Open a database with cold storage enabled against `cold_path` (index
    /// persistence disabled; WAL under `temp_dir/wal`) — a simulated restart
    /// that triggers the Issue #3389 startup merge of persisted cold bounds.
    fn open_db_with_cold(temp_dir: &std::path::Path, cold_path: &std::path::Path) -> AletheiaDB {
        use crate::config::{AletheiaDBConfig, HistoricalConfigBuilder, WalConfigBuilder};

        let config = AletheiaDBConfig::builder()
            .wal(
                WalConfigBuilder::new()
                    .wal_dir(temp_dir.join("wal"))
                    .build(),
            )
            .persistence(crate::storage::index_persistence::PersistenceConfig {
                enabled: false,
                ..Default::default()
            })
            .historical(
                HistoricalConfigBuilder::new()
                    .enable_cold_storage(true)
                    .cold_storage_path(cold_path)
                    .build(),
            )
            .build();
        AletheiaDB::with_unified_config(config).expect("db open with cold")
    }

    /// Core widen-only invariant at the DB level (Issue #3389): a cold-migrated
    /// OLD fact and a newer hot fact must together bound the extent — earliest
    /// WIDENS back to cold, latest is NOT narrowed away from the hot fact. This
    /// is the merge's central contract, previously untested at DB level because
    /// every other DB test has an empty/absent hot tier; it simultaneously
    /// covers "migrated old version + newer hot version → extent spans both".
    #[test]
    fn cold_old_and_hot_recent_extent_spans_both_widen_only() {
        let temp = tempfile::tempdir().expect("tempdir");
        let cold_path = temp.path().join("cold.redb");

        // Cold tier: an OLD fact, earlier than anything the hot tier holds.
        let cold_valid = ts(1_000_000_000_000_000); // ~2001
        let cold_tx = ts(1_100_000_000_000_000); // ~2004
        seed_cold_node(
            &cold_path,
            TimeRange::from(cold_valid),
            TimeRange::from(cold_tx),
        );

        // Reopen against the same cold file, then write a RECENT fact through
        // the public API so it lands in the hot tier. Its valid start is later
        // than cold's (NARROWER earliest) and its coordinates are newer than
        // cold's (WIDER latest) in BOTH dimensions.
        let db = open_db_with_cold(temp.path(), &cold_path);
        let hot_valid = ts(1_800_000_000_000_000); // ~2027, well after cold
        let before_write = time::now();
        db.create_node_with_valid_time("Person", props("name", "Bob"), Some(hot_valid))
            .expect("create hot node");

        let extent = db.temporal_extent().expect("extent");

        // (i) earliest WIDENS back to the cold fact in both dimensions — the
        //     startup merge restored cold's older bounds the hot tier lacked.
        assert_eq!(
            extent.valid_time.earliest,
            Some(cold_valid),
            "valid earliest must widen to the cold fact's older start"
        );
        assert_eq!(
            extent.transaction_time.earliest,
            Some(cold_tx),
            "tx earliest must widen to the cold fact's older start"
        );

        // (ii) latest is NOT narrowed to cold's older coordinates — it retains
        //      the newer hot fact. This is the widen-only proof: the merge
        //      never pulls `latest` back to the smaller cold value.
        assert_eq!(
            extent.valid_time.latest,
            Some(hot_valid),
            "valid latest must retain the newer hot fact, not narrow to cold"
        );
        let tx_latest = extent.transaction_time.latest.expect("tx latest");
        assert!(
            tx_latest >= before_write,
            "tx latest must retain the newer hot commit, not narrow to cold"
        );
        assert!(
            tx_latest > cold_tx,
            "tx latest must exceed the older cold coordinate"
        );
        assert!(
            extent.valid_time.latest.expect("latest") < TIMESTAMP_MAX,
            "open-interval sentinel must never leak into latest"
        );
    }

    /// A cold-migrated version with a CLOSED transaction interval whose finite
    /// end is strictly later than its start must survive a restart in
    /// `transaction_time.latest` (Issue #3389). The existing restart test only
    /// covers the open-interval `latest == start` case; this proves a real
    /// closed end (`latest != start`) round-trips through the persisted bounds.
    #[test]
    fn cold_closed_tx_end_survives_restart_in_latest() {
        let temp = tempfile::tempdir().expect("tempdir");
        let cold_path = temp.path().join("cold.redb");

        let valid_start = ts(1_000_000_000_000_000);
        let tx_start = ts(1_100_000_000_000_000);
        let tx_end = ts(1_500_000_000_000_000); // finite close strictly after start
        seed_cold_node(
            &cold_path,
            TimeRange::from(valid_start),
            TimeRange::new(tx_start, tx_end).expect("closed tx range"),
        );

        let db = open_db_with_cold(temp.path(), &cold_path);
        let extent = db.temporal_extent().expect("extent");

        assert_eq!(
            extent.transaction_time.earliest,
            Some(tx_start),
            "cold tx earliest must survive restart"
        );
        assert_eq!(
            extent.transaction_time.latest,
            Some(tx_end),
            "a cold-migrated CLOSED tx end must survive restart in latest (not the start)"
        );
        assert_ne!(
            extent.transaction_time.latest,
            Some(tx_start),
            "closed end must not collapse to the interval start"
        );
        assert!(extent.transaction_time.latest.expect("latest") < TIMESTAMP_MAX);
    }

    /// Locks in the documented hot-tier-only per-label contract: after a
    /// cold-migrated restart the OVERALL bounds include a cold-only fact, but
    /// the per-label breakdown (`temporal_extent_by_label`) does NOT attribute
    /// it to any label (persisted cold bounds are aggregate-only, not
    /// per-label).
    #[test]
    fn cold_only_fact_bounds_overall_but_not_per_label_after_restart() {
        let temp = tempfile::tempdir().expect("tempdir");
        let cold_path = temp.path().join("cold.redb");

        let cold_valid = ts(1_000_000_000_000_000);
        let cold_tx = ts(1_100_000_000_000_000);
        seed_cold_node(
            &cold_path,
            TimeRange::from(cold_valid),
            TimeRange::from(cold_tx),
        );

        let db = open_db_with_cold(temp.path(), &cold_path);
        let extent = db.temporal_extent_by_label().expect("extent");

        // Overall bounds reflect the cold-only fact (merged at startup).
        assert_eq!(
            extent.valid_time.earliest,
            Some(cold_valid),
            "overall bounds must include the cold-only fact"
        );
        assert_eq!(extent.transaction_time.earliest, Some(cold_tx));

        // Per-label breakdown is hot-tier-only: the cold fact's "Person" label
        // is NOT attributed. With no hot versions the breakdown is empty.
        assert!(
            extent
                .node_labels
                .as_deref()
                .expect("node labels")
                .is_empty(),
            "cold-only history must not appear in the per-label breakdown"
        );
        assert!(extent.edge_types.as_deref().expect("edge types").is_empty());
    }

    /// A cold-ENABLED but empty database must yield the same extent as a
    /// hot-only database given identical writes: startup finds no persisted
    /// cold bounds (`get_temporal_extent_bounds -> None`), so the merge branch
    /// is a strict no-op. Distinct from the redb-unit `None`-when-empty test —
    /// this exercises the merge's `None`-bounds branch at the DB layer.
    #[test]
    fn cold_enabled_but_empty_yields_same_extent_as_hot_only() {
        let valid = ts(1_000_000_000_000_000);

        // Hot-only database (no cold tier configured).
        let hot_only = AletheiaDB::new().expect("db init");
        hot_only
            .create_node_with_valid_time("Person", props("name", "Alice"), Some(valid))
            .expect("create node");
        let hot_extent = hot_only.temporal_extent().expect("extent");

        // Cold-ENABLED but empty database, same write.
        let temp = tempfile::tempdir().expect("tempdir");
        let cold_path = temp.path().join("cold.redb");
        let cold_db = open_db_with_cold(temp.path(), &cold_path);
        cold_db
            .create_node_with_valid_time("Person", props("name", "Alice"), Some(valid))
            .expect("create node");
        let cold_extent = cold_db.temporal_extent().expect("extent");

        // Valid-time extent is deterministic from the backdated write; the
        // empty cold tier contributed nothing, so it must match hot-only
        // exactly. (Transaction time differs by commit wallclock across the two
        // databases, so only valid time is compared.)
        assert_eq!(
            cold_extent.valid_time, hot_extent.valid_time,
            "cold-enabled-but-empty valid extent must equal hot-only"
        );
        assert_eq!(cold_extent.valid_time.earliest, Some(valid));
        assert_eq!(hot_extent.valid_time.earliest, Some(valid));
    }
}
