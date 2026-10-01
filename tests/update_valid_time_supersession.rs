//! Append-only valid-time supersession on update.
//!
//! `update_node_with_valid_time` / `update_edge_with_valid_time` used to append
//! the new version with an open-ended valid interval and close only the
//! predecessor's TRANSACTION time. At the current transaction time the
//! predecessor was then invisible everywhere, so an as-of valid-time read
//! strictly between the two versions' `valid_from`s returned `NodeNotFound`.
//!
//! The fix keeps #3504's append-only rule (the predecessor's own valid interval
//! is never rewritten — earlier-transaction-time snapshots still see it
//! open-ended) and appends a structural *carry-forward* version recording the
//! predecessor over `[old_valid_from, new_valid_from)` at the update's commit.
//! These tests pin the acceptance criteria:
//!
//! 1. an as-of read between two versions resolves to the earlier version;
//! 2. history shows the earlier version's content over `[t1, t2)`;
//! 3. the same for edges;
//! 4. backfills between versions keep the slices non-overlapping and gap-free
//!    without corrupting the successor;
//! 5. equal-`valid_from` updates replace, update/retract interplay, deletes,
//!    the pull changefeed, and crash recovery all stay consistent.

use aletheiadb::config::WalConfigBuilder;
use aletheiadb::core::changefeed::{ChangeFeedQuery, ChangeType};
use aletheiadb::core::history::VersionInfo;
use aletheiadb::core::hlc::HybridTimestamp;
use aletheiadb::core::id::{EdgeId, NodeId};
use aletheiadb::core::property::PropertyValue;
use aletheiadb::core::temporal::{TIMESTAMP_MAX, Timestamp, time};
use aletheiadb::storage::index_persistence::PersistenceConfig;
use aletheiadb::storage::wal::DurabilityMode;
use aletheiadb::{AletheiaDB, AletheiaDBConfig, Error, PropertyMap, PropertyMapBuilder};
use std::collections::BTreeMap;
use tempfile::TempDir;

const HOUR: i64 = 3_600_000_000;

/// A timestamp `hours` before `now` (wallclock micros), optionally nudged.
fn ago(now: i64, hours: f64) -> Timestamp {
    HybridTimestamp::new(now - (hours * HOUR as f64) as i64, 0).unwrap()
}

fn city(value: &str) -> PropertyMap {
    PropertyMapBuilder::new().insert("city", value).build()
}

fn strength(value: i64) -> PropertyMap {
    PropertyMapBuilder::new().insert("strength", value).build()
}

fn node_city(db: &AletheiaDB, id: NodeId, valid: Timestamp) -> Option<String> {
    db.get_node_at_valid_time(id, valid).ok().and_then(|n| {
        n.properties
            .get("city")
            .and_then(|v| v.as_str().map(str::to_owned))
    })
}

fn node_city_at(db: &AletheiaDB, id: NodeId, valid: Timestamp, tx: Timestamp) -> Option<String> {
    db.get_node_at_time(id, valid, tx).ok().and_then(|n| {
        n.properties
            .get("city")
            .and_then(|v| v.as_str().map(str::to_owned))
    })
}

fn edge_strength(db: &AletheiaDB, id: EdgeId, valid: Timestamp) -> Option<i64> {
    db.get_edge_at_valid_time(id, valid)
        .ok()
        .and_then(|e| e.properties.get("strength").and_then(PropertyValue::as_int))
}

/// `(valid_start, valid_end, properties)` of an entity's current valid-time
/// slices (as returned by `get_*_valid_time_slices`, already sorted).
fn open_slices(slices: &[VersionInfo]) -> Vec<(Timestamp, Timestamp, PropertyMap)> {
    slices
        .iter()
        .map(|v| {
            assert!(v.temporal.transaction_time().is_current());
            (
                v.temporal.valid_time().start(),
                v.temporal.valid_time().end(),
                v.properties.clone(),
            )
        })
        .collect()
}

