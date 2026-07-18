#![cfg(feature = "semantic-temporal")]
//! End-to-end integration tests for temporal semantic drift alarms (Issue #3367).
//!
//! These drive the real write path: enable a temporal vector index, write
//! embedding history, declare monitors via the gated `AletheiaDB` accessors,
//! and assert firing / persistence / `AS OF` stability / changefeed delivery /
//! background-driver behavior.
//!
//! STAGE A (RED): the `AletheiaDB::*_drift_*` accessors are `todo!()` skeletons,
//! so every test that reaches them panics. They assert the *intended* Stage B
//! contract and must FAIL until the green implementation lands.

use std::time::Duration;

use aletheiadb::AletheiaDB;
use aletheiadb::PropertyMapBuilder;
use aletheiadb::core::changefeed::ChangeType;
use aletheiadb::core::changefeed_subscription::ChangeFilter;
use aletheiadb::core::error::{Error, QueryError};
use aletheiadb::core::id::NodeId;
use aletheiadb::core::temporal::time;
use aletheiadb::experimental::temporal::drift_alarm::{
    DRIFT_ALARM_LABEL, DriftAlarmEngine, DriftAlarmFilter, DriftMonitorSpec, DriftTarget, EvalMode,
};
use aletheiadb::index::vector::temporal::{
    DriftMetric, RetentionPolicy, SnapshotStrategy, TemporalVectorConfig,
};
use aletheiadb::index::vector::{DistanceMetric, HnswConfig};
use std::sync::Arc;

const DIM: usize = 4;

fn normalize(v: &[f32]) -> Vec<f32> {
    let mag = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if mag == 0.0 {
        return v.to_vec();
    }
    v.iter().map(|x| x / mag).collect()
}

/// A DB with a Cosine temporal vector index on `"embedding"`.
fn db_with_index(metric: DistanceMetric) -> AletheiaDB {
    let db = AletheiaDB::new().expect("db");
    let config = TemporalVectorConfig {
        snapshot_strategy: SnapshotStrategy::TransactionInterval(1),
        retention_policy: RetentionPolicy::KeepAll,
        max_snapshots: 200,
        full_snapshot_interval: 10,
        hnsw_config: Some(HnswConfig::new(DIM, metric)),
    };
    db.enable_temporal_vector_index("embedding", config)
        .expect("enable temporal vector index");
    db
}

fn create_doc(db: &AletheiaDB, label: &str, embedding: &[f32]) -> NodeId {
    db.create_node(
        label,
        PropertyMapBuilder::new()
            .insert("title", "doc")
            .insert_vector("embedding", &normalize(embedding))
            .build(),
    )
    .expect("create node")
}

/// Overwrite a node's embedding (PATCH), creating a new bi-temporal version.
fn set_embedding(db: &AletheiaDB, node: NodeId, embedding: &[f32]) {
    db.update_node_with_valid_time(
        node,
        PropertyMapBuilder::new()
            .insert_vector("embedding", &normalize(embedding))
            .build(),
        None,
    )
    .expect("update node");
}

fn per_entity_monitor(metric: DriftMetric, threshold: f32) -> DriftMonitorSpec {
    DriftMonitorSpec {
        property_key: "embedding".to_string(),
        label: Some("Doc".to_string()),
        entities: None,
        metric,
        threshold,
        window: Duration::from_secs(3600),
        target: DriftTarget::PerEntity,
        mode: EvalMode::OnWrite,
    }
}

// -- Case 10: unknown property -> INVALID_ARGUMENT --------------------------

#[test]
fn create_monitor_unknown_property_is_invalid_argument() {
    let db = db_with_index(DistanceMetric::Cosine);
    let mut spec = per_entity_monitor(DriftMetric::Cosine, 0.25);
    spec.property_key = "does_not_exist".to_string();
    let err = db.create_drift_monitor(spec).expect_err("must reject");
    assert!(
        matches!(err, Error::Query(QueryError::InvalidParameter { .. })),
        "unknown property must map to INVALID_ARGUMENT, got {err:?}"
    );
}

// -- Case 11: non-positive threshold -> INVALID_ARGUMENT --------------------

