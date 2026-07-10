//! Backend-agnostic observability contract for AletheiaDB.
//!
//! AletheiaDB emits OpenTelemetry-compatible spans through [`tracing`] and
//! reports bounded-cardinality metrics through [`MetricsRecorder`]. The crate
//! deliberately does not own exporters, collectors, HTTP scrape endpoints, or
//! vendor clients. Applications install their preferred `tracing` subscriber
//! and, when desired, enable the `metrics-rs` feature to forward metric samples
//! into the [`metrics`](https://docs.rs/metrics) facade.

pub mod metrics;

#[cfg(feature = "metrics-rs")]
pub mod metrics_rs_adapter;

#[cfg(feature = "otel")]
pub mod otel;

use arc_swap::ArcSwap;
use std::sync::{Arc, LazyLock};

pub use metrics::{
    CriticalEvent, ErrorCategory, METRIC_CRITICAL_EVENTS, METRIC_ERRORS, METRIC_LABEL_CATEGORY,
    METRIC_LABEL_DURABILITY_MODE, METRIC_LABEL_EVENT, METRIC_LABEL_STATUS,
    METRIC_TRANSACTION_COMMIT_DURATION, METRIC_TRANSACTION_COMMITS, METRIC_TRANSACTION_OPERATIONS,
    METRIC_WRITE_CONFLICTS, METRICS, Metrics, MetricsRecorder, MetricsSnapshot, NoOpMetrics,
};

/// Stable database system name used in OTel attributes.
pub const DB_SYSTEM_NAME: &str = "aletheiadb";

/// Span: parses, plans, or executes an AletheiaDB query.
pub const SPAN_QUERY_EXECUTE: &str = "aletheiadb.query.execute";

/// Span: combines graph traversal, vector ranking, or temporal query work.
pub const SPAN_QUERY_HYBRID: &str = "aletheiadb.query.hybrid";

/// Span: mutates vector index configuration or metadata.
pub const SPAN_VECTOR_INDEX: &str = "aletheiadb.vector.index";

/// Span: executes a vector similarity search.
pub const SPAN_VECTOR_SEARCH: &str = "aletheiadb.vector.search";

/// Span: reconstructs or queries temporal state.
pub const SPAN_TEMPORAL_QUERY: &str = "aletheiadb.temporal.query";

/// Span: reads historical storage directly.
pub const SPAN_STORAGE_HISTORICAL_QUERY: &str = "aletheiadb.storage.historical.query";

/// Span: commits a write transaction.
pub const SPAN_TRANSACTION_COMMIT: &str = "aletheiadb.transaction.commit";

/// Span: serves an inbound HTTP request (the root span of the HTTP surface,
/// under which query/write child spans nest). Issue #3376.
pub const SPAN_HTTP_REQUEST: &str = "aletheiadb.http.request";

/// OTel semantic attribute: database system name.
pub const ATTR_DB_SYSTEM_NAME: &str = "db.system.name";

/// OTel semantic attribute: database operation name.
pub const ATTR_DB_OPERATION_NAME: &str = "db.operation.name";

/// AletheiaDB attribute: query family such as `aql`, `hybrid`, or `temporal`.
pub const ATTR_QUERY_KIND: &str = "aletheiadb.query.kind";

/// AletheiaDB attribute: vector property being indexed or searched.
pub const ATTR_VECTOR_PROPERTY: &str = "aletheiadb.vector.property";

/// AletheiaDB attribute: durability mode for a committed transaction.
pub const ATTR_DURABILITY_MODE: &str = "aletheiadb.durability.mode";

/// AletheiaDB attribute: transaction identifier.
pub const ATTR_TRANSACTION_ID: &str = "aletheiadb.transaction.id";

/// AletheiaDB attribute: bounded error category.
pub const ATTR_ERROR_CATEGORY: &str = "aletheiadb.error.category";

/// AletheiaDB attribute: bounded operation status.
pub const ATTR_STATUS: &str = "aletheiadb.status";

/// AletheiaDB attribute: high-level operation name (e.g. the `/query`
/// operation tag). Bounded set; never contains user data.
pub const ATTR_OPERATION: &str = "aletheiadb.operation";

/// AletheiaDB attribute: number of entities/rows returned by an operation.
pub const ATTR_RESULT_COUNT: &str = "aletheiadb.result.count";

/// AletheiaDB attribute: whether the operation was temporally scoped
/// (`as_of`) versus current-state.
pub const ATTR_TEMPORAL_SCOPE: &str = "aletheiadb.temporal.scope";

/// AletheiaDB attribute: structured error code (Issue #3234) on failure.
pub const ATTR_ERROR_CODE: &str = "aletheiadb.error.code";

/// OTel semantic attribute: HTTP request method.
pub const ATTR_HTTP_REQUEST_METHOD: &str = "http.request.method";

/// OTel semantic attribute: URL path (route), never the query string.
pub const ATTR_URL_PATH: &str = "url.path";

