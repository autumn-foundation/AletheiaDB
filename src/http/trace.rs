//! HTTP root-span extractor and W3C context propagation (Issue #3376).
//!
//! autumn-web's sealed `IntoAppLayer` bound blocks arbitrary tower middleware
//! on the production `run_server` path (see [`server`](super::server) and
//! [`auth`](super::auth)), so — exactly like [`AuthContext`](super::AuthContext)
//! — the HTTP root span is created by an **extractor** that runs before the
//! handler body rather than a tower `Layer`. This keeps production and the
//! test router in parity.
//!
//! The extractor:
//!
//! - builds the [`SPAN_HTTP_REQUEST`](crate::observability::SPAN_HTTP_REQUEST)
//!   root span carrying the request method and route path;
//! - honors an incoming W3C `traceparent`/`tracestate` so database spans nest
//!   inside the caller's trace (absent → a new root trace); and
//! - exposes recording methods the handler calls to stamp the operation name,
//!   result count, and error code onto the span once known.
//!
//! Credentials (`Authorization`, `x-api-key`, bearer tokens) are never read
//! here and never become span attributes.
//!
//! When the `observability` feature is disabled the extractor is a zero-sized
//! no-op, so handler signatures are identical across feature sets.

use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use std::convert::Infallible;

/// Per-request tracing context: owns the HTTP root span.
pub struct HttpTrace {
    #[cfg(feature = "observability")]
    span: tracing::Span,
}

impl HttpTrace {
    /// A clone of the root span, for instrumenting the handler body so that
    /// database child spans (query/commit) nest underneath it.
    #[cfg(feature = "observability")]
    #[must_use]
    pub fn span(&self) -> tracing::Span {
        self.span.clone()
    }

    /// Whether recording per-request attributes onto the root span is
    /// worthwhile. Callers gate the *computation* of attribute values on this so
    /// that no work is done when nothing will consume it (Issue #3376, MED2 /
    /// Perf):
    ///
    /// - `false` when no subscriber is interested in the span at all
    ///   (`span.is_disabled()`), i.e. a compiled-in-but-runtime-disabled build;
    /// - with the OTel exporter active, this reflects the head-sampling
    ///   decision — a **sampled-out** span returns `false`, so per-request
    ///   attribute recording is skipped for traces that will never be exported;
    /// - otherwise `true` (some non-OTel subscriber may record the span).
    ///
    /// Compiled out entirely when `observability` is absent, so a feature-absent
    /// build has no call site to evaluate.
    #[cfg(feature = "observability")]
    #[must_use]
    #[inline]
    pub fn is_recording(&self) -> bool {
        if self.span.is_disabled() {
            return false;
        }
        #[cfg(feature = "otel")]
        if crate::observability::otel::is_active() {
            return crate::observability::otel::span_is_sampled(&self.span);
        }
        true
    }

    /// Record the high-level operation name (e.g. the `/query` operation tag).
    #[inline]
    pub fn record_operation(&self, operation: &str) {
        #[cfg(feature = "observability")]
        self.span
            .record(crate::observability::ATTR_OPERATION, operation);
        #[cfg(not(feature = "observability"))]
        let _ = operation;
    }

    /// Record the number of entities/rows the operation returned.
    #[inline]
    pub fn record_result_count(&self, count: usize) {
        #[cfg(feature = "observability")]
        self.span.record(
            crate::observability::ATTR_RESULT_COUNT,
            i64::try_from(count).unwrap_or(i64::MAX),
        );
        #[cfg(not(feature = "observability"))]
        let _ = count;
    }

    /// Record whether the operation was temporally scoped (`as_of`) vs current.
    #[inline]
    pub fn record_temporal_scope(&self, as_of: bool) {
        #[cfg(feature = "observability")]
        self.span.record(
            crate::observability::ATTR_TEMPORAL_SCOPE,
            if as_of { "as_of" } else { "current" },
        );
        #[cfg(not(feature = "observability"))]
        let _ = as_of;
    }

    /// Record a structured error code (Issue #3234) and mark the span errored.
    #[inline]
    pub fn record_error(&self, code: &str) {
        #[cfg(feature = "observability")]
        {
            self.span
                .record(crate::observability::ATTR_ERROR_CODE, code);
            self.span.record("otel.status_code", "ERROR");
        }
        #[cfg(not(feature = "observability"))]
        let _ = code;
    }

    /// Attach sanitized statement text — only when statement capture is
    /// explicitly enabled. Property *values* are never included here; the
    /// caller passes the statement string verbatim and is responsible for not
    /// interpolating values into it (AQL/Cypher use `$param` bindings).
    #[cfg(feature = "otel")]
    #[inline]
    pub fn record_statement(&self, statement: &str) {
        #[cfg(feature = "observability")]
        self.span
            .record(crate::observability::ATTR_DB_QUERY_TEXT, statement);
    }
}

#[cfg(feature = "otel")]
fn header_value(parts: &Parts, name: &str) -> Option<String> {
    parts
        .headers
        .get(name)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned)
}

/// Read the incoming W3C `traceparent`, treating a duplicated header as absent.
///
/// Per the [W3C Trace Context] spec, if more than one `traceparent` header is
/// present the trace context is ambiguous and MUST be handled as if no parent
/// were supplied — the request then starts a fresh root trace rather than
/// nesting under an arbitrarily-chosen (and possibly forged) parent
/// (Issue #3376, LOW5).
///
/// [W3C Trace Context]: https://www.w3.org/TR/trace-context/#traceparent-header
#[cfg(feature = "otel")]
fn traceparent_value(parts: &Parts) -> Option<String> {
    let mut values = parts.headers.get_all("traceparent").iter();
    let first = values.next()?;
    if values.next().is_some() {
        // Multiple `traceparent` values → no valid parent.
        return None;
    }
    first.to_str().ok().map(str::to_owned)
}

impl FromRequestParts<autumn_web::prelude::AppState> for HttpTrace {
    type Rejection = Infallible;

    async fn from_request_parts(
        _parts: &mut Parts,
        _state: &autumn_web::prelude::AppState,
    ) -> Result<Self, Self::Rejection> {
        #[cfg(feature = "observability")]
        let this = {
            let method = _parts.method.as_str();
            let path = _parts.uri.path();
            let span = crate::observability::http_request_span(method, path);

            // Honor an incoming W3C trace context so DB spans nest inside the
            // caller's trace. Only meaningful (and only costed) when the OTel
            // exporter is actually active.
            #[cfg(feature = "otel")]
            if crate::observability::otel::is_active() {
                let traceparent = traceparent_value(_parts);
                let tracestate = header_value(_parts, "tracestate");
                crate::observability::otel::attach_parent(
                    &span,
                    traceparent.as_deref(),
                    tracestate.as_deref(),
                );
            }

            Self { span }
        };
        #[cfg(not(feature = "observability"))]
        let this = Self {};
        Ok(this)
    }
}

/// The active trace id for correlation, or `None` when tracing is inactive or
/// the `otel` feature is disabled.
#[cfg(feature = "otel")]
#[must_use]
pub fn active_trace_id() -> Option<String> {
    crate::observability::otel::current_trace_id()
}

/// The active trace id — always `None` without the `otel` feature.
#[cfg(not(feature = "otel"))]
#[must_use]
pub fn active_trace_id() -> Option<String> {
    None
}
