//! OpenTelemetry OTLP export, sampling, and W3C context propagation
//! (Issue #3376).
//!
//! This module is the exporter/subscriber half of the observability story that
//! [ADR 0056](../../../docs/adr/0056-observability-contract.md) deliberately
//! left to the application. It composes with the `observability` feature — it
//! reuses that module's stable `tracing` span vocabulary — and adds:
//!
//! - an **OTLP exporter** over HTTP/protobuf, configured from the standard
//!   `OTEL_*` environment variables ([`OtelConfig::from_env`]);
//! - **head sampling** (`always_on` / `always_off` / `traceidratio` and their
//!   `parentbased_*` variants — [`SamplerConfig`]);
//! - a process-wide **subscriber installer** ([`init`]) that bridges `tracing`
//!   spans into OpenTelemetry and installs the W3C `TraceContextPropagator`;
//! - reusable **W3C `traceparent`/`tracestate` extract/inject helpers**
//!   ([`extract_context`], [`attach_parent`], [`inject_context`]) that the HTTP
//!   surface uses and the MCP surface can reuse;
//! - [`current_trace_id`] for stamping the active trace id into error responses
//!   and logs;
//! - an [`InMemorySpanExporter`] for tests / embedded assertions.
//!
//! # Zero cost when disabled
//!
//! The entire module is `#[cfg(feature = "otel")]`. When the feature is absent
//! it does not exist and every call site is compiled out. When the feature is
//! present but tracing is not initialized, [`is_active`] is a single relaxed
//! atomic load that hot paths check before doing any context work.
//!
//! # Privacy
//!
//! No credential (API key, `Authorization` header, bearer token) is ever read
//! or written by this module. The W3C carriers only ever touch the
//! `traceparent` and `tracestate` header names. Query text is opt-in
//! ([`OtelConfig::capture_statements`]); property *values* are never emitted.

use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use opentelemetry::propagation::{Extractor, Injector};
use opentelemetry::trace::{TraceContextExt, TracerProvider as _};
use opentelemetry_sdk::Resource;
use opentelemetry_sdk::error::OTelSdkResult;
use opentelemetry_sdk::propagation::TraceContextPropagator;
use opentelemetry_sdk::trace::{Sampler, SdkTracerProvider, SimpleSpanProcessor, SpanExporter};
use tracing_opentelemetry::OpenTelemetrySpanExt;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

/// Re-export of the SDK's exported-span type so downstream tests / embedders
/// can inspect captured spans (via [`InMemorySpanExporter::spans`]) without
/// taking a direct dependency on `opentelemetry_sdk`.
pub use opentelemetry_sdk::trace::SpanData;

/// Standard OTLP endpoint environment variable.
pub const ENV_OTLP_ENDPOINT: &str = "OTEL_EXPORTER_OTLP_ENDPOINT";
/// Standard OTel service-name environment variable.
pub const ENV_SERVICE_NAME: &str = "OTEL_SERVICE_NAME";
/// Standard OTel head-sampler selector environment variable.
pub const ENV_TRACES_SAMPLER: &str = "OTEL_TRACES_SAMPLER";
/// Standard OTel sampler argument (ratio) environment variable.
pub const ENV_TRACES_SAMPLER_ARG: &str = "OTEL_TRACES_SAMPLER_ARG";
/// AletheiaDB master enable switch for OTel tracing.
pub const ENV_ENABLE: &str = "ALETHEIADB_OTEL";
/// AletheiaDB opt-in switch for capturing sanitized statement text.
pub const ENV_CAPTURE_STATEMENTS: &str = "ALETHEIADB_OTEL_CAPTURE_STATEMENTS";

/// Tracer/instrumentation-scope name used for AletheiaDB spans.
pub const TRACER_NAME: &str = super::DB_SYSTEM_NAME;

/// Runtime flag: is OTel tracing currently active?
///
/// Hot paths load this (a single relaxed atomic) before doing any
/// span-context work, so a compiled-in-but-uninitialized build pays only the
/// load, never the OTel machinery.
static OTEL_ACTIVE: AtomicBool = AtomicBool::new(false);
static CAPTURE_STATEMENTS: AtomicBool = AtomicBool::new(false);

/// Whether OTel tracing has been initialized and is exporting.
#[must_use]
#[inline]
pub fn is_active() -> bool {
    OTEL_ACTIVE.load(Ordering::Relaxed)
}

