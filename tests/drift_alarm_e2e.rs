#![cfg(feature = "semantic-temporal")]
//! End-to-end integration tests for temporal semantic drift alarms (Issue #3367).
//!
//! These drive the real write path: enable a temporal vector index, declare
//! monitors via the gated `AletheiaDB` accessors, and assert validation, CRUD,
//! and background-driver (shed) behavior.
//!
//! # Where the firing fixtures live (Fix-1 correctness review)
//!
//! The literal firing rule compares the current embedding against the embedding
//! **on record `window` ago** (transaction-time as-of `now − window`; see
//! `evaluate_monitor`'s doc for why the past anchor is the transaction axis
//! under this engine's system-time supersession model). Deterministically
//! exercising that requires spreading the versions' *transaction* times, which
//! is only controllable via an injected `SimulatedClock` — and the simulated
//! clock is honored inside the library only under `cfg(test)` (the crate's own
//! unit tests), not from a separate integration-test binary. All firing,
//! persistence, resolve, `AS OF`-stability, changefeed-delivery, multi-entity
//! exact-set, and label-centroid fixtures therefore live as inline unit tests in
//! `src/experimental/temporal/drift_alarm.rs` (module `tests`), where they run
//! fully deterministically. This file keeps the integration-level coverage that
//! does not depend on time-separated commits.

use std::sync::Arc;
use std::time::Duration;

use aletheiadb::AletheiaDB;
use aletheiadb::PropertyMapBuilder;
use aletheiadb::core::error::{Error, QueryError};
use aletheiadb::core::id::NodeId;
use aletheiadb::experimental::temporal::drift_alarm::{
    DriftAlarmEngine, DriftMonitorSpec, DriftTarget, EvalMode,
};
use aletheiadb::index::vector::temporal::{
    DriftMetric, RetentionPolicy, SnapshotStrategy, TemporalVectorConfig,
};
use aletheiadb::index::vector::{DistanceMetric, HnswConfig};

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
    // Capacity-1 evaluation queue: the shock absorber holds a single pending
    // task. To prove shed-not-block *deterministically* (rather than racing the
    // evaluator against the commit rate), freeze evaluation: a stalled evaluator
    // is exactly the saturation scenario AC6 governs. With the worker parked, the
    // queue can hold at most one pending task and every further enqueue sheds --
    // while commits, which are entirely off the engine's path, keep succeeding.
    let engine = DriftAlarmEngine::with_capacity(Arc::clone(&db), 1);
    engine.start().expect("start engine");
    engine.set_evaluation_paused(true);

    // Flood updates faster than the (frozen) evaluator drains; commits must all
    // succeed and never block. 200 updates on one node stays under the per-entity
    // version cap. Each is its own committed transaction emitting a Doc change ->
    // one enqueued evaluation the dispatcher tries to hand to the parked worker.
    let node = create_doc(&db, "Doc", &[1.0, 0.0, 0.0, 0.0]);
    for i in 0..200u32 {
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

    // The dispatcher delivers changes asynchronously; wait (bounded) for it to
    // drain the subscription into the saturated queue and record shed work.
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while engine.shed_count() == 0 && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }

    // Some evaluation work was shed rather than back-pressuring writers.
    assert!(
        engine.shed_count() > 0,
        "a saturated evaluation queue sheds work (observable)"
    );
    // Resume + stop cleanly (no panics on shutdown, even from a paused worker).
    engine.set_evaluation_paused(false);
    engine.stop();
}