/// OTel semantic attribute: sanitized database statement text. Emitted only
/// when statement capture is explicitly opted into; property *values* are
/// never included.
pub const ATTR_DB_QUERY_TEXT: &str = "db.query.text";

static METRICS_RECORDER: LazyLock<ArcSwap<SharedMetricsRecorder>> =
    LazyLock::new(|| ArcSwap::from_pointee(SharedMetricsRecorder::new(Arc::new(NoOpMetrics))));

struct SharedMetricsRecorder {
    inner: Arc<dyn MetricsRecorder>,
}

impl SharedMetricsRecorder {
    fn new(inner: Arc<dyn MetricsRecorder>) -> Self {
        Self { inner }
    }
}

impl MetricsRecorder for SharedMetricsRecorder {
    fn record_error(&self, category: ErrorCategory) {
        self.inner.record_error(category);
    }

    fn record_write_conflict(&self) {
        self.inner.record_write_conflict();
    }

    fn record_critical_event(&self, event: CriticalEvent) {
        self.inner.record_critical_event(event);
    }

    fn record_transaction_commit(
        &self,
        duration_secs: f64,
        operations_count: u64,
        durability_mode: &str,
        status: &str,
    ) {
        self.inner.record_transaction_commit(
            duration_secs,
            operations_count,
            durability_mode,
            status,
        );
    }
}

/// Bundle of telemetry dependencies supplied by the embedding application.
///
/// `TelemetryConfig::default()` keeps metrics disabled. Spans are emitted via
/// `tracing` call sites and are controlled by whichever subscriber the
/// application installs.
#[derive(Clone)]
pub struct TelemetryConfig {
    /// Service name applications can stamp onto their subscriber/resource.
    pub service_name: Arc<str>,

    /// Metrics sink for contract-defined samples.
    pub metrics: Arc<dyn MetricsRecorder>,
}

impl std::fmt::Debug for TelemetryConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TelemetryConfig")
            .field("service_name", &self.service_name)
            .field("metrics", &std::any::type_name_of_val(&*self.metrics))
            .finish()
    }
}

impl Default for TelemetryConfig {
    fn default() -> Self {
        Self {
            service_name: Arc::from(DB_SYSTEM_NAME),
            metrics: Arc::new(NoOpMetrics),
        }
    }
}

impl TelemetryConfig {
    /// Start a builder with safe no-op defaults.
    #[must_use]
    pub fn builder() -> TelemetryConfigBuilder {
        TelemetryConfigBuilder::default()
    }
}

/// Fluent builder for [`TelemetryConfig`].
#[derive(Default)]
pub struct TelemetryConfigBuilder {
    service_name: Option<Arc<str>>,
    metrics: Option<Arc<dyn MetricsRecorder>>,
}

impl TelemetryConfigBuilder {
    /// Override the service name.
    #[must_use]
    pub fn service_name(mut self, service_name: impl Into<Arc<str>>) -> Self {
        self.service_name = Some(service_name.into());
        self
    }

    /// Register a metrics recorder.
    #[must_use]
    pub fn metrics(mut self, metrics: Arc<dyn MetricsRecorder>) -> Self {
        self.metrics = Some(metrics);
        self
    }

    /// Finalize the configuration.
    #[must_use]
    pub fn build(self) -> TelemetryConfig {
        TelemetryConfig {
            service_name: self
                .service_name
                .unwrap_or_else(|| Arc::from(DB_SYSTEM_NAME)),
            metrics: self.metrics.unwrap_or_else(|| Arc::new(NoOpMetrics)),
        }
    }
}

/// Install process-wide telemetry hooks for AletheiaDB metrics.
///
/// This intentionally does not install a `tracing` subscriber. Subscriber and
/// exporter ownership remains with the application.
pub fn install(config: TelemetryConfig) {
    METRICS_RECORDER.store(Arc::new(SharedMetricsRecorder::new(config.metrics)));
}

/// Get a snapshot of the built-in atomic metric mirror.
#[must_use]
pub fn metrics() -> MetricsSnapshot {
    METRICS.snapshot()
}

/// Record an error against both the atomic mirror and an installed recorder.
pub fn record_error(category: ErrorCategory) {
    METRICS.record_error(category);
    with_installed_recorder(|recorder| recorder.record_error(category));
}

/// Record a write conflict.
pub fn record_write_conflict() {
    METRICS.record_write_conflict();
    with_installed_recorder(|recorder| recorder.record_write_conflict());
}

/// Record a critical event.
pub fn record_critical_event(event: CriticalEvent) {
    METRICS.record_critical_event(event);
    with_installed_recorder(|recorder| recorder.record_critical_event(event));
}

/// Record a transaction commit.
pub fn record_transaction_commit(
    duration_secs: f64,
    operations_count: u64,
    durability_mode: &str,
    status: &str,
) {
    METRICS.record_transaction_commit(duration_secs, operations_count, durability_mode, status);
    with_installed_recorder(|recorder| {
        recorder.record_transaction_commit(
            duration_secs,
            operations_count,
            durability_mode,
            status,
        );
    });
}

