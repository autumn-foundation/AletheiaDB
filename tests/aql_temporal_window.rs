//! End-to-end tests for AQL temporal aggregation windows (Issue #3363).
//!
//! These exercise the full pipeline (`execute_aql` -> parser -> converter ->
//! planner -> executor) and assert hand-computed per-window values, the
//! documented boundary/sampling rules, bi-temporal `AS OF SYSTEM_TIME`
//! correctness, empty-window handling, and structured errors for malformed
//! specs. The pure interval math has its own unit tests in
//! `src/query/temporal_window.rs`; these cover semantics that require real
//! history.

use aletheiadb::AletheiaDB;
use aletheiadb::PropertyMapBuilder;
use aletheiadb::api::transaction::WriteRequestOptions;
use aletheiadb::core::NodeId;
use aletheiadb::core::error::{Error, QueryError};
use aletheiadb::core::property::PropertyValue;
use aletheiadb::core::temporal::Timestamp;
use aletheiadb::query::executor::QueryRow;
use aletheiadb::query::temporal_window::parse_boundary_micros;

/// Microseconds since epoch for an RFC 3339 instant (test convenience).
fn micros(rfc: &str) -> i64 {
    parse_boundary_micros(rfc).expect("valid rfc3339")
}

/// A `Timestamp` at an RFC 3339 instant.
fn ts(rfc: &str) -> Timestamp {
    Timestamp::from(micros(rfc))
}

fn rows(db: &AletheiaDB, aql: &str) -> Vec<QueryRow> {
    db.execute_aql(aql)
        .expect("query executes")
        .collect_all()
        .expect("collect rows")
}

/// Look up a projected column value by name in a row.
fn col<'a>(row: &'a QueryRow, name: &str) -> Option<&'a PropertyValue> {
    row.columns
        .as_ref()
        .and_then(|cols| cols.iter().find(|(k, _)| k == name).map(|(_, v)| v))
}

fn col_f64(row: &QueryRow, name: &str) -> Option<f64> {
    match col(row, name) {
        Some(PropertyValue::Float(f)) => Some(*f),
        Some(PropertyValue::Int(i)) => Some(*i as f64),
        _ => None,
    }
}

fn col_i64(row: &QueryRow, name: &str) -> Option<i64> {
    match col(row, name) {
        Some(PropertyValue::Int(i)) => Some(*i),
        _ => None,
    }
}

fn col_str(row: &QueryRow, name: &str) -> Option<String> {
    match col(row, name) {
        Some(PropertyValue::String(s)) => Some(s.to_string()),
        _ => None,
    }
}

fn is_null(row: &QueryRow, name: &str) -> bool {
    matches!(col(row, name), Some(PropertyValue::Null))
}

/// Create a `Product` node with an initial price valid from `valid_from`.
fn create_product(db: &AletheiaDB, price: i64, valid_from: Timestamp) -> NodeId {
    db.create_node_with_options(
        "Product",
        PropertyMapBuilder::new().insert("price", price).build(),
        WriteRequestOptions::new().with_valid_from(valid_from),
    )
    .expect("create product")
}

/// Update a `Product`'s price valid from `valid_from`.
fn update_price(db: &AletheiaDB, id: NodeId, price: i64, valid_from: Timestamp) {
    db.update_node_with_options(
        id,
        PropertyMapBuilder::new().insert("price", price).build(),
        WriteRequestOptions::new().with_valid_from(valid_from),
    )
    .expect("update price");
}

// ---------------------------------------------------------------------------
// AC 3 + 4: hand-computed monthly value fixture, boundary & sampling rules.
// ---------------------------------------------------------------------------