#[test]
fn create_monitor_non_positive_threshold_is_invalid_argument() {
    let db = db_with_index(DistanceMetric::Cosine);
    let spec = per_entity_monitor(DriftMetric::Cosine, 0.0);
    let err = db.create_drift_monitor(spec).expect_err("must reject");
    assert!(
        matches!(err, Error::Query(QueryError::InvalidParameter { .. })),
        "non-positive threshold must map to INVALID_ARGUMENT, got {err:?}"
    );
}

// -- Case 12: zero/negative window -> INVALID_ARGUMENT ----------------------

#[test]
fn create_monitor_zero_window_is_invalid_argument() {
    let db = db_with_index(DistanceMetric::Cosine);
    let mut spec = per_entity_monitor(DriftMetric::Cosine, 0.25);
    spec.window = Duration::from_secs(0);
    let err = db.create_drift_monitor(spec).expect_err("must reject");
    assert!(
        matches!(err, Error::Query(QueryError::InvalidParameter { .. })),
        "zero window must map to INVALID_ARGUMENT, got {err:?}"
    );
}

// -- Case 13: metric mismatch vs index metric -> INVALID_ARGUMENT -----------

#[test]
fn create_monitor_metric_mismatch_is_invalid_argument() {
    // Index is Cosine; monitor asks for Euclidean.
    let db = db_with_index(DistanceMetric::Cosine);
    let spec = per_entity_monitor(DriftMetric::Euclidean, 0.25);
    let err = db.create_drift_monitor(spec).expect_err("must reject");
    assert!(
        matches!(err, Error::Query(QueryError::InvalidParameter { .. })),
        "metric mismatch must map to INVALID_ARGUMENT, got {err:?}"
    );
}

// -- Case 3 (e2e) + Case 14: fired alarm queryable with all fields ----------

#[test]
fn above_threshold_write_produces_queryable_alarm() {
    let db = db_with_index(DistanceMetric::Cosine);
    let node = create_doc(&db, "Doc", &[1.0, 0.0, 0.0, 0.0]);
    let monitor = db
        .create_drift_monitor(per_entity_monitor(DriftMetric::Cosine, 0.5))
        .expect("create monitor");

    // Drift the embedding to orthogonal (cosine distance 1.0 > 0.5).
    set_embedding(&db, node, &[0.0, 1.0, 0.0, 0.0]);

    let fired = db
        .evaluate_drift_monitor_now(monitor.id)
        .expect("evaluate now");
    assert_eq!(fired.len(), 1, "exactly one alarm expected");
    let alarm = &fired[0];
    assert_eq!(alarm.entity, Some(node));
    assert_eq!(alarm.monitor_id, monitor.id);
    assert!(!alarm.resolved);
    assert!(
        (alarm.threshold - 0.5).abs() < 1e-6,
        "threshold carried on alarm"
    );
    assert!(
        alarm.measured_distance > 0.5,
        "measured distance {} must exceed threshold",
        alarm.measured_distance
    );
    assert!(
        alarm.to_version.is_some() && alarm.from_version.is_some(),
        "both compared version refs present"
    );

    // Queryable via query_drift_alarms.
    let queried = db
        .query_drift_alarms(&DriftAlarmFilter::for_monitor(monitor.id))
        .expect("query alarms");
    assert_eq!(queried.len(), 1, "the fired alarm is durably queryable");
    assert_eq!(queried[0].alarm_id, alarm.alarm_id);
}

// -- Case 1/2 (e2e): sub-threshold + at-threshold jitter never fires --------

#[test]
fn sub_threshold_jitter_never_fires() {
    let db = db_with_index(DistanceMetric::Cosine);
    let node = create_doc(&db, "Doc", &[1.0, 0.0, 0.0, 0.0]);
    let monitor = db
        .create_drift_monitor(per_entity_monitor(DriftMetric::Cosine, 0.5))
        .expect("create monitor");
    // Tiny jitter — stays far below threshold.
    set_embedding(&db, node, &[1.0, 0.02, 0.0, 0.0]);
    let fired = db
        .evaluate_drift_monitor_now(monitor.id)
        .expect("evaluate now");
    assert!(fired.is_empty(), "sub-threshold jitter must not fire");
}

// -- Case 4 (e2e): re-crossing fires only after resolution ------------------

