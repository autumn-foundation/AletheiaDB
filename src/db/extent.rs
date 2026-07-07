//! Queryable bi-temporal extent of the dataset (Issue #3238).
//!
//! AletheiaDB lets a caller ask `AS OF <t>`, but nothing tells the caller what
//! time range the data actually covers: an `AS OF` before the earliest
//! recorded valid time returns an empty result that is indistinguishable from
//! "nothing existed then". [`AletheiaDB::temporal_extent`] closes that gap by
//! reporting the earliest and latest valid-time and transaction-time
//! coordinates observed across **all recorded history** — including
//! expired/superseded versions and delete tombstones — so a caller (notably
//! an LLM over MCP) can calibrate `AS OF` queries to land inside real data.
//!
//! # Semantics
//!
//! - **Coverage**: bounds are computed over every version ever recorded, not
//!   just the current state. A fact written for 2019 and later corrected
//!   still counts toward `earliest`. This is a calendar *range*, not a
//!   current-state count.
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
use std::collections::BTreeMap;

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

        let mut candidate = start;
        let end = range.end();
        if end != TIMESTAMP_MAX {
            candidate = candidate.max(end);
        }
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
fn into_label_extents(acc: BTreeMap<InternedString, BoundsAccumulator>) -> Vec<LabelExtent> {
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
    /// latest valid-time and transaction-time coordinates across **all
    /// recorded history**, including expired/superseded versions and delete
    /// tombstones.
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
    /// Bounds are read from the always-maintained temporal indexes, which
    /// retain an entry for every version ever recorded (entries are kept
    /// even after a version's payload migrates to cold storage), so this is
    /// an O(total versions) in-memory fold with no storage I/O.
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
    /// (computed from the temporal indexes, which cover every version ever
    /// recorded). The per-label breakdown is computed from the historical
    /// version store, which retains all recorded versions **still resident in
    /// the hot tier**; on databases with cold-storage migration enabled,
    /// versions whose payload has been migrated to the cold tier are not
    /// attributed to a label (the overall bounds still cover them).
    #[must_use = "this Result must be used; ignoring errors can lead to silent failures"]
    pub fn temporal_extent_by_label(&self) -> Result<TemporalExtent> {
        // Overall bounds first (temporal indexes; transient DashMap shard
        // guards only), then the historical read lock for the label scan —
        // the two phases never overlap, so no lock-order interaction with
        // the `historical` (3) → `temporal_indexes` (4) write-path ordering.
        let (valid_time, transaction_time) = self.overall_extent_bounds();

        let mut node_acc: BTreeMap<InternedString, BoundsAccumulator> = BTreeMap::new();
        let mut edge_acc: BTreeMap<InternedString, BoundsAccumulator> = BTreeMap::new();
        {
            let historical = self.historical.read();
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
        }

        Ok(TemporalExtent {
            valid_time,
            transaction_time,
            node_labels: Some(into_label_extents(node_acc)),
            edge_types: Some(into_label_extents(edge_acc)),
        })
    }

    /// Fold the temporal indexes into overall per-dimension bounds.
    ///
    /// The temporal indexes receive an entry for every committed write and
    /// retain entries even after cold-tier migration, making them the
    /// authoritative source for "all recorded history". An empty database
    /// yields all-`None` bounds.
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
        // must never be the open-interval sentinel or epoch.
        let tx_earliest = extent.transaction_time.earliest.expect("tx earliest");
        let tx_latest = extent.transaction_time.latest.expect("tx latest");
        assert!(tx_earliest >= before, "tx earliest within test window");
        assert!(tx_latest <= after, "tx latest within test window");
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

        let node_labels = extent.node_labels.expect("node labels");
        assert_eq!(node_labels.len(), 2);
        // Sorted by label: Company before Person.
        assert_eq!(node_labels[0].label, "Company");
        assert_eq!(node_labels[0].valid_time.earliest, Some(t2));
        assert_eq!(node_labels[0].valid_time.latest, Some(t2));
        assert_eq!(node_labels[1].label, "Person");
        assert_eq!(node_labels[1].valid_time.earliest, Some(t1));
        assert_eq!(node_labels[1].valid_time.latest, Some(t1));

        let edge_types = extent.edge_types.expect("edge types");
        assert_eq!(edge_types.len(), 1);
        assert_eq!(edge_types[0].label, "WORKS_AT");
        assert_eq!(edge_types[0].valid_time.earliest, Some(t3));
        assert_eq!(edge_types[0].valid_time.latest, Some(t3));
        // Per-label transaction bounds are real timestamps too.
        assert!(edge_types[0].transaction_time.earliest.is_some());
        assert!(edge_types[0].transaction_time.latest.is_some());
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

        let before = ts(earliest.wallclock() - 1_000_000);
        let now = time::now();

        let schema_before = db.schema_as_of(before, now).expect("schema before");
        assert!(
            schema_before.node_labels.is_empty(),
            "AS OF before valid_time.earliest must be empty"
        );

        let schema_inside = db.schema_as_of(earliest, now).expect("schema inside");
        assert_eq!(schema_inside.node_labels.len(), 1);
        assert_eq!(schema_inside.node_labels[0].label, "Person");
    }
}
