//! Prometheus text-format `/metrics` exposition endpoint (Issue #3624).
//!
//! The autumn server surfaces AletheiaDB's internal
//! [`MetricsRecorder`](aletheiadb::observability::MetricsRecorder) — concretely
//! the process-wide [`METRICS`](aletheiadb::observability::METRICS) atomic
//! mirror captured by [`aletheiadb::observability::metrics`] — as a standard
//! Prometheus 0.0.4 text exposition so an operator can scrape it with the usual
//! monitoring stack instead of the JSON `database_stats` tool.
//!
//! # Access class
//!
//! The route is classified [`AccessClass::Metrics`](aletheiadb::auth::AccessClass::Metrics)
//! — the *same* class the `database_stats` MCP tool and the `GET /status`
//! liveness probe use — via the shared [`Authorized<MetricsClass>`] seam, so
//! authentication (uniform 401) and RBAC are enforced identically to every
//! other route, and a refusal renders the #3629 nested `{error:{…}}` envelope.
//!
//! # Info-disclosure invariant (hard requirement)
//!
//! Every label this endpoint emits is a **bounded, process-wide enum** —
//! `category` ([`ErrorCategory`]) and `event` ([`CriticalEvent`]). No
//! per-principal, per-API-key, per-request, or otherwise unbounded/secret value
//! is ever placed in a metric name or label. The exposition is byte-identical
//! regardless of *which* credential scraped it.

use aletheiadb::observability::{
    CriticalEvent, ErrorCategory, METRIC_LABEL_CATEGORY, METRIC_LABEL_EVENT, MetricsSnapshot,
};
use autumn_web::prelude::get;
use axum::http::header::CONTENT_TYPE;
use axum::response::{IntoResponse, Response};
use std::fmt::Write as _;

use crate::security::{Authorized, MetricsClass};

/// The Prometheus text-format content type (exposition format version 0.0.4).
///
/// Deliberately the bare `text/plain; version=0.0.4` the issue's acceptance
/// criteria name (no `charset` suffix), so scrapers keyed on the exact string
/// match it.
pub const PROMETHEUS_CONTENT_TYPE: &str = "text/plain; version=0.0.4";

/// Prometheus counter: errors by bounded category. Sanitized name for
/// [`aletheiadb::observability::METRIC_ERRORS`] (`.` → `_`, `_total` suffix).
const PROM_ERRORS: &str = "aletheiadb_errors_total";

/// Prometheus counter: write conflicts under snapshot isolation. Sanitized
/// [`aletheiadb::observability::METRIC_WRITE_CONFLICTS`].
const PROM_WRITE_CONFLICTS: &str = "aletheiadb_write_conflicts_total";

/// Prometheus counter: critical events (should be zero in healthy systems).
/// Sanitized [`aletheiadb::observability::METRIC_CRITICAL_EVENTS`].
const PROM_CRITICAL_EVENTS: &str = "aletheiadb_critical_events_total";

/// Prometheus counter: committed transactions. Sanitized
/// [`aletheiadb::observability::METRIC_TRANSACTION_COMMITS`].
const PROM_TRANSACTION_COMMITS: &str = "aletheiadb_transaction_commits_total";

/// Prometheus gauge: currently-live changefeed subscriptions (Issue #3678). A
/// process-wide **bounded aggregate** — deliberately unlabeled (no per-principal
/// label) to preserve the info-disclosure invariant; the per-principal breakdown
/// is surfaced via the authenticated `database_stats` JSON instead.
const PROM_CHANGEFEED_ACTIVE: &str = "aletheiadb_changefeed_subscriptions_active";

/// Prometheus counter: per-principal changefeed quota rejections (Issue #3678).
/// Also an unlabeled process-wide aggregate.
const PROM_CHANGEFEED_QUOTA_REJECTIONS: &str = "aletheiadb_changefeed_quota_rejections_total";

/// A rendered Prometheus text exposition, wrapped so it serializes with the
/// [`PROMETHEUS_CONTENT_TYPE`] content type rather than autumn's default JSON.
#[derive(Debug, Clone)]
pub struct PrometheusExposition(pub String);