fn with_installed_recorder(f: impl FnOnce(&dyn MetricsRecorder)) {
    let recorder = METRICS_RECORDER.load();
    f(&**recorder);
}

/// Create the standard query execution span.
#[must_use]
pub fn query_execute_span(operation_name: &str, query_kind: &str) -> tracing::Span {
    tracing::info_span!(
        SPAN_QUERY_EXECUTE,
        "db.system.name" = DB_SYSTEM_NAME,
        "db.operation.name" = operation_name,
        "aletheiadb.query.kind" = query_kind,
    )
}

/// Create the standard hybrid query span.
#[must_use]
pub fn hybrid_query_span(operation_name: &str) -> tracing::Span {
    tracing::info_span!(
        SPAN_QUERY_HYBRID,
        "db.system.name" = DB_SYSTEM_NAME,
        "db.operation.name" = operation_name,
        "aletheiadb.query.kind" = "hybrid",
    )
}

/// Create the standard vector index span.
#[must_use]
pub fn vector_index_span(operation_name: &str, property_name: &str) -> tracing::Span {
    tracing::info_span!(
        SPAN_VECTOR_INDEX,
        "db.system.name" = DB_SYSTEM_NAME,
        "db.operation.name" = operation_name,
        "aletheiadb.vector.property" = property_name,
    )
}

/// Create the standard vector search span.
#[must_use]
pub fn vector_search_span(operation_name: &str, property_name: &str) -> tracing::Span {
    tracing::info_span!(
        SPAN_VECTOR_SEARCH,
        "db.system.name" = DB_SYSTEM_NAME,
        "db.operation.name" = operation_name,
        "aletheiadb.vector.property" = property_name,
    )
}

/// Create the standard temporal query span.
#[must_use]
pub fn temporal_query_span(operation_name: &str) -> tracing::Span {
    tracing::info_span!(
        SPAN_TEMPORAL_QUERY,
        "db.system.name" = DB_SYSTEM_NAME,
        "db.operation.name" = operation_name,
        "aletheiadb.query.kind" = "temporal",
    )
}

/// Create the standard historical-storage query span.
#[must_use]
pub fn historical_storage_query_span(operation_name: &str) -> tracing::Span {
    tracing::info_span!(
        SPAN_STORAGE_HISTORICAL_QUERY,
        "db.system.name" = DB_SYSTEM_NAME,
        "db.operation.name" = operation_name,
        "aletheiadb.query.kind" = "temporal",
    )
}

/// Create the standard transaction commit span.
#[must_use]
pub fn transaction_commit_span(tx_id: &str, durability_mode: &str) -> tracing::Span {
    tracing::info_span!(
        SPAN_TRANSACTION_COMMIT,
        "db.system.name" = DB_SYSTEM_NAME,
        "db.operation.name" = "transaction.commit",
        "aletheiadb.transaction.id" = tx_id,
        "aletheiadb.durability.mode" = durability_mode,
    )
}

/// Create the root HTTP request span (Issue #3376).
///
/// The span carries the request method and route path up front. The
/// operation name, result count, temporal scope, statement text, and error
/// code are recorded later via [`tracing::Span::record`] once known — they are
/// declared here as empty fields so the recording sites are cheap and the
/// attribute set is stable. Credentials are deliberately never captured.
#[must_use]
pub fn http_request_span(method: &str, path: &str) -> tracing::Span {
    // `parent: None` makes this a true root span, independent of any ambient
    // tracing span (e.g. a tower `TraceLayer` request span). An incoming W3C
    // context is attached afterward via `otel::attach_parent`, so the span is
    // either a fresh root (absent traceparent) or nests under the caller's
    // remote span — never accidentally under framework middleware.
    tracing::info_span!(
        parent: None,
        SPAN_HTTP_REQUEST,
        "db.system.name" = DB_SYSTEM_NAME,
        "http.request.method" = method,
        "url.path" = path,
        "aletheiadb.operation" = tracing::field::Empty,
        "aletheiadb.result.count" = tracing::field::Empty,
        "aletheiadb.temporal.scope" = tracing::field::Empty,
        "aletheiadb.error.code" = tracing::field::Empty,
        "db.query.text" = tracing::field::Empty,
        "otel.status_code" = tracing::field::Empty,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_telemetry_is_noop() {
        let telemetry = TelemetryConfig::default();
        assert_eq!(&*telemetry.service_name, DB_SYSTEM_NAME);
        telemetry.metrics.record_error(ErrorCategory::Other);
    }

    #[test]
    fn install_accepts_replacement_config() {
        install(TelemetryConfig::default());
        install(
            TelemetryConfig::builder()
                .service_name("test-service")
                .build(),
        );
        record_error(ErrorCategory::Query);
    }
}