#[test]
fn re_crossing_fires_only_after_resolution() {
    let db = db_with_index(DistanceMetric::Cosine);
    let node = create_doc(&db, "Doc", &[1.0, 0.0, 0.0, 0.0]);
    let monitor = db
        .create_drift_monitor(per_entity_monitor(DriftMetric::Cosine, 0.5))
        .expect("create monitor");
    set_embedding(&db, node, &[0.0, 1.0, 0.0, 0.0]);
    let first = db.evaluate_drift_monitor_now(monitor.id).expect("eval");
    assert_eq!(first.len(), 1, "first crossing fires");

    // Re-evaluate while unresolved: no new alarm.
    let again = db.evaluate_drift_monitor_now(monitor.id).expect("eval");
    assert!(again.is_empty(), "no re-fire while unresolved");

    // Resolve, then a further crossing may fire again.
    db.resolve_drift_alarm(first[0].alarm_id).expect("resolve");
    set_embedding(&db, node, &[0.0, 0.0, 1.0, 0.0]);
    let third = db.evaluate_drift_monitor_now(monitor.id).expect("eval");
    assert_eq!(third.len(), 1, "re-arms only after resolution");
}

// -- Case 9 (e2e): label centroid fires when population shifts ---------------

#[test]
fn label_centroid_fires_on_population_shift() {
    let db = db_with_index(DistanceMetric::Cosine);
    let mut nodes = Vec::new();
    for _ in 0..4 {
        nodes.push(create_doc(&db, "Doc", &[1.0, 0.0, 0.0, 0.0]));
    }
    let spec = DriftMonitorSpec {
        target: DriftTarget::LabelCentroid,
        ..per_entity_monitor(DriftMetric::Cosine, 0.4)
    };
    let monitor = db.create_drift_monitor(spec).expect("create monitor");

    // Shift the whole population so the centroid moves past threshold even if
    // per-entity moves are individually modest.
    for &n in &nodes {
        set_embedding(&db, n, &[0.0, 1.0, 0.0, 0.0]);
    }
    let fired = db.evaluate_drift_monitor_now(monitor.id).expect("eval");
    assert_eq!(fired.len(), 1, "exactly one label-level alarm");
    assert_eq!(fired[0].label.as_deref(), Some("Doc"));
    assert!(
        fired[0].entity.is_none(),
        "label alarm has no single entity"
    );
}

// -- Case 15: resolve is recorded + AS OF stable ----------------------------

#[test]
fn resolve_is_recorded_and_as_of_stable() {
    let db = db_with_index(DistanceMetric::Cosine);
    let node = create_doc(&db, "Doc", &[1.0, 0.0, 0.0, 0.0]);
    let monitor = db
        .create_drift_monitor(per_entity_monitor(DriftMetric::Cosine, 0.5))
        .expect("create monitor");
    set_embedding(&db, node, &[0.0, 1.0, 0.0, 0.0]);
    let fired = db.evaluate_drift_monitor_now(monitor.id).expect("eval");
    let alarm_id = fired[0].alarm_id;

    let before_resolution = time::now();
    let resolved = db.resolve_drift_alarm(alarm_id).expect("resolve");
    assert!(resolved.resolved, "resolve marks resolved");

    // Unresolved filter excludes it.
    let unresolved = db
        .query_drift_alarms(&DriftAlarmFilter {
            resolved: Some(false),
            ..DriftAlarmFilter::for_monitor(monitor.id)
        })
        .expect("query");
    assert!(
        unresolved.iter().all(|a| a.alarm_id != alarm_id),
        "resolved alarm excluded from unresolved filter"
    );

    // AS OF before the resolution still shows it unresolved (append-only).
    let historical = db
        .query_drift_alarms(&DriftAlarmFilter {
            resolved: None,
            time_range: Some((before_resolution, before_resolution)),
            ..DriftAlarmFilter::for_monitor(monitor.id)
        })
        .expect("query as of");
    // The alarm existed and was unresolved at that coordinate.
    assert!(
        historical
            .iter()
            .any(|a| a.alarm_id == alarm_id && !a.resolved),
        "AS OF before resolution shows the alarm unresolved"
    );
}

// -- Case 16: alarm not retroactively deleted by later writes ---------------