impl IntoResponse for PrometheusExposition {
    fn into_response(self) -> Response {
        ([(CONTENT_TYPE, PROMETHEUS_CONTENT_TYPE)], self.0).into_response()
    }
}

/// The `category`-labelled error counters, paired with the snapshot field each
/// reads. The label value is the bounded [`ErrorCategory::as_str`] — never a
/// free-form or caller-supplied string.
fn error_families(snapshot: &MetricsSnapshot) -> [(ErrorCategory, u64); 7] {
    [
        (ErrorCategory::Storage, snapshot.error_storage_total),
        (ErrorCategory::Temporal, snapshot.error_temporal_total),
        (ErrorCategory::Query, snapshot.error_query_total),
        (ErrorCategory::Transaction, snapshot.error_transaction_total),
        (ErrorCategory::Vector, snapshot.error_vector_total),
        (ErrorCategory::Io, snapshot.error_io_total),
        (ErrorCategory::Other, snapshot.error_other_total),
    ]
}

/// The `event`-labelled critical-event counters, paired with the snapshot field
/// each reads. The label value is the bounded [`CriticalEvent::as_str`].
fn critical_families(snapshot: &MetricsSnapshot) -> [(CriticalEvent, u64); 3] {
    [
        (CriticalEvent::LockPoison, snapshot.lock_poison_count),
        (
            CriticalEvent::TimestampViolation,
            snapshot.timestamp_violations,
        ),
        (
            CriticalEvent::WalChecksumFailure,
            snapshot.wal_checksum_failures,
        ),
    ]
}

/// Render a [`MetricsSnapshot`] as a Prometheus 0.0.4 text exposition.
///
/// Pure and deterministic: the same snapshot always renders the same bytes, and
/// the output carries only bounded-cardinality labels (`category`, `event`). No
/// I/O, no allocation beyond the returned `String`.
#[must_use]
pub fn render_prometheus(snapshot: &MetricsSnapshot) -> String {
    let mut out = String::with_capacity(1024);

    // ── errors{category=…} ───────────────────────────────────────────────────
    let _ = writeln!(
        out,
        "# HELP {PROM_ERRORS} Total errors by bounded category."
    );
    let _ = writeln!(out, "# TYPE {PROM_ERRORS} counter");
    for (category, count) in error_families(snapshot) {
        let _ = writeln!(
            out,
            "{PROM_ERRORS}{{{METRIC_LABEL_CATEGORY}=\"{}\"}} {count}",
            category.as_str()
        );
    }

    // ── write_conflicts ──────────────────────────────────────────────────────
    let _ = writeln!(
        out,
        "# HELP {PROM_WRITE_CONFLICTS} Write conflicts observed under snapshot isolation."
    );
    let _ = writeln!(out, "# TYPE {PROM_WRITE_CONFLICTS} counter");
    let _ = writeln!(out, "{PROM_WRITE_CONFLICTS} {}", snapshot.write_conflicts);

    // ── critical_events{event=…} ─────────────────────────────────────────────
    let _ = writeln!(
        out,
        "# HELP {PROM_CRITICAL_EVENTS} Critical events that should be zero in healthy systems."
    );
    let _ = writeln!(out, "# TYPE {PROM_CRITICAL_EVENTS} counter");
    for (event, count) in critical_families(snapshot) {
        let _ = writeln!(
            out,
            "{PROM_CRITICAL_EVENTS}{{{METRIC_LABEL_EVENT}=\"{}\"}} {count}",
            event.as_str()
        );
    }

    // ── transaction_commits ──────────────────────────────────────────────────
    let _ = writeln!(
        out,
        "# HELP {PROM_TRANSACTION_COMMITS} Total committed transactions."
    );
    let _ = writeln!(out, "# TYPE {PROM_TRANSACTION_COMMITS} counter");
    let _ = writeln!(
        out,
        "{PROM_TRANSACTION_COMMITS} {}",
        snapshot.transaction_commits_total
    );

    // ── changefeed_subscriptions_active (gauge) ──────────────────────────────
    let _ = writeln!(
        out,
        "# HELP {PROM_CHANGEFEED_ACTIVE} Currently-live changefeed subscriptions (process-wide total)."
    );
    let _ = writeln!(out, "# TYPE {PROM_CHANGEFEED_ACTIVE} gauge");
    let _ = writeln!(
        out,
        "{PROM_CHANGEFEED_ACTIVE} {}",
        snapshot.changefeed_subscriptions_active
    );

    // ── changefeed_quota_rejections_total (counter) ──────────────────────────
    let _ = writeln!(
        out,
        "# HELP {PROM_CHANGEFEED_QUOTA_REJECTIONS} Changefeed subscribes rejected by the per-caller fairness quota."
    );
    let _ = writeln!(out, "# TYPE {PROM_CHANGEFEED_QUOTA_REJECTIONS} counter");
    let _ = writeln!(
        out,
        "{PROM_CHANGEFEED_QUOTA_REJECTIONS} {}",
        snapshot.changefeed_quota_rejections_total
    );

    out
}