#[test]
fn monthly_avg_hand_computed_with_boundary_and_clamp() {
    let db = AletheiaDB::new().expect("db");
    let id = create_product(&db, 100, ts("2024-01-01T00:00:00Z"));
    // Mid-February change (does not affect the Feb window, whose start is Feb 1).
    update_price(&db, id, 200, ts("2024-02-15T00:00:00Z"));
    // Change exactly at a window boundary -> counted in the April window.
    update_price(&db, id, 300, ts("2024-04-01T00:00:00Z"));

    let result = rows(
        &db,
        "MATCH (p:Product) \
         WINDOW 1 month OVER VALID_TIME FROM '2024-01-01T00:00:00Z' TO '2024-05-01T00:00:00Z' \
         RETURN AVG(p.price) AS avg_price, MIN(p.price) AS lo, MAX(p.price) AS hi, \
                COUNT(p.price) AS n, COUNT(*) AS present",
    );

    // Four windows: Jan, Feb, Mar, Apr.
    assert_eq!(result.len(), 4, "one row per monthly window");

    // Self-describing window bounds (RFC 3339).
    assert_eq!(
        col_str(&result[0], "window_start").as_deref(),
        Some("2024-01-01T00:00:00.000000Z")
    );
    assert_eq!(
        col_str(&result[0], "window_end").as_deref(),
        Some("2024-02-01T00:00:00.000000Z")
    );
    // Last window's end is clamped to the range end (May 1).
    assert_eq!(
        col_str(&result[3], "window_end").as_deref(),
        Some("2024-05-01T00:00:00.000000Z")
    );

    // Value-at-window-start sampling: [Jan=100, Feb=100, Mar=200, Apr=300].
    let expected = [100.0, 100.0, 200.0, 300.0];
    for (i, want) in expected.iter().enumerate() {
        assert_eq!(
            col_f64(&result[i], "avg_price"),
            Some(*want),
            "avg window {i}"
        );
        assert_eq!(col_f64(&result[i], "lo"), Some(*want), "min window {i}");
        assert_eq!(col_f64(&result[i], "hi"), Some(*want), "max window {i}");
        assert_eq!(col_i64(&result[i], "n"), Some(1), "count(price) window {i}");
        assert_eq!(
            col_i64(&result[i], "present"),
            Some(1),
            "count(*) window {i}"
        );
    }
}

// ---------------------------------------------------------------------------
// AC 4: empty windows emit an explicit null/zero row, never dropped.
// ---------------------------------------------------------------------------

#[test]
fn empty_window_emits_null_and_zero_row() {
    let db = AletheiaDB::new().expect("db");
    // Product only exists from mid-February onward.
    let _id = create_product(&db, 50, ts("2024-02-10T00:00:00Z"));

    let result = rows(
        &db,
        "MATCH (p:Product) \
         WINDOW 1 month OVER VALID_TIME FROM '2024-01-01T00:00:00Z' TO '2024-04-01T00:00:00Z' \
         RETURN AVG(p.price) AS avg_price, COUNT(*) AS present, CHANGES(p) AS changes",
    );

    assert_eq!(result.len(), 3, "three windows, none dropped");
    // January: entity does not exist at all -> null value aggregate, zero counts.
    assert!(is_null(&result[0], "avg_price"), "empty window avg is null");
    assert_eq!(col_i64(&result[0], "present"), Some(0));
    assert_eq!(col_i64(&result[0], "changes"), Some(0));
    // February: the entity is created on Feb-10, i.e. AFTER the window start
    // (Feb-01). Under the "value at window start" sampling rule it is not yet
    // present at the sample instant, so the value aggregate is null and
    // COUNT(*) is 0 -- but a version *starts* inside the window, so CHANGES is 1.
    assert!(
        is_null(&result[1], "avg_price"),
        "not present at Feb-01 window start"
    );
    assert_eq!(col_i64(&result[1], "present"), Some(0));
    assert_eq!(
        col_i64(&result[1], "changes"),
        Some(1),
        "create starts in February"
    );
    // March: the entity is valid at the March-01 window start (open interval)
    // -> sampled through the final window.
    assert_eq!(col_f64(&result[2], "avg_price"), Some(50.0));
    assert_eq!(col_i64(&result[2], "present"), Some(1));
    assert_eq!(
        col_i64(&result[2], "changes"),
        Some(0),
        "no new version in March"
    );
}

// ---------------------------------------------------------------------------
// AC 2: CHANGES volatility (weekly), hand-computed.
// ---------------------------------------------------------------------------

#[test]
fn weekly_changes_volatility_hand_computed() {
    let db = AletheiaDB::new().expect("db");
    // Create in week 1, two changes in week 2, none in week 3, one in week 4.
    let id = create_product(&db, 10, ts("2024-01-01T00:00:00Z")); // week 1
    update_price(&db, id, 11, ts("2024-01-08T12:00:00Z")); // week 2
    update_price(&db, id, 12, ts("2024-01-10T00:00:00Z")); // week 2
    update_price(&db, id, 12, ts("2024-01-22T00:00:00Z")); // week 4 (value unchanged)

    let result = rows(
        &db,
        "MATCH (p:Product) \
         WINDOW 1 week OVER VALID_TIME FROM '2024-01-01T00:00:00Z' TO '2024-01-29T00:00:00Z' \
         RETURN CHANGES(p) AS versions, CHANGES(p.price) AS price_changes",
    );

    assert_eq!(result.len(), 4, "four weekly windows");
    // Entity-version starts per week: [1, 2, 0, 1].
    assert_eq!(col_i64(&result[0], "versions"), Some(1));
    assert_eq!(col_i64(&result[1], "versions"), Some(2));
    assert_eq!(col_i64(&result[2], "versions"), Some(0));
    assert_eq!(col_i64(&result[3], "versions"), Some(1));
    // Genuine price changes per week: week1 create (absent->10) counts; week2
    // both are genuine changes (10->11->12); week4 re-set to same 12 is NOT a
    // change.
    assert_eq!(col_i64(&result[0], "price_changes"), Some(1));
    assert_eq!(col_i64(&result[1], "price_changes"), Some(2));
    assert_eq!(col_i64(&result[2], "price_changes"), Some(0));
    assert_eq!(
        col_i64(&result[3], "price_changes"),
        Some(0),
        "same value is not a change"
    );
}