#[test]
fn alarm_not_retroactively_deleted_by_later_writes() {
    let db = db_with_index(DistanceMetric::Cosine);
    let node = create_doc(&db, "Doc", &[1.0, 0.0, 0.0, 0.0]);
    let monitor = db
        .create_drift_monitor(per_entity_monitor(DriftMetric::Cosine, 0.5))
        .expect("create monitor");
    set_embedding(&db, node, &[0.0, 1.0, 0.0, 0.0]);
    let fired = db.evaluate_drift_monitor_now(monitor.id).expect("eval");
    let alarm_id = fired[0].alarm_id;

    // Further, unrelated writes to the entity.
    db.update_node_with_valid_time(
        node,
        PropertyMapBuilder::new()
            .insert("title", "renamed")
            .insert_vector("embedding", &normalize(&[0.0, 1.0, 0.0, 0.0]))
            .build(),
        None,
    )
    .expect("update");

    let still = db
        .query_drift_alarms(&DriftAlarmFilter::for_monitor(monitor.id))
        .expect("query");
    assert!(
        still.iter().any(|a| a.alarm_id == alarm_id),
        "alarm survives later entity writes"
    );
}

// -- Case 17: changefeed subscriber receives the alarm-node Created event ----

#[test]
fn changefeed_delivers_alarm_created_event() {
    let db = db_with_index(DistanceMetric::Cosine);
    let node = create_doc(&db, "Doc", &[1.0, 0.0, 0.0, 0.0]);
    let monitor = db
        .create_drift_monitor(per_entity_monitor(DriftMetric::Cosine, 0.5))
        .expect("create monitor");

    let sub = db
        .subscribe_changes(ChangeFilter::all().with_node_labels([DRIFT_ALARM_LABEL]))
        .expect("subscribe");

    set_embedding(&db, node, &[0.0, 1.0, 0.0, 0.0]);
    db.evaluate_drift_monitor_now(monitor.id).expect("eval");

    let records = sub.recv_timeout(Duration::from_secs(2)).unwrap_or_default();
    assert!(
        records
            .iter()
            .any(|r| r.label == DRIFT_ALARM_LABEL && r.change_type == ChangeType::Created),
        "changefeed delivers a Created event for the materialized alarm node"
    );
}

// -- Case 18: monitor create/list/delete round-trip -------------------------

#[test]
fn monitor_crud_round_trip() {
    let db = db_with_index(DistanceMetric::Cosine);
    let monitor = db
        .create_drift_monitor(per_entity_monitor(DriftMetric::Cosine, 0.5))
        .expect("create");
    let listed = db.list_drift_monitors();
    assert!(
        listed.iter().any(|m| m.id == monitor.id),
        "created monitor is listed"
    );
    assert_eq!(
        db.get_drift_monitor(monitor.id).expect("get").id,
        monitor.id
    );

    db.delete_drift_monitor(monitor.id).expect("delete");
    assert!(
        db.list_drift_monitors().iter().all(|m| m.id != monitor.id),
        "deleted monitor is gone"
    );
    // Deleted monitor no longer evaluates.
    let err = db.get_drift_monitor(monitor.id).expect_err("gone");
    assert!(
        matches!(err, Error::Query(QueryError::InvalidParameter { .. }))
            || matches!(err, Error::Storage(_))
    );
}

// -- Case 19: bounded queue sheds on saturation; commits never block --------

#[test]
fn saturated_queue_sheds_without_blocking_commits() {
    let db = Arc::new(db_with_index(DistanceMetric::Cosine));
    let _monitor = db
        .create_drift_monitor(per_entity_monitor(DriftMetric::Cosine, 0.5))
        .expect("create");
    let engine = DriftAlarmEngine::new(Arc::clone(&db));
    engine.start().expect("start engine");

    // Flood writes faster than evaluation drains; commits must all succeed.
    let node = create_doc(&db, "Doc", &[1.0, 0.0, 0.0, 0.0]);
    for i in 0..5_000u32 {
        let x = (i % 7) as f32;
        db.update_node_with_valid_time(
            node,
            PropertyMapBuilder::new()
                .insert_vector("embedding", &normalize(&[1.0, x, 0.0, 0.0]))
                .build(),
            None,
        )
        .expect("commit must never block or fail");
    }

    // Some evaluation work was shed rather than back-pressuring writers.
    assert!(
        engine.shed_count() > 0,
        "a saturated evaluation queue sheds work (observable)"
    );
    engine.stop();
}