/// Assert the slices partition `[start, inf)`: non-overlapping, gap-free.
fn assert_partition(slices: &[VersionInfo], start: Timestamp) {
    let slices = open_slices(slices);
    assert!(
        !slices.is_empty(),
        "a live entity must have a current slice"
    );
    assert_eq!(
        slices[0].0, start,
        "slices must start at creation: {slices:?}"
    );
    for pair in slices.windows(2) {
        assert_eq!(
            pair[0].1, pair[1].0,
            "slices must be gap-free and non-overlapping: {slices:?}"
        );
    }
    assert_eq!(
        slices.last().unwrap().1,
        TIMESTAMP_MAX,
        "the last slice must be open-ended: {slices:?}"
    );
}

// --- Acceptance criteria 1 & 2: the issue's repro -------------------------

#[test]
fn as_of_read_between_versions_returns_predecessor() {
    let db = AletheiaDB::new().unwrap();
    let now = time::now().wallclock();
    let (t1, mid, t2) = (ago(now, 3.0), ago(now, 2.0), ago(now, 1.0));

    let id = db
        .create_node_with_valid_time("N", city("Paris"), Some(t1))
        .unwrap();
    db.update_node_with_valid_time(id, city("London"), Some(t2))
        .unwrap();

    // The repro: previously NodeNotFound.
    assert_eq!(node_city(&db, id, mid).as_deref(), Some("Paris"));
    assert_eq!(node_city(&db, id, t1).as_deref(), Some("Paris"));
    // Version 2 for t >= t2.
    assert_eq!(node_city(&db, id, t2).as_deref(), Some("London"));
    assert_eq!(node_city(&db, id, time::now()).as_deref(), Some("London"));
    // Nothing before creation.
    assert_eq!(node_city(&db, id, ago(now, 4.0)), None);
    // Current state is the latest version.
    assert_eq!(
        db.get_node(id).unwrap().properties.get("city"),
        Some(&PropertyValue::from("London"))
    );
}

#[test]
fn history_shows_predecessor_content_over_closed_interval() {
    let db = AletheiaDB::new().unwrap();
    let now = time::now().wallclock();
    let (t1, t2) = (ago(now, 3.0), ago(now, 1.0));

    let id = db
        .create_node_with_valid_time("N", city("Paris"), Some(t1))
        .unwrap();
    db.update_node_with_valid_time(id, city("London"), Some(t2))
        .unwrap();

    // The current valid-time view: version 1's content over [t1, t2),
    // version 2 over [t2, inf).
    let slice_infos = db.get_node_valid_time_slices(id).unwrap();
    let slices = open_slices(&slice_infos);
    assert_eq!(slices.len(), 2, "{slices:?}");
    assert_eq!((slices[0].0, slices[0].1), (t1, t2));
    assert_eq!(slices[0].2.get("city"), Some(&PropertyValue::from("Paris")));
    assert_eq!((slices[1].0, slices[1].1), (t2, TIMESTAMP_MAX));
    assert_eq!(
        slices[1].2.get("city"),
        Some(&PropertyValue::from("London"))
    );
    assert_partition(&slice_infos, t1);
    assert!(slice_infos[0].version_id.is_structural());
    assert!(!slice_infos[1].version_id.is_structural());

    // History still lists one entry per write (no structural noise), with
    // version numbers counting writes.
    let history = db.get_node_history(id).unwrap();
    assert_eq!(history.versions.len(), 2);
    assert!(
        history
            .versions
            .iter()
            .all(|v| !v.version_id.is_structural())
    );
    assert_eq!(
        db.get_node_at_version(id, 2)
            .unwrap()
            .properties
            .get("city"),
        Some(&PropertyValue::from("London"))
    );

    // #3504 holds: the ORIGINAL version-1 record keeps its open-ended valid
    // interval and is superseded on the transaction axis only.
    let original = &history.versions[0];
    assert!(original.temporal.valid_time().is_current());
    assert!(!original.temporal.transaction_time().is_current());
}