/// `GET /metrics` — Prometheus text-format exposition of the internal
/// `MetricsRecorder` counters.
///
/// Classified [`AccessClass::Metrics`] through [`Authorized<MetricsClass>`]: in
/// [`Required`](aletheiadb::auth::AuthMode::Required) mode a missing/invalid
/// credential is a uniform 401 (nested #3629 envelope); every role that permits
/// the metrics class (admin/writer/reader/metrics) is admitted. The route is
/// **HTTP + OpenAPI only** — it is intentionally NOT an MCP tool (no
/// `api_doc(mcp)`), matching `GET /status`.
///
/// Infallible on the success path (reading the atomic mirror cannot fail), so it
/// returns the response value directly; the auth/RBAC error path is carried by
/// the [`Authorized`] extractor's rejection.
#[get("/metrics")]
#[api_doc(
    description = "Prometheus text-format (version 0.0.4) exposition of internal metrics counters"
)]
pub async fn metrics(_auth: Authorized<MetricsClass>) -> PrometheusExposition {
    let snapshot = aletheiadb::observability::metrics();
    PrometheusExposition(render_prometheus(&snapshot))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A snapshot with a distinct value in every field, so the renderer's
    /// field→family wiring is pinned (a swapped field would flip a value).
    fn distinct_snapshot() -> MetricsSnapshot {
        MetricsSnapshot {
            lock_poison_count: 1,
            timestamp_violations: 2,
            wal_checksum_failures: 3,
            write_conflicts: 4,
            error_storage_total: 5,
            error_temporal_total: 6,
            error_query_total: 7,
            error_transaction_total: 8,
            error_vector_total: 9,
            error_io_total: 10,
            error_other_total: 11,
            transaction_commits_total: 12,
            changefeed_subscriptions_active: 13,
            changefeed_quota_rejections_total: 14,
        }
    }

    #[test]
    fn content_type_is_prometheus_004() {
        assert_eq!(PROMETHEUS_CONTENT_TYPE, "text/plain; version=0.0.4");
    }

    #[test]
    fn render_has_help_type_and_samples_for_every_family() {
        let body = render_prometheus(&distinct_snapshot());

        // Each family carries HELP + TYPE + at least one sample.
        for name in [
            PROM_ERRORS,
            PROM_WRITE_CONFLICTS,
            PROM_CRITICAL_EVENTS,
            PROM_TRANSACTION_COMMITS,
            PROM_CHANGEFEED_ACTIVE,
            PROM_CHANGEFEED_QUOTA_REJECTIONS,
        ] {
            assert!(
                body.contains(&format!("# HELP {name} ")),
                "missing HELP for {name}"
            );
        }
        // Counters carry `# TYPE … counter`; the changefeed gauge carries gauge.
        for name in [
            PROM_ERRORS,
            PROM_WRITE_CONFLICTS,
            PROM_CRITICAL_EVENTS,
            PROM_TRANSACTION_COMMITS,
            PROM_CHANGEFEED_QUOTA_REJECTIONS,
        ] {
            assert!(
                body.contains(&format!("# TYPE {name} counter")),
                "missing TYPE for {name}"
            );
        }
        assert!(
            body.contains(&format!("# TYPE {PROM_CHANGEFEED_ACTIVE} gauge")),
            "changefeed active must be a gauge"
        );
    }

    #[test]
    fn render_wires_each_field_to_the_right_labelled_sample() {
        let body = render_prometheus(&distinct_snapshot());

        // errors{category=…}
        assert!(body.contains(&format!("{PROM_ERRORS}{{category=\"storage\"}} 5")));
        assert!(body.contains(&format!("{PROM_ERRORS}{{category=\"temporal\"}} 6")));
        assert!(body.contains(&format!("{PROM_ERRORS}{{category=\"query\"}} 7")));
        assert!(body.contains(&format!("{PROM_ERRORS}{{category=\"transaction\"}} 8")));
        assert!(body.contains(&format!("{PROM_ERRORS}{{category=\"vector\"}} 9")));
        assert!(body.contains(&format!("{PROM_ERRORS}{{category=\"io\"}} 10")));
        assert!(body.contains(&format!("{PROM_ERRORS}{{category=\"other\"}} 11")));

        // critical_events{event=…}
        assert!(body.contains(&format!(
            "{PROM_CRITICAL_EVENTS}{{event=\"lock_poison\"}} 1"
        )));
        assert!(body.contains(&format!(
            "{PROM_CRITICAL_EVENTS}{{event=\"timestamp_violation\"}} 2"
        )));
        assert!(body.contains(&format!(
            "{PROM_CRITICAL_EVENTS}{{event=\"wal_checksum_failure\"}} 3"
        )));

        // unlabelled scalars
        assert!(body.contains(&format!("{PROM_WRITE_CONFLICTS} 4")));
        assert!(body.contains(&format!("{PROM_TRANSACTION_COMMITS} 12")));
        assert!(body.contains(&format!("{PROM_CHANGEFEED_ACTIVE} 13")));
        assert!(body.contains(&format!("{PROM_CHANGEFEED_QUOTA_REJECTIONS} 14")));
    }

    #[test]
    fn no_metric_name_contains_a_dot() {
        let body = render_prometheus(&distinct_snapshot());
        for line in body.lines() {
            if line.starts_with('#') || line.is_empty() {
                continue;
            }
            let name = line
                .split_once('{')
                .map_or(line.split(' ').next().unwrap_or(""), |(n, _)| n);
            assert!(
                !name.contains('.'),
                "sanitized name must have no dot: {name}"
            );
        }
    }

    #[test]
    fn labels_are_bounded_enums_only_no_secrets() {
        use std::collections::BTreeSet;

        let body = render_prometheus(&distinct_snapshot());

        // Denylist (kept): no secret-y substring may appear.
        for forbidden in ["principal", "api_key", "token", "key_prefix", "role="] {
            assert!(
                !body.contains(forbidden),
                "exposition must not emit a {forbidden:?} label"
            );
        }

        // Allowlist (stronger): parse the label KEYS off every sample line and
        // assert the set is a subset of the two bounded process-wide enums, and
        // that the total series count is exactly 14 (7 error categories + 3
        // critical events + 4 unlabeled scalars — write_conflicts,
        // transaction_commits, and the two Issue #3678 changefeed aggregates). The
        // changefeed aggregates are deliberately UNLABELED (no `principal` label),
        // so the bounded-key allowlist is unchanged. A future edit that wires in an
        // extra label (e.g. the unused `METRIC_LABEL_DURABILITY_MODE` /
        // `METRIC_LABEL_STATUS`) fails here.
        let allowed: BTreeSet<&str> = ["category", "event"].into_iter().collect();
        let mut keys: BTreeSet<String> = BTreeSet::new();
        let mut series = 0usize;
        for line in body.lines() {
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            series += 1;
            if let Some(open) = line.find('{') {
                let close = line.find('}').expect("labelled sample closes its brace");
                for pair in line[open + 1..close].split(',') {
                    if pair.trim().is_empty() {
                        continue;
                    }
                    let key = pair.split('=').next().expect("label pair has a key").trim();
                    keys.insert(key.to_string());
                }
            }
        }
        assert!(
            keys.iter().all(|k| allowed.contains(k.as_str())),
            "emitted label keys {keys:?} must be a subset of {allowed:?}"
        );
        assert_eq!(
            series, 14,
            "render must emit exactly 14 time series (7 error categories + 3 critical events + 4 scalars)"
        );
    }
}
