//! `GET /metrics` Prometheus exposition endpoint (Issue #3624).
//!
//! The autumn server exposes the internal `MetricsRecorder`'s counters as a
//! standard Prometheus text-format (`text/plain; version=0.0.4`) scrape target.
//! The route is classified [`AccessClass::Metrics`] (the same class the
//! `database_stats` tool / `GET /status` probe use), authenticated + RBAC-gated
//! through the shared [`Authorized`](aletheia_server::security::Authorized) seam,
//! and emits the #3629 nested error envelope on an unauthenticated request.
//!
//! Info-disclosure invariant (hard requirement): the exposition never carries a
//! per-principal or per-API-key label — every emitted label is a bounded,
//! process-wide enum (`category`, `event`). These tests assert that no
//! credential material leaks into the body.

use std::collections::BTreeSet;
use std::sync::Arc;

use aletheia_server::build_server_client;
use aletheiadb::auth::{AuthMode, AuthStore, Role};
use aletheiadb::{AletheiaDB, PropertyMapBuilder};
use serde_json::Value;

const ADMIN_TOKEN: &str = "admin-token-abcdefghijklmnop";
const METRICS_TOKEN: &str = "metrics-probe-token-abcdef";

/// The exact content type the endpoint must advertise (Prometheus text format
/// version 0.0.4, per the issue's acceptance criteria).
const PROM_CONTENT_TYPE: &str = "text/plain; version=0.0.4";

/// The complete, bounded set of label keys the exposition is allowed to emit —
/// both are process-wide enums, neither is per-principal/per-request.
const ALLOWED_LABEL_KEYS: &[&str] = &["category", "event"];

/// The exact number of time series the exposition emits: 7 error categories +
/// 3 critical events + 2 unlabeled scalars (write_conflicts, transaction_commits).
/// Pinning this count makes a future label/series addition fail loudly rather
/// than silently widening the exposed surface.
const EXPECTED_SERIES_COUNT: usize = 12;

/// Parse every sample line's label KEYS (the identifiers left of each `=`) and
/// count the total number of sample (time-series) lines. Comment/blank lines are
/// ignored. This is the allowlist primitive the info-disclosure guard uses.
fn label_keys_and_series_count(body: &str) -> (BTreeSet<String>, usize) {
    let mut keys = BTreeSet::new();
    let mut series = 0usize;
    for line in body.lines() {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        series += 1;
        let Some(open) = line.find('{') else {
            continue; // an unlabeled scalar sample
        };
        let close = line
            .find('}')
            .expect("labelled sample must close its brace");
        for pair in line[open + 1..close].split(',') {
            if pair.trim().is_empty() {
                continue;
            }
            let key = pair.split('=').next().expect("label pair has a key").trim();
            keys.insert(key.to_string());
        }
    }
    (keys, series)
}

/// A minimal DB plus an auth store carrying an admin key and a metrics-role key.
fn fixture() -> (Arc<AletheiaDB>, Arc<AuthStore>) {
    let db = Arc::new(AletheiaDB::new().expect("create db"));
    db.create_node(
        "Person",
        PropertyMapBuilder::new().insert("name", "Alice").build(),
    )
    .expect("create node");

    let store = Arc::new(AuthStore::new());
    store
        .insert_bootstrap_key("admin", Role::Admin, ADMIN_TOKEN)
        .expect("insert admin key");
    store
        .insert_bootstrap_key("probe", Role::Metrics, METRICS_TOKEN)
        .expect("insert metrics key");
    (db, store)
}

/// Assert the body is a well-formed Prometheus text exposition: every non-blank,
/// non-comment line is `metric_name[{labels}] value`, HELP/TYPE comments are
/// well-formed, and at least one expected metric family is present.
fn assert_valid_prometheus(body: &str) {
    let mut saw_help = false;
    let mut saw_type = false;
    let mut saw_sample = false;

    for line in body.lines() {
        if line.is_empty() {
            continue;
        }
        if let Some(rest) = line.strip_prefix("# HELP ") {
            saw_help = true;
            assert!(
                rest.split_whitespace().next().is_some(),
                "# HELP line must name a metric: {line:?}"
            );
            continue;
        }
        if let Some(rest) = line.strip_prefix("# TYPE ") {
            saw_type = true;
            let mut parts = rest.split_whitespace();
            let _name = parts.next().expect("# TYPE names a metric");
            let kind = parts.next().expect("# TYPE has a metric kind");
            assert!(
                matches!(
                    kind,
                    "counter" | "gauge" | "histogram" | "summary" | "untyped"
                ),
                "# TYPE kind must be a Prometheus metric type, got {kind:?}"
            );
            continue;
        }
        assert!(
            !line.starts_with('#'),
            "only HELP/TYPE comment lines are allowed, got {line:?}"
        );

        // A sample line: `name{labels} value` or `name value`.
        let (name_and_labels, value) = line
            .rsplit_once(' ')
            .unwrap_or_else(|| panic!("sample line must have a value: {line:?}"));
        // The value must parse as an f64 (integers included).
        value
            .parse::<f64>()
            .unwrap_or_else(|_| panic!("sample value must be numeric: {line:?}"));
        // The metric name (before any `{`) must be a valid Prometheus name.
        let name = name_and_labels
            .split_once('{')
            .map_or(name_and_labels, |(n, _)| n);
        assert!(
            !name.is_empty()
                && name
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b':'),
            "metric name must match [a-zA-Z0-9_:]+, got {name:?}"
        );
        // Prometheus names cannot contain '.' — the internal dotted metric names
        // must be sanitized to underscores.
        assert!(
            !name.contains('.'),
            "metric name must not contain '.': {name:?}"
        );
        saw_sample = true;
    }

    assert!(saw_help, "exposition must carry at least one # HELP line");
    assert!(saw_type, "exposition must carry at least one # TYPE line");
    assert!(saw_sample, "exposition must carry at least one sample line");
    assert!(
        body.contains("aletheiadb_errors_total"),
        "exposition must include the errors counter family"
    );
    assert!(
        body.contains("aletheiadb_transaction_commits_total"),
        "exposition must include the transaction-commits counter family"
    );
}