#[test]
fn earlier_transaction_time_still_sees_predecessor_open_ended() {
    let db = AletheiaDB::new().unwrap();
    let now = time::now().wallclock();
    let (t1, t2) = (ago(now, 3.0), ago(now, 1.0));

    let id = db
        .create_node_with_valid_time("N", city("Paris"), Some(t1))
        .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(2));
    let before_update = time::now();
    std::thread::sleep(std::time::Duration::from_millis(2));
    db.update_node_with_valid_time(id, city("London"), Some(t2))
        .unwrap();

    // As of the system time before the update, Paris held for ALL valid
    // times (append-only: no retroactive valid-time close, #3504).
    assert_eq!(
        node_city_at(&db, id, ago(now, 0.5), before_update).as_deref(),
        Some("Paris")
    );
    assert_eq!(
        node_city_at(&db, id, ago(now, 2.0), before_update).as_deref(),
        Some("Paris")
    );
    // As of now, the timeline is split.
    assert_eq!(
        node_city_at(&db, id, ago(now, 0.5), time::now()).as_deref(),
        Some("London")
    );
}

// --- Acceptance criterion 3: edges ----------------------------------------

#[test]
fn edge_update_carries_forward_predecessor() {
    let db = AletheiaDB::new().unwrap();
    let now = time::now().wallclock();
    let (t0, t1, mid, t2) = (ago(now, 4.0), ago(now, 3.0), ago(now, 2.0), ago(now, 1.0));

    let a = db
        .create_node_with_valid_time("P", PropertyMap::new(), Some(t0))
        .unwrap();
    let b = db
        .create_node_with_valid_time("P", PropertyMap::new(), Some(t0))
        .unwrap();
    let e = db
        .create_edge_with_valid_time(a, b, "KNOWS", strength(1), Some(t1))
        .unwrap();
    db.update_edge_with_valid_time(e, strength(9), Some(t2))
        .unwrap();

    assert_eq!(edge_strength(&db, e, mid), Some(1));
    assert_eq!(edge_strength(&db, e, t2), Some(9));
    assert_eq!(edge_strength(&db, e, time::now()), Some(9));
    assert_eq!(edge_strength(&db, e, t0), None);

    assert_eq!(db.get_edge_history(e).unwrap().versions.len(), 2);
    let history = db.get_edge_valid_time_slices(e).unwrap();
    let slices = open_slices(&history);
    assert_eq!(slices.len(), 2, "{slices:?}");
    assert_eq!((slices[0].0, slices[0].1), (t1, t2));
    assert_eq!(
        slices[0].2.get("strength"),
        Some(&PropertyValue::from(1i64))
    );
    assert_partition(&history, t1);

    // The edge stays traversable in current state with the latest properties.
    assert_eq!(db.get_outgoing_edges(a), vec![e]);
    assert_eq!(
        db.get_edge(e).unwrap().properties.get("strength"),
        Some(&PropertyValue::from(9i64))
    );
}

#[test]
fn edge_backfill_between_versions_keeps_successor() {
    let db = AletheiaDB::new().unwrap();
    let now = time::now().wallclock();
    let (t0, t1, t2) = (ago(now, 3.0), ago(now, 2.5), ago(now, 2.0));

    let a = db
        .create_node_with_valid_time("P", PropertyMap::new(), Some(t0))
        .unwrap();
    let b = db
        .create_node_with_valid_time("P", PropertyMap::new(), Some(t0))
        .unwrap();
    let e = db
        .create_edge_with_valid_time(a, b, "KNOWS", strength(1), Some(t0))
        .unwrap();
    db.update_edge_with_valid_time(e, strength(3), Some(t2))
        .unwrap();
    db.update_edge_with_valid_time(e, strength(2), Some(t1))
        .unwrap();

    assert_eq!(edge_strength(&db, e, ago(now, 2.75)), Some(1));
    assert_eq!(edge_strength(&db, e, ago(now, 2.25)), Some(2));
    assert_eq!(edge_strength(&db, e, ago(now, 1.0)), Some(3));
    assert_eq!(
        db.get_edge(e).unwrap().properties.get("strength"),
        Some(&PropertyValue::from(3i64)),
        "a backfill must not change the current state"
    );
    assert_partition(&db.get_edge_valid_time_slices(e).unwrap(), t0);
}

