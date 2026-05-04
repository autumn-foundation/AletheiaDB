#![cfg(feature = "observability")]

use std::sync::Arc;

use aletheiadb::observability::{
    ATTR_DB_OPERATION_NAME, ATTR_DB_SYSTEM_NAME, ATTR_DURABILITY_MODE, ATTR_ERROR_CATEGORY,
    ATTR_QUERY_KIND, ATTR_STATUS, DB_SYSTEM_NAME, ErrorCategory, METRIC_ERRORS,
    METRIC_LABEL_CATEGORY, METRIC_LABEL_DURABILITY_MODE, METRIC_LABEL_STATUS,
    METRIC_TRANSACTION_COMMITS, MetricsRecorder, NoOpMetrics, SPAN_QUERY_EXECUTE,
    SPAN_TRANSACTION_COMMIT, SPAN_VECTOR_SEARCH, TelemetryConfig, transaction_commit_span,
};
use aletheiadb::{
    AletheiaDB, AletheiaDBConfig, DurabilityMode, HistoricalConfigBuilder, PropertyMapBuilder,
    WalConfigBuilder, WriteOps,
};

#[test]
fn otel_span_contract_names_are_stable() {
    assert_eq!(DB_SYSTEM_NAME, "aletheiadb");
    assert_eq!(SPAN_QUERY_EXECUTE, "aletheiadb.query.execute");
    assert_eq!(SPAN_VECTOR_SEARCH, "aletheiadb.vector.search");
    assert_eq!(SPAN_TRANSACTION_COMMIT, "aletheiadb.transaction.commit");

    assert_eq!(ATTR_DB_SYSTEM_NAME, "db.system.name");
    assert_eq!(ATTR_DB_OPERATION_NAME, "db.operation.name");
    assert_eq!(ATTR_QUERY_KIND, "aletheiadb.query.kind");
    assert_eq!(ATTR_DURABILITY_MODE, "aletheiadb.durability.mode");
    assert_eq!(ATTR_ERROR_CATEGORY, "aletheiadb.error.category");
    assert_eq!(ATTR_STATUS, "aletheiadb.status");
}

#[test]
fn otel_contract_helpers_create_tracing_spans() {
    let span = transaction_commit_span("tx-42", "GroupCommit");
    let _entered = span.enter();
}

#[test]
fn metrics_contract_names_and_labels_are_stable() {
    assert_eq!(METRIC_ERRORS, "aletheiadb.errors");
    assert_eq!(METRIC_TRANSACTION_COMMITS, "aletheiadb.transaction.commits");

    assert_eq!(METRIC_LABEL_CATEGORY, "category");
    assert_eq!(METRIC_LABEL_STATUS, "status");
    assert_eq!(METRIC_LABEL_DURABILITY_MODE, "durability_mode");
}

#[test]
fn metrics_recorder_contract_is_object_safe_and_noop_by_default() {
    let recorder: Arc<dyn MetricsRecorder> = Arc::new(NoOpMetrics);
    recorder.record_error(ErrorCategory::Storage);
    recorder.record_transaction_commit(0.001, 2, "GroupCommit", "committed");

    let telemetry = TelemetryConfig::builder()
        .service_name("release-candidate")
        .metrics(recorder)
        .build();

    assert_eq!(&*telemetry.service_name, "release-candidate");
    telemetry.metrics.record_error(ErrorCategory::Query);
}

#[test]
fn transaction_commit_metric_is_emitted_only_after_successful_commit() {
    let temp_dir = tempfile::tempdir().expect("create temp wal dir");
    let config = AletheiaDBConfig::builder()
        .wal(
            WalConfigBuilder::new()
                .wal_dir(temp_dir.path().join("wal"))
                .durability_mode(DurabilityMode::Synchronous)
                .build(),
        )
        .historical(
            HistoricalConfigBuilder::new()
                .max_versions_per_entity(1)
                .expect("valid historical retention")
                .build(),
        )
        .build();
    let db = AletheiaDB::with_unified_config(config).expect("create database");

    let starting_commits = aletheiadb::observability::metrics().transaction_commits_total;

    let mut create_tx = db.write_transaction().expect("begin create transaction");
    let node_id = create_tx
        .create_node(
            "Person",
            PropertyMapBuilder::new().insert("name", "Alice").build(),
        )
        .expect("buffer node creation");
    create_tx.commit().expect("initial commit succeeds");

    let after_successful_commit = aletheiadb::observability::metrics().transaction_commits_total;
    assert_eq!(after_successful_commit, starting_commits + 1);

    let mut update_tx = db.write_transaction().expect("begin update transaction");
    update_tx
        .update_node(
            node_id,
            PropertyMapBuilder::new().insert("name", "Ada").build(),
        )
        .expect("buffer node update");

    let failed_commit = update_tx.commit_with_timestamp();
    assert!(
        failed_commit.is_err(),
        "retention limit should fail during post-WAL apply"
    );

    assert_eq!(
        aletheiadb::observability::metrics().transaction_commits_total,
        after_successful_commit
    );
}