// ---------------------------------------------------------------------------
// AC 5: transaction-time dimension (AS OF SYSTEM_TIME) is respected.
// ---------------------------------------------------------------------------

#[test]
fn as_of_system_time_respects_correction() {
    use std::thread::sleep;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};
    let now_micros = || -> i64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("after epoch")
            .as_micros() as i64
    };

    let db = AletheiaDB::new().expect("db");
    // v1: price 100, valid from Jan 1 (open-ended in valid time).
    let id = create_product(&db, 100, ts("2024-01-01T00:00:00Z"));

    sleep(Duration::from_millis(20));
    let tx_between = now_micros();
    sleep(Duration::from_millis(20));

    // v2 (recorded later): price 200 valid from March 1.
    update_price(&db, id, 200, ts("2024-03-01T00:00:00Z"));

    let q = |as_of: Option<i64>| -> Vec<QueryRow> {
        let clause = match as_of {
            Some(t) => format!(" AS OF SYSTEM_TIME {t}"),
            None => String::new(),
        };
        rows(
            &db,
            &format!(
                "MATCH (p:Product) \
                 WINDOW 1 month OVER VALID_TIME FROM '2024-01-01T00:00:00Z' \
                 TO '2024-05-01T00:00:00Z'{clause} RETURN AVG(p.price) AS avg_price"
            ),
        )
    };

    // As of NOW: the March-onward correction is visible; March window samples 200.
    let now_rows = q(None);
    assert_eq!(now_rows.len(), 4);
    assert_eq!(
        col_f64(&now_rows[2], "avg_price"),
        Some(200.0),
        "March, now"
    );

    // As of BEFORE the correction: only v1 believed; March still samples 100.
    let past_rows = q(Some(tx_between));
    assert_eq!(
        col_f64(&past_rows[2], "avg_price"),
        Some(100.0),
        "March, as-of past"
    );
    // January is unaffected either way.
    assert_eq!(col_f64(&now_rows[0], "avg_price"), Some(100.0));
    assert_eq!(col_f64(&past_rows[0], "avg_price"), Some(100.0));
}

// ---------------------------------------------------------------------------
// Multi-entity aggregation across a matched set.
// ---------------------------------------------------------------------------

#[test]
fn avg_across_matched_set() {
    let db = AletheiaDB::new().expect("db");
    create_product(&db, 100, ts("2024-01-01T00:00:00Z"));
    create_product(&db, 300, ts("2024-01-01T00:00:00Z"));

    let result = rows(
        &db,
        "MATCH (p:Product) \
         WINDOW 1 month OVER VALID_TIME FROM '2024-01-01T00:00:00Z' TO '2024-02-01T00:00:00Z' \
         RETURN AVG(p.price) AS avg_price, SUM(p.price) AS total, COUNT(*) AS n",
    );
    assert_eq!(result.len(), 1);
    assert_eq!(col_f64(&result[0], "avg_price"), Some(200.0));
    assert_eq!(col_i64(&result[0], "total"), Some(400));
    assert_eq!(col_i64(&result[0], "n"), Some(2));
}

// ---------------------------------------------------------------------------
// AQL timestamp-encoding parity: RFC 3339 boundaries == microsecond boundaries.
// ---------------------------------------------------------------------------