// --- Acceptance criterion 4/5: backfill, equal valid_from, retraction -----

#[test]
fn backfill_between_versions_is_bounded_by_successor() {
    let db = AletheiaDB::new().unwrap();
    let now = time::now().wallclock();
    let (t0, t1, t2) = (ago(now, 3.0), ago(now, 2.5), ago(now, 2.0));

    let id = db
        .create_node_with_valid_time("N", city("Paris"), Some(t0))
        .unwrap();
    db.update_node_with_valid_time(id, city("London"), Some(t2))
        .unwrap();
    // Backfill a correction between t0 and t2.
    db.update_node_with_valid_time(id, city("Berlin"), Some(t1))
        .unwrap();

    assert_eq!(node_city(&db, id, ago(now, 2.75)).as_deref(), Some("Paris"));
    assert_eq!(node_city(&db, id, t1).as_deref(), Some("Berlin"));
    assert_eq!(
        node_city(&db, id, ago(now, 2.25)).as_deref(),
        Some("Berlin")
    );
    assert_eq!(node_city(&db, id, t2).as_deref(), Some("London"));
    assert_eq!(node_city(&db, id, time::now()).as_deref(), Some("London"));

    // The successor's interval is untouched and current state is unchanged.
    assert_eq!(
        db.get_node(id).unwrap().properties.get("city"),
        Some(&PropertyValue::from("London"))
    );
    let history = db.get_node_valid_time_slices(id).unwrap();
    assert_partition(&history, t0);
    let slices = open_slices(&history);
    let bounds: Vec<_> = slices.iter().map(|s| (s.0, s.1)).collect();
    assert_eq!(bounds, vec![(t0, t1), (t1, t2), (t2, TIMESTAMP_MAX)]);

    // A later ordinary update still supersedes the open head.
    db.update_node_with_valid_time(id, city("Rome"), Some(ago(now, 1.0)))
        .unwrap();
    assert_eq!(node_city(&db, id, ago(now, 1.5)).as_deref(), Some("London"));
    assert_eq!(
        node_city(&db, id, ago(now, 2.25)).as_deref(),
        Some("Berlin")
    );
    assert_eq!(node_city(&db, id, ago(now, 0.5)).as_deref(), Some("Rome"));
    assert_partition(&db.get_node_valid_time_slices(id).unwrap(), t0);
}

#[test]
fn equal_valid_from_update_replaces_without_overlap() {
    let db = AletheiaDB::new().unwrap();
    let now = time::now().wallclock();
    let t1 = ago(now, 2.0);

    let id = db
        .create_node_with_valid_time("N", city("Paris"), Some(t1))
        .unwrap();
    db.update_node_with_valid_time(id, city("London"), Some(t1))
        .unwrap();

    assert_eq!(node_city(&db, id, t1).as_deref(), Some("London"));
    assert_eq!(node_city(&db, id, ago(now, 1.0)).as_deref(), Some("London"));

    // Degenerate replace: exactly one slice claims [t1, inf), no carry-forward.
    let slice_infos = db.get_node_valid_time_slices(id).unwrap();
    assert_eq!(slice_infos.len(), 1, "{slice_infos:?}");
    assert!(!slice_infos[0].version_id.is_structural());
    assert_partition(&slice_infos, t1);
    assert_eq!(db.get_node_history(id).unwrap().versions.len(), 2);
}