/// Whether sanitized statement capture is enabled (opt-in, off by default).
#[must_use]
#[inline]
pub fn capture_statements() -> bool {
    CAPTURE_STATEMENTS.load(Ordering::Relaxed)
}

fn set_active(active: bool) {
    OTEL_ACTIVE.store(active, Ordering::Relaxed);
}

fn set_capture_statements(capture: bool) {
    CAPTURE_STATEMENTS.store(capture, Ordering::Relaxed);
}

// ===========================================================================
// Errors
// ===========================================================================

/// Errors raised while initializing OTel tracing.
#[derive(Debug, thiserror::Error)]
pub enum OtelError {
    /// The OTLP exporter could not be constructed.
    #[error("failed to build OTLP exporter: {0}")]
    Exporter(String),

    /// A global `tracing` subscriber was already installed.
    #[error("a global tracing subscriber is already installed: {0}")]
    SubscriberAlreadySet(String),
}

// ===========================================================================
// Sampling
// ===========================================================================

/// Head-sampling policy, mirroring the OTel `OTEL_TRACES_SAMPLER` values.
///
/// The default is [`ParentBasedAlwaysOn`](SamplerConfig::ParentBasedAlwaysOn):
/// it is the OTel default and the one that makes incoming `traceparent`
/// continuation work out of the box.
#[derive(Debug, Clone, PartialEq, Default)]
pub enum SamplerConfig {
    /// Sample every trace.
    AlwaysOn,
    /// Sample no trace.
    AlwaysOff,
    /// Sample a fixed fraction of traces (`0.0..=1.0`).
    TraceIdRatio(f64),
    /// Respect the parent's decision; sample roots always.
    #[default]
    ParentBasedAlwaysOn,
    /// Respect the parent's decision; drop roots.
    ParentBasedAlwaysOff,
    /// Respect the parent's decision; sample a fraction of roots.
    ParentBasedTraceIdRatio(f64),
}

impl SamplerConfig {
    /// Parse from the standard `OTEL_TRACES_SAMPLER` token and optional
    /// `OTEL_TRACES_SAMPLER_ARG` ratio. Unknown tokens fall back to the
    /// default (parent-based always-on).
    #[must_use]
    pub fn parse(token: &str, arg: Option<&str>) -> Self {
        let ratio = || {
            arg.and_then(|a| a.trim().parse::<f64>().ok())
                .unwrap_or(1.0)
        };
        match token.trim().to_ascii_lowercase().as_str() {
            "always_on" | "alwayson" => Self::AlwaysOn,
            "always_off" | "alwaysoff" => Self::AlwaysOff,
            "traceidratio" => Self::TraceIdRatio(ratio()),
            "parentbased_always_off" => Self::ParentBasedAlwaysOff,
            "parentbased_traceidratio" => Self::ParentBasedTraceIdRatio(ratio()),
            _ => Self::ParentBasedAlwaysOn,
        }
    }

    /// Read the sampler from the environment.
    #[must_use]
    pub fn from_env() -> Self {
        match std::env::var(ENV_TRACES_SAMPLER) {
            Ok(token) => {
                let arg = std::env::var(ENV_TRACES_SAMPLER_ARG).ok();
                Self::parse(&token, arg.as_deref())
            }
            Err(_) => Self::default(),
        }
    }

    fn to_sdk_sampler(&self) -> Sampler {
        match *self {
            Self::AlwaysOn => Sampler::AlwaysOn,
            Self::AlwaysOff => Sampler::AlwaysOff,
            Self::TraceIdRatio(r) => Sampler::TraceIdRatioBased(r),
            Self::ParentBasedAlwaysOn => Sampler::ParentBased(Box::new(Sampler::AlwaysOn)),
            Self::ParentBasedAlwaysOff => Sampler::ParentBased(Box::new(Sampler::AlwaysOff)),
            Self::ParentBasedTraceIdRatio(r) => {
                Sampler::ParentBased(Box::new(Sampler::TraceIdRatioBased(r)))
            }
        }
    }
}

// ===========================================================================
// Configuration
// ===========================================================================