#[test]
fn rfc3339_and_micros_boundaries_agree() {
    let db = AletheiaDB::new().expect("db");
    let id = create_product(&db, 100, ts("2024-01-01T00:00:00Z"));
    update_price(&db, id, 200, ts("2024-02-01T00:00:00Z"));

    let start = micros("2024-01-01T00:00:00Z");
    let end = micros("2024-03-01T00:00:00Z");

    let with_rfc = rows(
        &db,
        "MATCH (p:Product) \
         WINDOW 1 month OVER VALID_TIME FROM '2024-01-01T00:00:00Z' TO '2024-03-01T00:00:00Z' \
         RETURN AVG(p.price) AS avg_price",
    );
    let with_micros = rows(
        &db,
        &format!(
            "MATCH (p:Product) \
             WINDOW 1 month OVER VALID_TIME FROM {start} TO {end} RETURN AVG(p.price) AS avg_price"
        ),
    );

    assert_eq!(with_rfc.len(), with_micros.len());
    for (a, b) in with_rfc.iter().zip(with_micros.iter()) {
        assert_eq!(col_f64(a, "avg_price"), col_f64(b, "avg_price"));
        assert_eq!(col_str(a, "window_start"), col_str(b, "window_start"));
    }
    assert_eq!(col_f64(&with_rfc[0], "avg_price"), Some(100.0));
    assert_eq!(col_f64(&with_rfc[1], "avg_price"), Some(200.0));
}

// ---------------------------------------------------------------------------
// AC 6: malformed specs return structured errors (never panic / empty success).
// ---------------------------------------------------------------------------

#[test]
fn malformed_specs_return_structured_errors() {
    let db = AletheiaDB::new().expect("db");
    let _ = create_product(&db, 100, ts("2024-01-01T00:00:00Z"));

    // Backwards range.
    assert!(
        db.execute_aql(
            "MATCH (p:Product) WINDOW 1 day OVER VALID_TIME \
             FROM '2024-05-01T00:00:00Z' TO '2024-01-01T00:00:00Z' RETURN AVG(p.price)"
        )
        .is_err()
    );

    // Zero window size.
    assert!(
        db.execute_aql(
            "MATCH (p:Product) WINDOW 0 day OVER VALID_TIME \
             FROM '2024-01-01T00:00:00Z' TO '2024-02-01T00:00:00Z' RETURN AVG(p.price)"
        )
        .is_err()
    );

    // Unknown unit.
    assert!(
        db.execute_aql(
            "MATCH (p:Product) WINDOW 1 fortnight OVER VALID_TIME \
             FROM '2024-01-01T00:00:00Z' TO '2024-02-01T00:00:00Z' RETURN AVG(p.price)"
        )
        .is_err()
    );

    // Unparseable boundary.
    assert!(
        db.execute_aql(
            "MATCH (p:Product) WINDOW 1 day OVER VALID_TIME \
             FROM 'not-a-time' TO '2024-02-01T00:00:00Z' RETURN AVG(p.price)"
        )
        .is_err()
    );

    // Unknown aggregate function.
    assert!(
        db.execute_aql(
            "MATCH (p:Product) WINDOW 1 day OVER VALID_TIME \
             FROM '2024-01-01T00:00:00Z' TO '2024-02-01T00:00:00Z' RETURN MEDIAN(p.price)"
        )
        .is_err()
    );

    // SUM without a property argument.
    assert!(
        db.execute_aql(
            "MATCH (p:Product) WINDOW 1 day OVER VALID_TIME \
             FROM '2024-01-01T00:00:00Z' TO '2024-02-01T00:00:00Z' RETURN SUM(*)"
        )
        .is_err()
    );

    // Aggregate references a different variable than the matched entity.
    assert!(
        db.execute_aql(
            "MATCH (p:Product) WINDOW 1 day OVER VALID_TIME \
             FROM '2024-01-01T00:00:00Z' TO '2024-02-01T00:00:00Z' RETURN AVG(q.price)"
        )
        .is_err()
    );

    // Windowing an edge / traversal (v1: nodes only) is rejected.
    assert!(
        db.execute_aql(
            "MATCH (p:Product)-[r:SELLS]->(q) WINDOW 1 day OVER VALID_TIME \
             FROM '2024-01-01T00:00:00Z' TO '2024-02-01T00:00:00Z' RETURN AVG(r.price)"
        )
        .is_err()
    );
}

// ---------------------------------------------------------------------------
// AC 6: malformed specs map to the exact structured QueryError variants that the
// MCP `query` tool renders as parse_error / invalid_params / unsupported_construct.
//
// This test lives at the query-executor level (execute_aql) rather than the MCP
// surface: the MCP `query` tool forwards the AQL string to this executor and
// then maps each QueryError variant one-to-one to a structured error kind
// (src/mcp/server.rs::map_query_error -- SyntaxError->parse_error,
// InvalidParameter->invalid_params, UnsupportedFeature->unsupported_construct).
// Proving the executor emits the right variant is therefore proof that the MCP
// path returns the right kind, without editing src/mcp. It also proves a valid
// window query returns rows carrying window_start/window_end.
// ---------------------------------------------------------------------------