#[test]
fn retract_after_update_keeps_both_prefix_slices() {
    let db = AletheiaDB::new().unwrap();
    let now = time::now().wallclock();
    let (t1, t2, t_retract) = (ago(now, 3.0), ago(now, 2.0), ago(now, 1.0));

    let id = db
        .create_node_with_valid_time("N", city("Paris"), Some(t1))
        .unwrap();
    db.update_node_with_valid_time(id, city("London"), Some(t2))
        .unwrap();
    db.retract_node(id, t_retract).unwrap();

    assert_eq!(node_city(&db, id, ago(now, 2.5)).as_deref(), Some("Paris"));
    assert_eq!(node_city(&db, id, ago(now, 1.5)).as_deref(), Some("London"));
    assert_eq!(node_city(&db, id, t_retract), None);
    assert_eq!(node_city(&db, id, time::now()), None);
    assert!(
        db.get_node(id).is_err(),
        "retracted node leaves current state"
    );

    let bounds: Vec<_> = open_slices(&db.get_node_valid_time_slices(id).unwrap())
        .iter()
        .map(|s| (s.0, s.1))
        .collect();
    assert_eq!(bounds, vec![(t1, t2), (t2, t_retract)]);
}

#[test]
fn update_after_retract_is_rejected_and_history_is_unchanged() {
    let db = AletheiaDB::new().unwrap();
    let now = time::now().wallclock();
    let (t1, t2, t_retract) = (ago(now, 3.0), ago(now, 2.0), ago(now, 1.0));

    let id = db
        .create_node_with_valid_time("N", city("Paris"), Some(t1))
        .unwrap();
    db.update_node_with_valid_time(id, city("London"), Some(t2))
        .unwrap();
    db.retract_node(id, t_retract).unwrap();
    let before = db.get_node_history(id).unwrap().versions.len();

    let err = db
        .update_node_with_valid_time(id, city("Rome"), Some(ago(now, 0.5)))
        .unwrap_err();
    assert!(matches!(err, Error::Storage(_)), "got {err:?}");
    assert_eq!(db.get_node_history(id).unwrap().versions.len(), before);
    assert_eq!(node_city(&db, id, ago(now, 2.5)).as_deref(), Some("Paris"));
}

#[test]
fn delete_after_update_withdraws_every_slice() {
    let db = AletheiaDB::new().unwrap();
    let now = time::now().wallclock();
    let (t1, t2) = (ago(now, 3.0), ago(now, 2.0));

    let id = db
        .create_node_with_valid_time("N", city("Paris"), Some(t1))
        .unwrap();
    db.update_node_with_valid_time(id, city("London"), Some(t2))
        .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(2));
    let before_delete = time::now();
    std::thread::sleep(std::time::Duration::from_millis(2));
    db.delete_node_with_valid_time(id, None).unwrap();

    // Unchanged delete semantics: absent at every valid time as of now...
    for probe in [ago(now, 2.5), ago(now, 1.0), time::now()] {
        assert_eq!(node_city(&db, id, probe), None);
    }
    assert!(open_slices(&db.get_node_valid_time_slices(id).unwrap()).is_empty());
    // ...while system time before the delete still shows both slices.
    assert_eq!(
        node_city_at(&db, id, ago(now, 2.5), before_delete).as_deref(),
        Some("Paris")
    );
    assert_eq!(
        node_city_at(&db, id, ago(now, 1.0), before_delete).as_deref(),
        Some("London")
    );
}