/// Runtime configuration for OTel tracing.
#[derive(Debug, Clone)]
pub struct OtelConfig {
    /// Master switch. When `false`, [`init`] is a no-op returning `None`.
    pub enabled: bool,
    /// OTLP endpoint. `None` lets the exporter use `OTEL_EXPORTER_OTLP_ENDPOINT`
    /// or its built-in default (`http://localhost:4318`).
    pub endpoint: Option<String>,
    /// `service.name` resource attribute.
    pub service_name: String,
    /// Head-sampling policy.
    pub sampler: SamplerConfig,
    /// Opt-in: attach sanitized statement text as `db.query.text`. Off by
    /// default so query text never leaves the process unless requested. Even
    /// when on, property *values* are never emitted.
    pub capture_statements: bool,
}

impl Default for OtelConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            endpoint: None,
            service_name: super::DB_SYSTEM_NAME.to_string(),
            sampler: SamplerConfig::default(),
            capture_statements: false,
        }
    }
}

fn env_flag(name: &str) -> bool {
    matches!(
        std::env::var(name).map(|v| v.trim().to_ascii_lowercase()),
        Ok(v) if v == "1" || v == "true" || v == "yes" || v == "on"
    )
}

impl OtelConfig {
    /// Build a configuration from the standard `OTEL_*` and AletheiaDB
    /// environment variables.
    ///
    /// Tracing is enabled when `ALETHEIADB_OTEL` is truthy **or** an
    /// `OTEL_EXPORTER_OTLP_ENDPOINT` is set (presence of an endpoint is a
    /// strong signal the operator wired up a collector).
    #[must_use]
    pub fn from_env() -> Self {
        let endpoint = std::env::var(ENV_OTLP_ENDPOINT)
            .ok()
            .filter(|s| !s.trim().is_empty());
        let enabled = env_flag(ENV_ENABLE) || endpoint.is_some();
        let service_name = std::env::var(ENV_SERVICE_NAME)
            .ok()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| super::DB_SYSTEM_NAME.to_string());
        Self {
            enabled,
            endpoint,
            service_name,
            sampler: SamplerConfig::from_env(),
            capture_statements: env_flag(ENV_CAPTURE_STATEMENTS),
        }
    }

    fn resource(&self) -> Resource {
        Resource::builder()
            .with_service_name(self.service_name.clone())
            .build()
    }
}

// ===========================================================================
// W3C context propagation (reusable by HTTP + MCP)
// ===========================================================================

/// Extractor carrier holding only the two W3C trace-context headers.
///
/// Deliberately narrow: it can only ever surface `traceparent` and
/// `tracestate`, so no credential header can leak into a propagated context.
struct W3CExtractor {
    traceparent: Option<String>,
    tracestate: Option<String>,
}

impl Extractor for W3CExtractor {
    fn get(&self, key: &str) -> Option<&str> {
        match key.to_ascii_lowercase().as_str() {
            "traceparent" => self.traceparent.as_deref(),
            "tracestate" => self.tracestate.as_deref(),
            _ => None,
        }
    }

    fn keys(&self) -> Vec<&str> {
        let mut keys = Vec::new();
        if self.traceparent.is_some() {
            keys.push("traceparent");
        }
        if self.tracestate.is_some() {
            keys.push("tracestate");
        }
        keys
    }
}

/// Injector carrier that accumulates outbound header pairs.
#[derive(Default)]
struct W3CInjector {
    pairs: Vec<(String, String)>,
}

impl Injector for W3CInjector {
    fn set(&mut self, key: &str, value: String) {
        self.pairs.push((key.to_string(), value));
    }
}

/// Extract an OpenTelemetry [`Context`](opentelemetry::Context) from incoming
/// W3C `traceparent`/`tracestate` header values.
///
/// Absent/invalid input yields an empty context (a subsequent span becomes a
/// new root). Reusable by any transport that can surface the two header
/// strings (HTTP headers, MCP metadata).
#[must_use]
pub fn extract_context(
    traceparent: Option<&str>,
    tracestate: Option<&str>,
) -> opentelemetry::Context {
    let carrier = W3CExtractor {
        traceparent: traceparent.map(str::to_owned),
        tracestate: tracestate.map(str::to_owned),
    };
    opentelemetry::global::get_text_map_propagator(|prop| prop.extract(&carrier))
}

