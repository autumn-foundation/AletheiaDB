#![cfg(feature = "observability")]

use std::sync::Arc;

use aletheiadb::observability::{
    ATTR_DB_OPERATION_NAME, ATTR_DB_SYSTEM_NAME, ATTR_DURABILITY_MODE, ATTR_ERROR_CATEGORY,
    ATTR_QUERY_KIND, ATTR_STATUS, DB_SYSTEM_NAME, ErrorCategory, METRIC_ERRORS,
    METRIC_LABEL_CATEGORY, METRIC_LABEL_DURABILITY_MODE, METRIC_LABEL_STATUS,
    METRIC_TRANSACTION_COMMITS, MetricsRecorder, NoOpMetrics, SPAN_QUERY_EXECUTE,
    SPAN_TRANSACTION_COMMIT, SPAN_VECTOR_SEARCH, TelemetryConfig, transaction_commit_span,
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