#[test]
fn plain_updates_partition_valid_time() {
    let db = AletheiaDB::new().unwrap();
    let id = db.create_node("N", city("v0")).unwrap();
    let created = db.get_node_history(id).unwrap().versions[0]
        .temporal
        .valid_time()
        .start();
    for i in 1..=5 {
        std::thread::sleep(std::time::Duration::from_millis(1));
        db.update_node_with_valid_time(id, city(&format!("v{i}")), None)
            .unwrap();
    }
    let slice_infos = db.get_node_valid_time_slices(id).unwrap();
    assert_partition(&slice_infos, created);
    assert_eq!(db.get_node_history(id).unwrap().versions.len(), 6);
    let values: Vec<_> = open_slices(&slice_infos)
        .iter()
        .map(|s| s.2.get("city").and_then(|v| v.as_str().map(str::to_owned)))
        .collect();
    let expected: Vec<_> = (0..=5).map(|i| Some(format!("v{i}"))).collect();
    assert_eq!(values, expected);
}

/// Model-based check: random forward and backfill updates must match a
/// breakpoint map where an update at `t` owns `[t, next breakpoint)`.
#[test]
fn random_updates_match_breakpoint_model() {
    let db = AletheiaDB::new().unwrap();
    let now = time::now().wallclock();
    let base = now - 100 * HOUR;
    let at = |minute: i64| HybridTimestamp::new(base + minute * 60_000_000, 0).unwrap();

    let id = db
        .create_node_with_valid_time("N", city("c0"), Some(at(0)))
        .unwrap();
    let mut model: BTreeMap<i64, String> = BTreeMap::new();
    model.insert(0, "c0".to_owned());

    // Deterministic LCG so failures reproduce.
    let mut seed: u64 = 0x5eed_1234_abcd_0001;
    let mut rand = |bound: i64| {
        seed = seed
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        ((seed >> 33) as i64).rem_euclid(bound)
    };

    for step in 1..=60 {
        let minute = 1 + rand(400);
        let value = format!("c{step}");
        db.update_node_with_valid_time(id, city(&value), Some(at(minute)))
            .unwrap();
        model.insert(minute, value);

        for probe in [0, rand(420), rand(420), 419] {
            let expected = model.range(..=probe).next_back().map(|(_, v)| v.clone());
            assert_eq!(
                node_city(&db, id, at(probe)),
                expected,
                "step {step}: probe minute {probe}, model {model:?}"
            );
        }
        assert_partition(&db.get_node_valid_time_slices(id).unwrap(), at(0));
        let latest = model.values().next_back().cloned();
        assert_eq!(
            db.get_node(id)
                .unwrap()
                .properties
                .get("city")
                .and_then(|v| v.as_str().map(str::to_owned)),
            latest,
            "current state is the open-ended slice"
        );
    }
}

// --- Changefeed ------------------------------------------------------------

#[test]
fn pull_changefeed_skips_structural_versions() {
    let db = AletheiaDB::new().unwrap();
    let now = time::now().wallclock();
    let id = db
        .create_node_with_valid_time("N", city("Paris"), Some(ago(now, 3.0)))
        .unwrap();
    db.update_node_with_valid_time(id, city("London"), Some(ago(now, 1.0)))
        .unwrap();
    db.update_node_with_valid_time(id, city("Berlin"), Some(ago(now, 2.0)))
        .unwrap();

    let page = db
        .list_changes(&ChangeFeedQuery {
            tx_from: Timestamp::from(0),
            tx_to: TIMESTAMP_MAX,
            valid_from: None,
            valid_to: None,
            label: None,
            limit: 1000,
            cursor: None,
        })
        .unwrap();
    let types: Vec<_> = page.changes.iter().map(|c| c.change_type).collect();
    assert_eq!(
        types,
        vec![
            ChangeType::Created,
            ChangeType::Modified,
            ChangeType::Modified
        ],
        "one row per write, never a carry-forward/re-assertion: {:?}",
        page.changes
    );
}

// --- Crash recovery ----------------------------------------------------------