#[test]
fn malformed_specs_map_to_structured_query_error_variants() {
    let db = AletheiaDB::new().expect("db");
    let _ = create_product(&db, 100, ts("2024-01-01T00:00:00Z"));

    // Backwards range -> InvalidParameter -> MCP `invalid_params`.
    assert!(
        matches!(
            db.execute_aql(
                "MATCH (p:Product) WINDOW 1 day OVER VALID_TIME \
                 FROM '2024-05-01T00:00:00Z' TO '2024-01-01T00:00:00Z' RETURN AVG(p.price)"
            ),
            Err(Error::Query(QueryError::InvalidParameter { .. }))
        ),
        "backwards range must be InvalidParameter"
    );

    // Zero window size -> InvalidParameter.
    assert!(
        matches!(
            db.execute_aql(
                "MATCH (p:Product) WINDOW 0 day OVER VALID_TIME \
                 FROM '2024-01-01T00:00:00Z' TO '2024-02-01T00:00:00Z' RETURN AVG(p.price)"
            ),
            Err(Error::Query(QueryError::InvalidParameter { .. }))
        ),
        "zero window size must be InvalidParameter"
    );

    // Unknown unit -> InvalidParameter.
    assert!(
        matches!(
            db.execute_aql(
                "MATCH (p:Product) WINDOW 1 fortnight OVER VALID_TIME \
                 FROM '2024-01-01T00:00:00Z' TO '2024-02-01T00:00:00Z' RETURN AVG(p.price)"
            ),
            Err(Error::Query(QueryError::InvalidParameter { .. }))
        ),
        "unknown unit must be InvalidParameter"
    );

    // Unparseable boundary -> InvalidParameter.
    assert!(
        matches!(
            db.execute_aql(
                "MATCH (p:Product) WINDOW 1 day OVER VALID_TIME \
                 FROM 'not-a-time' TO '2024-02-01T00:00:00Z' RETURN AVG(p.price)"
            ),
            Err(Error::Query(QueryError::InvalidParameter { .. }))
        ),
        "unparseable boundary must be InvalidParameter"
    );

    // Unknown aggregate function -> UnsupportedFeature -> `unsupported_construct`.
    assert!(
        matches!(
            db.execute_aql(
                "MATCH (p:Product) WINDOW 1 day OVER VALID_TIME \
                 FROM '2024-01-01T00:00:00Z' TO '2024-02-01T00:00:00Z' RETURN MEDIAN(p.price)"
            ),
            Err(Error::Query(QueryError::UnsupportedFeature { .. }))
        ),
        "unknown aggregate must be UnsupportedFeature"
    );

    // Windowing an edge/traversal (v1: nodes only) -> UnsupportedFeature.
    assert!(
        matches!(
            db.execute_aql(
                "MATCH (p:Product)-[r:SELLS]->(q) WINDOW 1 day OVER VALID_TIME \
                 FROM '2024-01-01T00:00:00Z' TO '2024-02-01T00:00:00Z' RETURN AVG(r.price)"
            ),
            Err(Error::Query(QueryError::UnsupportedFeature { .. }))
        ),
        "edge/traversal window must be UnsupportedFeature"
    );

    // Grammatically malformed WINDOW clause -> SyntaxError -> `parse_error`.
    assert!(
        matches!(
            db.execute_aql(
                "MATCH (p:Product) WINDOW day OVER VALID_TIME \
                 FROM '2024-01-01T00:00:00Z' TO '2024-02-01T00:00:00Z' RETURN AVG(p.price)"
            ),
            Err(Error::Query(QueryError::SyntaxError { .. }))
        ),
        "missing window size must be a SyntaxError"
    );

    // A valid query returns rows carrying self-describing window bounds.
    let ok = rows(
        &db,
        "MATCH (p:Product) \
         WINDOW 1 month OVER VALID_TIME FROM '2024-01-01T00:00:00Z' TO '2024-03-01T00:00:00Z' \
         RETURN AVG(p.price) AS avg_price",
    );
    assert_eq!(ok.len(), 2, "two monthly windows");
    assert_eq!(
        col_str(&ok[0], "window_start").as_deref(),
        Some("2024-01-01T00:00:00.000000Z")
    );
    assert_eq!(
        col_str(&ok[0], "window_end").as_deref(),
        Some("2024-02-01T00:00:00.000000Z")
    );
}