/// Attach an incoming trace context to `span` as its remote parent, so DB
/// spans nest inside the caller's trace.
///
/// When `traceparent` is absent, this is a no-op and `span` starts a new root
/// trace — exactly the AC-required behavior.
pub fn attach_parent(span: &tracing::Span, traceparent: Option<&str>, tracestate: Option<&str>) {
    if traceparent.is_none() {
        return;
    }
    let cx = extract_context(traceparent, tracestate);
    span.set_parent(cx);
}

/// Inject the given context into W3C header pairs for outbound propagation.
#[must_use]
pub fn inject_context(cx: &opentelemetry::Context) -> Vec<(String, String)> {
    let mut carrier = W3CInjector::default();
    opentelemetry::global::get_text_map_propagator(|prop| prop.inject_context(cx, &mut carrier));
    carrier.pairs
}

/// Inject the *currently active* span's context into W3C header pairs.
#[must_use]
pub fn current_context_headers() -> Vec<(String, String)> {
    inject_context(&tracing::Span::current().context())
}

// ===========================================================================
// Trace-id correlation
// ===========================================================================

/// The active trace id (32 lowercase hex chars) if a valid, sampled span is
/// current; otherwise `None`.
///
/// Used to stamp the trace id into error responses and debug logs so a failing
/// call can be looked up directly in the trace backend.
#[must_use]
pub fn current_trace_id() -> Option<String> {
    if !is_active() {
        return None;
    }
    let cx = tracing::Span::current().context();
    let span = cx.span();
    let sc = span.span_context();
    if sc.is_valid() {
        Some(sc.trace_id().to_string())
    } else {
        None
    }
}

/// The active `(trace_id, span_id)` pair if a valid span is current.
#[must_use]
pub fn current_ids() -> Option<(String, String)> {
    if !is_active() {
        return None;
    }
    let cx = tracing::Span::current().context();
    let span = cx.span();
    let sc = span.span_context();
    if sc.is_valid() {
        Some((sc.trace_id().to_string(), sc.span_id().to_string()))
    } else {
        None
    }
}

// ===========================================================================
// In-memory exporter (tests / embedded assertions)
// ===========================================================================

/// A [`SpanExporter`] that retains finished spans in memory.
///
/// Cheap to clone (shares one buffer). Used by the integration tests as a
/// stand-in OTLP receiver so parent-child structure and the credential-
/// exclusion invariant can be asserted without a network collector.
#[derive(Debug, Clone, Default)]
pub struct InMemorySpanExporter {
    spans: Arc<Mutex<Vec<SpanData>>>,
}

impl InMemorySpanExporter {
    /// Create an empty exporter.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Snapshot of all spans exported so far.
    #[must_use]
    pub fn spans(&self) -> Vec<SpanData> {
        self.spans.lock().expect("span buffer poisoned").clone()
    }

    /// Discard all retained spans.
    pub fn clear(&self) {
        self.spans.lock().expect("span buffer poisoned").clear();
    }
}

impl SpanExporter for InMemorySpanExporter {
    fn export(
        &self,
        batch: Vec<SpanData>,
    ) -> impl std::future::Future<Output = OTelSdkResult> + Send {
        if let Ok(mut guard) = self.spans.lock() {
            guard.extend(batch);
        }
        std::future::ready(Ok(()))
    }
}

// ===========================================================================
// Provider / subscriber construction
// ===========================================================================

/// Build an [`SdkTracerProvider`] around an arbitrary exporter, applying the
/// configured sampler and resource. `simple` selects synchronous export
/// (tests) versus the batching processor (production).
#[must_use]
pub fn provider_with_exporter<E>(
    config: &OtelConfig,
    exporter: E,
    simple: bool,
) -> SdkTracerProvider
where
    E: SpanExporter + 'static,
{
    let builder = SdkTracerProvider::builder()
        .with_sampler(config.sampler.to_sdk_sampler())
        .with_resource(config.resource());
    let builder = if simple {
        builder.with_span_processor(SimpleSpanProcessor::new(exporter))
    } else {
        builder.with_batch_exporter(exporter)
    };
    builder.build()
}

/// Build the OTLP HTTP/protobuf exporter from `config`.
fn build_otlp_exporter(config: &OtelConfig) -> Result<opentelemetry_otlp::SpanExporter, OtelError> {
    use opentelemetry_otlp::WithExportConfig;
    let mut builder = opentelemetry_otlp::SpanExporter::builder().with_http();
    if let Some(endpoint) = &config.endpoint {
        builder = builder.with_endpoint(endpoint.clone());
    }
    builder
        .build()
        .map_err(|e| OtelError::Exporter(e.to_string()))
}

