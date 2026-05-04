//! `metrics` crate adapter for AletheiaDB's metrics contract.
//!
//! Enable with the `metrics-rs` feature, install any `metrics`-compatible
//! exporter in the host application, then register [`MetricsRsRecorder`] via
//! [`TelemetryConfig`](crate::observability::TelemetryConfig).

use metrics::{counter, histogram};

use crate::observability::{
    CriticalEvent, ErrorCategory, METRIC_CRITICAL_EVENTS, METRIC_ERRORS, METRIC_LABEL_CATEGORY,
    METRIC_LABEL_DURABILITY_MODE, METRIC_LABEL_EVENT, METRIC_LABEL_STATUS,
    METRIC_TRANSACTION_COMMIT_DURATION, METRIC_TRANSACTION_COMMITS, METRIC_TRANSACTION_OPERATIONS,
    METRIC_WRITE_CONFLICTS, MetricsRecorder,
};

/// [`MetricsRecorder`] implementation backed by the global `metrics` registry.
#[derive(Debug, Clone, Copy, Default)]
pub struct MetricsRsRecorder;

impl MetricsRecorder for MetricsRsRecorder {
    fn record_error(&self, category: ErrorCategory) {
        counter!(
            METRIC_ERRORS,
            METRIC_LABEL_CATEGORY => category.as_str(),
        )
        .increment(1);
    }

    fn record_write_conflict(&self) {
        counter!(METRIC_WRITE_CONFLICTS).increment(1);
    }

    fn record_critical_event(&self, event: CriticalEvent) {
        counter!(
            METRIC_CRITICAL_EVENTS,
            METRIC_LABEL_EVENT => event.as_str(),
        )
        .increment(1);
    }

    #[allow(clippy::cast_precision_loss)]
    fn record_transaction_commit(
        &self,
        duration_secs: f64,
        operations_count: u64,
        durability_mode: &str,
        status: &str,
    ) {
        counter!(
            METRIC_TRANSACTION_COMMITS,
            METRIC_LABEL_DURABILITY_MODE => durability_mode.to_owned(),
            METRIC_LABEL_STATUS => status.to_owned(),
        )
        .increment(1);

        histogram!(
            METRIC_TRANSACTION_COMMIT_DURATION,
            METRIC_LABEL_DURABILITY_MODE => durability_mode.to_owned(),
            METRIC_LABEL_STATUS => status.to_owned(),
        )
        .record(duration_secs);

        histogram!(
            METRIC_TRANSACTION_OPERATIONS,
            METRIC_LABEL_DURABILITY_MODE => durability_mode.to_owned(),
            METRIC_LABEL_STATUS => status.to_owned(),
        )
        .record(operations_count as f64);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::observability::TelemetryConfig;
    use std::sync::Arc;

    #[test]
    fn metrics_rs_recorder_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<MetricsRsRecorder>();
    }

    #[test]
    fn metrics_rs_recorder_can_be_arc_boxed() {
        let _: Arc<dyn MetricsRecorder> = Arc::new(MetricsRsRecorder);
    }

    #[test]
    fn methods_do_not_panic_without_global_recorder() {
        let recorder = MetricsRsRecorder;
        recorder.record_error(ErrorCategory::Storage);
        recorder.record_write_conflict();
        recorder.record_critical_event(CriticalEvent::LockPoison);
        recorder.record_transaction_commit(0.001, 2, "Synchronous", "committed");
    }

    #[test]
    fn telemetry_builder_accepts_metrics_rs_recorder() {
        let telemetry = TelemetryConfig::builder()
            .metrics(Arc::new(MetricsRsRecorder))
            .build();

        telemetry.metrics.record_error(ErrorCategory::Query);
    }
}
