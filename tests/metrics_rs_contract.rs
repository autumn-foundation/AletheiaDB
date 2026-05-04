#![cfg(feature = "metrics-rs")]

use std::sync::Arc;

use aletheiadb::observability::{
    ErrorCategory, MetricsRecorder, TelemetryConfig, metrics_rs_adapter::MetricsRsRecorder,
};

#[test]
fn metrics_rs_recorder_wires_into_telemetry_config() {
    let recorder: Arc<dyn MetricsRecorder> = Arc::new(MetricsRsRecorder);
    let telemetry = TelemetryConfig::builder().metrics(recorder).build();

    telemetry.metrics.record_error(ErrorCategory::Storage);
    telemetry
        .metrics
        .record_transaction_commit(0.001, 3, "Synchronous", "committed");
}