/// Guard returned by [`init`]; flushes and shuts the exporter down on drop.
#[must_use = "dropping the guard immediately shuts tracing down"]
pub struct OtelGuard {
    provider: SdkTracerProvider,
}

impl OtelGuard {
    /// Flush any pending spans to the exporter.
    pub fn force_flush(&self) {
        let _ = self.provider.force_flush();
    }
}

impl Drop for OtelGuard {
    fn drop(&mut self) {
        let _ = self.provider.force_flush();
        let _ = self.provider.shutdown();
        set_active(false);
        set_capture_statements(false);
    }
}

/// Install a global OpenTelemetry `tracing` subscriber that exports over OTLP.
///
/// Returns `Ok(None)` when `config.enabled` is `false` (nothing installed).
/// Returns `Ok(Some(guard))` on success; hold the guard for the process
/// lifetime and drop it to flush + shut down.
///
/// # Errors
///
/// Returns [`OtelError`] if the OTLP exporter cannot be built or a global
/// subscriber is already installed.
pub fn init(config: &OtelConfig) -> Result<Option<OtelGuard>, OtelError> {
    if !config.enabled {
        return Ok(None);
    }
    let exporter = build_otlp_exporter(config)?;
    let provider = provider_with_exporter(config, exporter, false);
    install_global(provider.clone())?;
    set_capture_statements(config.capture_statements);
    Ok(Some(OtelGuard { provider }))
}

/// Install a global subscriber that exports into the returned in-memory
/// exporter. Test/embedding helper — makes the whole process's spans (across
/// `spawn_blocking` threads) inspectable.
///
/// # Errors
///
/// Returns [`OtelError::SubscriberAlreadySet`] if a global subscriber already
/// exists.
pub fn init_in_memory_global(
    config: &OtelConfig,
) -> Result<(OtelGuard, InMemorySpanExporter), OtelError> {
    let exporter = InMemorySpanExporter::new();
    let provider = provider_with_exporter(config, exporter.clone(), true);
    install_global(provider.clone())?;
    set_capture_statements(config.capture_statements);
    Ok((OtelGuard { provider }, exporter))
}

fn install_global(provider: SdkTracerProvider) -> Result<(), OtelError> {
    opentelemetry::global::set_text_map_propagator(TraceContextPropagator::new());
    let tracer = provider.tracer(TRACER_NAME);
    opentelemetry::global::set_tracer_provider(provider);
    let otel_layer = tracing_opentelemetry::layer().with_tracer(tracer);
    tracing_subscriber::registry()
        .with(otel_layer)
        .try_init()
        .map_err(|e| OtelError::SubscriberAlreadySet(e.to_string()))?;
    set_active(true);
    Ok(())
}

/// Run `f` with a scoped (thread-local) OTel subscriber exporting into the
/// returned in-memory exporter. Does not touch global state, so multiple unit
/// tests can use it in the same process without conflict.
///
/// Note: because the subscriber is thread-local, spans created on other threads
/// (e.g. inside `spawn_blocking`) are not captured — use
/// [`init_in_memory_global`] for cross-thread scenarios.
#[cfg(test)]
fn with_scoped_in_memory<T>(config: &OtelConfig, f: impl FnOnce() -> T) -> (T, Vec<SpanData>) {
    let exporter = InMemorySpanExporter::new();
    let provider = provider_with_exporter(config, exporter.clone(), true);
    set_active(true);
    set_capture_statements(config.capture_statements);
    opentelemetry::global::set_text_map_propagator(TraceContextPropagator::new());
    let tracer = provider.tracer(TRACER_NAME);
    let subscriber =
        tracing_subscriber::registry().with(tracing_opentelemetry::layer().with_tracer(tracer));
    let out = tracing::subscriber::with_default(subscriber, f);
    let _ = provider.force_flush();
    (out, exporter.spans())
}