// ---------------------------------------------------------------------------
// Success metric: a 12-window monthly aggregation over ~1,000 valid-time
// versions completes end-to-end well within budget (AC target: < 50ms on the
// release perf rig). Correctness is asserted unconditionally; the wallclock
// bound is enforced strictly only in optimized builds (`cargo test --release`),
// where the 50ms target applies, and loosely in unoptimized debug builds to
// still catch a pathological (e.g. accidental O(versions^2)) regression without
// flaking on the ~10x-slower debug reconstruction path.
// ---------------------------------------------------------------------------

#[test]
fn monthly_12_window_over_1000_versions_within_budget() {
    use std::time::Instant;

    let db = AletheiaDB::new().expect("db");
    let year_start = ts("2023-01-01T00:00:00Z");
    let id = create_product(&db, 100, year_start);
    // ~1,000 versions spread across 2023 so all land inside the query range.
    let micros_per_day = 86_400_000_000i64;
    let step = (360 * micros_per_day) / 1000;
    for i in 1..1000i64 {
        update_price(
            &db,
            id,
            100 + i,
            Timestamp::from(micros("2023-01-01T00:00:00Z") + step * i),
        );
    }

    let query = "MATCH (p:Product) \
         WINDOW 1 month OVER VALID_TIME FROM '2023-01-01T00:00:00Z' TO '2024-01-01T00:00:00Z' \
         RETURN AVG(p.price) AS avg_price, MIN(p.price) AS lo, MAX(p.price) AS hi, \
                CHANGES(p.price) AS changes";

    // Warm once (build caches), then take the best of three timed runs.
    let _ = rows(&db, query);
    let mut best = std::time::Duration::from_secs(3600);
    let mut last: Vec<QueryRow> = Vec::new();
    for _ in 0..3 {
        let t0 = Instant::now();
        last = rows(&db, query);
        best = best.min(t0.elapsed());
    }

    // Correctness: exactly 12 monthly windows, all with data (monotonic prices).
    assert_eq!(last.len(), 12, "twelve monthly windows");
    for (i, row) in last.iter().enumerate() {
        assert!(col_f64(row, "avg_price").is_some(), "window {i} has data");
        assert!(col_i64(row, "changes").is_some(), "window {i} changes");
    }

    let budget = if cfg!(debug_assertions) {
        // Loose bound for the unoptimized reconstruction path; a genuine
        // algorithmic regression blows far past this, a 50ms target does not.
        std::time::Duration::from_millis(2000)
    } else {
        // The AC success metric: < 50ms end-to-end for 12 windows / ~1,000
        // versions on an optimized build.
        std::time::Duration::from_millis(50)
    };
    assert!(
        best <= budget,
        "12-window monthly aggregation over ~1,000 versions took {best:?}, exceeding {budget:?}"
    );
}

// ---------------------------------------------------------------------------
// Scope: the temporal WINDOW construct is AQL-only in v1 (Issue #3363 scopes
// Cypher/SQL out). The Cypher surface must *reject* it with a structured error
// rather than mis-parse it into a silently-wrong result. `WINDOW` is not a
// Cypher clause keyword, so the Cypher parser errors out; this test pins that
// (no silent fallback, no accidental acceptance).
// ---------------------------------------------------------------------------

#[cfg(feature = "cypher")]
#[test]
fn cypher_surface_rejects_temporal_window_construct() {
    let db = AletheiaDB::new().expect("db");
    let _ = create_product(&db, 100, ts("2024-01-01T00:00:00Z"));

    let res = db.execute_cypher(
        "MATCH (p:Product) \
         WINDOW 1 month OVER VALID_TIME FROM '2024-01-01T00:00:00Z' TO '2024-05-01T00:00:00Z' \
         RETURN AVG(p.price)",
    );
    assert!(
        res.is_err(),
        "the AQL-only WINDOW construct must be rejected by the Cypher surface, not mis-parsed"
    );
}

// ---------------------------------------------------------------------------
// Non-numeric properties are skipped by value aggregates (never garbage).
// ---------------------------------------------------------------------------