fn wal_only(dir: &std::path::Path) -> AletheiaDBConfig {
    AletheiaDBConfig::builder()
        .wal(
            WalConfigBuilder::new()
                .wal_dir(dir.join("wal"))
                .durability_mode(DurabilityMode::Synchronous)
                .build(),
        )
        .persistence(PersistenceConfig {
            enabled: false,
            ..PersistenceConfig::default()
        })
        .build()
}

type Snapshot = (
    Vec<Option<String>>,
    Vec<(Timestamp, Timestamp)>,
    Option<String>,
);

fn snapshot(db: &AletheiaDB, id: NodeId, probes: &[Timestamp]) -> Snapshot {
    let values = probes.iter().map(|p| node_city(db, id, *p)).collect();
    let bounds = open_slices(&db.get_node_valid_time_slices(id).unwrap())
        .iter()
        .map(|s| (s.0, s.1))
        .collect();
    let current = db.get_node(id).ok().and_then(|n| {
        n.properties
            .get("city")
            .and_then(|v| v.as_str().map(str::to_owned))
    });
    (values, bounds, current)
}

fn exercise(db: &AletheiaDB, now: i64) -> NodeId {
    let id = db
        .create_node_with_valid_time("N", city("Paris"), Some(ago(now, 3.0)))
        .unwrap();
    db.update_node_with_valid_time(id, city("London"), Some(ago(now, 2.0)))
        .unwrap();
    db.update_node_with_valid_time(id, city("Berlin"), Some(ago(now, 2.5)))
        .unwrap();
    db.update_node_with_valid_time(id, city("Rome"), None)
        .unwrap();
    id
}

#[test]
fn wal_replay_reproduces_valid_time_slices() {
    let tmp = TempDir::new().unwrap();
    let now = time::now().wallclock();
    let probes: Vec<_> = [3.5, 2.75, 2.25, 1.5]
        .iter()
        .map(|h| ago(now, *h))
        .collect();

    let (id, live) = {
        let db = AletheiaDB::with_unified_config(wal_only(tmp.path())).unwrap();
        let id = exercise(&db, now);
        (id, snapshot(&db, id, &probes))
    };
    assert_eq!(
        live.0,
        vec![
            None,
            Some("Paris".to_owned()),
            Some("Berlin".to_owned()),
            Some("London".to_owned())
        ]
    );

    let db = AletheiaDB::with_unified_config(wal_only(tmp.path())).unwrap();
    assert_eq!(snapshot(&db, id, &probes), live, "replay must match live");

    // Writes after recovery keep working (no structural id collisions).
    db.update_node_with_valid_time(id, city("Oslo"), None)
        .unwrap();
    assert_partition(&db.get_node_valid_time_slices(id).unwrap(), ago(now, 3.0));
}

#[test]
fn durable_reopen_reproduces_valid_time_slices() {
    let tmp = TempDir::new().unwrap();
    let now = time::now().wallclock();
    let probes: Vec<_> = [3.5, 2.75, 2.25, 1.5]
        .iter()
        .map(|h| ago(now, *h))
        .collect();

    let (id, live) = {
        let db = AletheiaDB::open(tmp.path()).unwrap();
        let id = exercise(&db, now);
        (id, snapshot(&db, id, &probes))
    };

    let db = AletheiaDB::open(tmp.path()).unwrap();
    assert_eq!(snapshot(&db, id, &probes), live, "reopen must match live");
    db.update_node_with_valid_time(id, city("Oslo"), None)
        .unwrap();
    assert_partition(&db.get_node_valid_time_slices(id).unwrap(), ago(now, 3.0));
    drop(db);

    // A second reopen after post-recovery writes.
    let db = AletheiaDB::open(tmp.path()).unwrap();
    assert_eq!(
        node_city(&db, id, ago(now, 2.25)).as_deref(),
        Some("Berlin")
    );
    assert_eq!(node_city(&db, id, time::now()).as_deref(), Some("Oslo"));
    assert_partition(&db.get_node_valid_time_slices(id).unwrap(), ago(now, 3.0));
}