// ── (1) success: 200 + content-type + valid Prometheus body ──────────────────

#[tokio::test]
async fn metrics_endpoint_returns_prometheus_text() {
    let (db, store) = fixture();
    // Anonymous mode: the synthetic principal is Admin, so the class gate passes
    // and a 200 positively proves the route is served.
    let client = build_server_client(db, store, AuthMode::Anonymous);

    let resp = client.get("/metrics").send().await;
    assert_eq!(resp.status.as_u16(), 200, "GET /metrics must be 200");
    assert_eq!(
        resp.header("content-type"),
        Some(PROM_CONTENT_TYPE),
        "content type must be the Prometheus text format"
    );
    assert_valid_prometheus(&resp.text());
}

// ── (2) unauth in required mode → 401 with the #3629 nested error envelope ────

#[tokio::test]
async fn metrics_endpoint_requires_credential_in_required_mode() {
    let (db, store) = fixture();
    let client = build_server_client(db, store, AuthMode::Required);

    let resp = client.get("/metrics").send().await;
    assert_eq!(
        resp.status.as_u16(),
        401,
        "unauthenticated /metrics must be 401 in required mode"
    );

    let body: Value = resp.json();
    // Nested envelope (#3629): {"error":{"code","message","retriable",...}} —
    // the legacy top-level `success`/`error` string is gone.
    assert!(
        body.get("success").is_none(),
        "nested envelope must not carry a top-level success field"
    );
    assert_eq!(
        body["error"]["code"], "UNAUTHENTICATED",
        "401 code must be UNAUTHENTICATED"
    );
    assert_eq!(body["error"]["retriable"], false);
    assert!(
        body["error"]["message"].is_string(),
        "error.message must be present"
    );
}

// ── (3) RBAC: the metrics-only role is admitted (Metrics class) ──────────────

#[tokio::test]
async fn metrics_endpoint_admits_metrics_role() {
    let (db, store) = fixture();
    let client = build_server_client(db, store, AuthMode::Required);

    let resp = client
        .get("/metrics")
        .header("authorization", &format!("Bearer {METRICS_TOKEN}"))
        .send()
        .await;
    assert_eq!(
        resp.status.as_u16(),
        200,
        "a metrics-role credential must be admitted"
    );
    assert_eq!(resp.header("content-type"), Some(PROM_CONTENT_TYPE));
    assert_valid_prometheus(&resp.text());
}

// ── (4) info-disclosure: no credential material leaks into the body ──────────

#[tokio::test]
async fn metrics_endpoint_does_not_leak_credentials() {
    let (db, store) = fixture();
    let client = build_server_client(db, store, AuthMode::Required);

    let resp = client
        .get("/metrics")
        .header("authorization", &format!("Bearer {ADMIN_TOKEN}"))
        .send()
        .await;
    assert_eq!(resp.status.as_u16(), 200);
    let body = resp.text();

    // Denylist (kept): no token, principal id, key prefix, or role name may
    // appear anywhere in the exposition.
    assert!(!body.contains(ADMIN_TOKEN), "admin token must not leak");
    assert!(!body.contains(METRICS_TOKEN), "metrics token must not leak");
    for forbidden in ["principal", "api_key", "apikey", "token", "key_prefix"] {
        assert!(
            !body.contains(forbidden),
            "exposition must not contain a {forbidden:?} label/name"
        );
    }

    // Allowlist (stronger): the set of emitted label KEYS must be a subset of the
    // two bounded process-wide enums, and the total time-series count must equal
    // the pinned constant. This fails loudly if a future edit wires in an extra
    // label (e.g. the unused durability_mode/status constants) or a new series.
    let allowed: BTreeSet<String> = ALLOWED_LABEL_KEYS.iter().map(|s| s.to_string()).collect();
    let (keys, series) = label_keys_and_series_count(&body);
    assert!(
        keys.is_subset(&allowed),
        "emitted label keys {keys:?} must be a subset of the bounded set {allowed:?}"
    );
    assert_eq!(
        series, EXPECTED_SERIES_COUNT,
        "exposition must emit exactly {EXPECTED_SERIES_COUNT} time series (7 error categories + 3 critical events + 2 scalars)"
    );
}