#[test]
fn non_numeric_property_skipped() {
    let db = AletheiaDB::new().expect("db");
    db.create_node_with_options(
        "Product",
        PropertyMapBuilder::new().insert("name", "widget").build(),
        WriteRequestOptions::new().with_valid_from(ts("2024-01-01T00:00:00Z")),
    )
    .expect("create");

    let result = rows(
        &db,
        "MATCH (p:Product) \
         WINDOW 1 month OVER VALID_TIME FROM '2024-01-01T00:00:00Z' TO '2024-02-01T00:00:00Z' \
         RETURN AVG(p.name) AS avg_name, COUNT(p.name) AS numeric_n, COUNT(*) AS present",
    );
    assert_eq!(result.len(), 1);
    // Non-numeric -> no numeric sample -> null value aggregate, zero numeric count,
    // but the entity is still present.
    assert!(is_null(&result[0], "avg_name"));
    assert_eq!(col_i64(&result[0], "numeric_n"), Some(0));
    assert_eq!(col_i64(&result[0], "present"), Some(1));
}

// ---------------------------------------------------------------------------
// Grammar error paths: every structurally-malformed WINDOW clause is rejected
// as a SyntaxError (never a panic or a silently-wrong parse). Each line below
// exercises a distinct `expect`/`expect_kw`/`parse_identifier` failure branch in
// the window-clause parser. A trailing clause after `WINDOW ... RETURN` is also
// rejected (the window clause is self-terminating).
// ---------------------------------------------------------------------------

#[test]
fn window_clause_grammar_errors_are_syntax_errors() {
    let db = AletheiaDB::new().expect("db");
    let _ = create_product(&db, 100, ts("2024-01-01T00:00:00Z"));

    let bad_queries = [
        // Unit is not an identifier (an integer follows the window size).
        "MATCH (p:Product) WINDOW 1 2 OVER VALID_TIME \
         FROM '2024-01-01T00:00:00Z' TO '2024-02-01T00:00:00Z' RETURN AVG(p.price)",
        // Missing OVER.
        "MATCH (p:Product) WINDOW 1 day VALID_TIME \
         FROM '2024-01-01T00:00:00Z' TO '2024-02-01T00:00:00Z' RETURN AVG(p.price)",
        // Missing VALID_TIME.
        "MATCH (p:Product) WINDOW 1 day OVER \
         FROM '2024-01-01T00:00:00Z' TO '2024-02-01T00:00:00Z' RETURN AVG(p.price)",
        // Missing FROM.
        "MATCH (p:Product) WINDOW 1 day OVER VALID_TIME \
         '2024-01-01T00:00:00Z' TO '2024-02-01T00:00:00Z' RETURN AVG(p.price)",
        // Missing TO (two timestamps in a row).
        "MATCH (p:Product) WINDOW 1 day OVER VALID_TIME \
         FROM '2024-01-01T00:00:00Z' '2024-02-01T00:00:00Z' RETURN AVG(p.price)",
        // AS without OF (partial AS OF SYSTEM_TIME).
        "MATCH (p:Product) WINDOW 1 day OVER VALID_TIME \
         FROM '2024-01-01T00:00:00Z' TO '2024-02-01T00:00:00Z' AS SYSTEM_TIME 0 \
         RETURN AVG(p.price)",
        // AS OF without SYSTEM_TIME.
        "MATCH (p:Product) WINDOW 1 day OVER VALID_TIME \
         FROM '2024-01-01T00:00:00Z' TO '2024-02-01T00:00:00Z' AS OF WALLCLOCK 0 \
         RETURN AVG(p.price)",
        // Aggregate function name missing (opens straight into a paren).
        "MATCH (p:Product) WINDOW 1 day OVER VALID_TIME \
         FROM '2024-01-01T00:00:00Z' TO '2024-02-01T00:00:00Z' RETURN (p.price)",
        // Missing '(' after the aggregate function.
        "MATCH (p:Product) WINDOW 1 day OVER VALID_TIME \
         FROM '2024-01-01T00:00:00Z' TO '2024-02-01T00:00:00Z' RETURN AVG p.price",
        // Missing ')' closing the aggregate argument (runs to end of query).
        "MATCH (p:Product) WINDOW 1 day OVER VALID_TIME \
         FROM '2024-01-01T00:00:00Z' TO '2024-02-01T00:00:00Z' RETURN AVG(p.price",
        // Trailing clause after a self-terminating WINDOW ... RETURN.
        "MATCH (p:Product) WINDOW 1 day OVER VALID_TIME \
         FROM '2024-01-01T00:00:00Z' TO '2024-02-01T00:00:00Z' RETURN AVG(p.price) LIMIT 10",
    ];

    for q in bad_queries {
        assert!(
            matches!(
                db.execute_aql(q),
                Err(Error::Query(QueryError::SyntaxError { .. }))
            ),
            "expected a SyntaxError for malformed window query:\n{q}"
        );
    }
}