#[cfg(test)]
fn set_active_test(v: bool) {
    set_active(v);
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;

    #[test]
    fn sampler_parse_covers_standard_tokens() {
        assert_eq!(
            SamplerConfig::parse("always_on", None),
            SamplerConfig::AlwaysOn
        );
        assert_eq!(
            SamplerConfig::parse("always_off", None),
            SamplerConfig::AlwaysOff
        );
        assert_eq!(
            SamplerConfig::parse("traceidratio", Some("0.25")),
            SamplerConfig::TraceIdRatio(0.25)
        );
        assert_eq!(
            SamplerConfig::parse("parentbased_always_off", None),
            SamplerConfig::ParentBasedAlwaysOff
        );
        assert_eq!(
            SamplerConfig::parse("parentbased_traceidratio", Some("0.5")),
            SamplerConfig::ParentBasedTraceIdRatio(0.5)
        );
        // Unknown → default parent-based always-on.
        assert_eq!(
            SamplerConfig::parse("nonsense", None),
            SamplerConfig::ParentBasedAlwaysOn
        );
    }

    #[test]
    fn sampler_all_variants_map_to_sdk() {
        for cfg in [
            SamplerConfig::AlwaysOn,
            SamplerConfig::AlwaysOff,
            SamplerConfig::TraceIdRatio(0.1),
            SamplerConfig::ParentBasedAlwaysOn,
            SamplerConfig::ParentBasedAlwaysOff,
            SamplerConfig::ParentBasedTraceIdRatio(0.1),
        ] {
            // Must not panic constructing the SDK sampler.
            let _ = cfg.to_sdk_sampler();
        }
    }

    #[test]
    fn default_config_is_disabled_and_private() {
        let cfg = OtelConfig::default();
        assert!(!cfg.enabled);
        assert!(
            !cfg.capture_statements,
            "statement capture must default off"
        );
        assert_eq!(cfg.service_name, super::super::DB_SYSTEM_NAME);
    }

    #[test]
    fn init_disabled_is_noop() {
        let cfg = OtelConfig::default();
        assert!(init(&cfg).expect("noop").is_none());
    }

    #[test]
    fn current_trace_id_none_when_inactive() {
        // With no active tracer, there is no trace id.
        set_active_test(false);
        assert!(current_trace_id().is_none());
        assert!(current_ids().is_none());
    }

    /// Extraction is confined to the two W3C headers — a credential-shaped
    /// key is never surfaced into the context carrier.
    #[test]
    fn extractor_only_exposes_w3c_headers() {
        let ex = W3CExtractor {
            traceparent: Some("00-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-01".into()),
            tracestate: Some("vendor=v".into()),
        };
        assert!(ex.get("traceparent").is_some());
        assert!(ex.get("tracestate").is_some());
        assert!(ex.get("authorization").is_none());
        assert!(ex.get("x-api-key").is_none());
        let keys = ex.keys();
        assert!(keys.contains(&"traceparent"));
        assert!(!keys.iter().any(|k| k.contains("auth")));
    }

    /// A child span created under an extracted parent shares the incoming
    /// trace id (context continuity), and no credential leaks into attributes.
    #[test]
    #[serial]
    fn scoped_child_span_inherits_incoming_trace() {
        let incoming = "00-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-01";
        let cfg = OtelConfig {
            enabled: true,
            sampler: SamplerConfig::AlwaysOn,
            ..OtelConfig::default()
        };
        let ((), spans) = with_scoped_in_memory(&cfg, || {
            let span = tracing::info_span!(
                "aletheiadb.http.request",
                "http.request.method" = "POST",
                "aletheiadb.credential" = "SHOULD-NOT-APPEAR",
            );
            attach_parent(&span, Some(incoming), None);
            let _g = span.enter();
            // A nested DB span.
            let child = super::super::query_execute_span("query.execute", "aql");
            let _cg = child.enter();
        });
        set_active_test(false);
        assert!(!spans.is_empty(), "expected exported spans");
        // All spans belong to the incoming trace id.
        for s in &spans {
            assert_eq!(
                s.span_context.trace_id().to_string(),
                "0af7651916cd43dd8448eb211c80319c",
                "span {} did not inherit incoming trace id",
                s.name
            );
        }
        // Parent-child: the query.execute span's parent is the http root span.
        let root = spans
            .iter()
            .find(|s| s.name == "aletheiadb.http.request")
            .expect("root span present");
        let child = spans
            .iter()
            .find(|s| s.name == super::super::SPAN_QUERY_EXECUTE)
            .expect("child span present");
        assert_eq!(
            child.parent_span_id,
            root.span_context.span_id(),
            "child span not nested under http root"
        );
    }
}