// ---------------------------------------------------------------------------
// A granularity/range pair that would generate an enormous number of windows is
// rejected with a structured InvalidParameter (the TooManyWindows guard), rather
// than allocating unbounded rows.
// ---------------------------------------------------------------------------

#[test]
fn too_many_windows_rejected_with_structured_error() {
    let db = AletheiaDB::new().expect("db");
    let _ = create_product(&db, 100, ts("2024-01-01T00:00:00Z"));

    // 24 years at 1-minute granularity is ~12.6M windows, far over the cap.
    assert!(
        matches!(
            db.execute_aql(
                "MATCH (p:Product) WINDOW 1 minute OVER VALID_TIME \
                 FROM '2000-01-01T00:00:00Z' TO '2024-01-01T00:00:00Z' RETURN COUNT(*)"
            ),
            Err(Error::Query(QueryError::InvalidParameter { .. }))
        ),
        "an over-cap window count must be InvalidParameter"
    );

    // A window multiplier exceeding u32 is likewise an InvalidParameter.
    assert!(
        matches!(
            db.execute_aql(
                "MATCH (p:Product) WINDOW 5000000000 day OVER VALID_TIME \
                 FROM '2024-01-01T00:00:00Z' TO '2024-02-01T00:00:00Z' RETURN COUNT(*)"
            ),
            Err(Error::Query(QueryError::InvalidParameter { .. }))
        ),
        "a window size exceeding u32 must be InvalidParameter"
    );
}

// ---------------------------------------------------------------------------
// Value aggregates over float-valued properties keep float arithmetic (SUM/AVG
// stay floats; the all-integer promotion path is not taken).
// ---------------------------------------------------------------------------

#[test]
fn float_property_value_aggregates() {
    let db = AletheiaDB::new().expect("db");
    db.create_node_with_options(
        "Gadget",
        PropertyMapBuilder::new().insert("price", 100.5f64).build(),
        WriteRequestOptions::new().with_valid_from(ts("2024-01-01T00:00:00Z")),
    )
    .expect("create");
    db.create_node_with_options(
        "Gadget",
        PropertyMapBuilder::new().insert("price", 200.25f64).build(),
        WriteRequestOptions::new().with_valid_from(ts("2024-01-01T00:00:00Z")),
    )
    .expect("create");

    let result = rows(
        &db,
        "MATCH (g:Gadget) \
         WINDOW 1 month OVER VALID_TIME FROM '2024-01-01T00:00:00Z' TO '2024-02-01T00:00:00Z' \
         RETURN SUM(g.price) AS total, AVG(g.price) AS avg_price, \
                MIN(g.price) AS lo, MAX(g.price) AS hi",
    );
    assert_eq!(result.len(), 1);
    // Floating sum/avg over {100.5, 200.25}.
    assert_eq!(col_f64(&result[0], "total"), Some(300.75));
    assert_eq!(col_f64(&result[0], "avg_price"), Some(150.375));
    assert_eq!(col_f64(&result[0], "lo"), Some(100.5));
    assert_eq!(col_f64(&result[0], "hi"), Some(200.25));
}

// ---------------------------------------------------------------------------
// Bare-entity (`COUNT(v)` / `CHANGES(v)`) and `CHANGES(*)` argument forms, plus
// the default (unaliased) output-column naming for each argument shape.
// ---------------------------------------------------------------------------

#[test]
fn bare_entity_and_star_aggregates_with_default_names() {
    let db = AletheiaDB::new().expect("db");
    let id = create_product(&db, 100, ts("2024-01-01T00:00:00Z"));
    // A second version starting inside the window is one entity change-point.
    update_price(&db, id, 150, ts("2024-01-15T00:00:00Z"));

    let result = rows(
        &db,
        "MATCH (p:Product) \
         WINDOW 1 month OVER VALID_TIME FROM '2024-01-01T00:00:00Z' TO '2024-02-01T00:00:00Z' \
         RETURN COUNT(p), COUNT(*), CHANGES(p), CHANGES(*)",
    );
    assert_eq!(result.len(), 1);

    // Default output names follow `func(arg)` (lower-cased), for entity and star.
    assert_eq!(
        col_i64(&result[0], "count(p)"),
        Some(1),
        "entity present once"
    );
    assert_eq!(col_i64(&result[0], "count(*)"), Some(1));
    // Two version-starts inside January (Jan 1 and Jan 15).
    assert_eq!(col_i64(&result[0], "changes(p)"), Some(2));
    assert_eq!(col_i64(&result[0], "changes(*)"), Some(2));
}
