//! End-to-end tests for AQL temporal joins (Issue #3379).
//!
//! These exercise the full pipeline (`execute_aql` -> parser -> converter ->
//! planner -> executor) and assert hand-computed alignment output for both
//! modes (event-aligned and interval-overlap), the documented half-open
//! boundary rules, bi-temporal `AS OF SYSTEM_TIME` correctness, traversal /
//! edge-validity composition, empty-overlap handling, and structured errors.
//! The pure interval math has its own unit tests in
//! `src/query/temporal_join.rs`; these cover semantics that require real
//! bi-temporal history.

use aletheiadb::AletheiaDB;
use aletheiadb::PropertyMapBuilder;
use aletheiadb::api::transaction::WriteRequestOptions;
use aletheiadb::core::error::{Error, QueryError};
use aletheiadb::core::property::PropertyValue;
use aletheiadb::core::temporal::Timestamp;
use aletheiadb::core::{EdgeId, NodeId};
use aletheiadb::query::executor::QueryRow;
use aletheiadb::query::temporal_window::parse_boundary_micros;

fn micros(rfc: &str) -> i64 {
    parse_boundary_micros(rfc).expect("valid rfc3339")
}

fn ts(rfc: &str) -> Timestamp {
    Timestamp::from(micros(rfc))
}

fn rows(db: &AletheiaDB, aql: &str) -> Vec<QueryRow> {
    db.execute_aql(aql)
        .expect("query executes")
        .collect_all()
        .expect("collect rows")
}

fn col<'a>(row: &'a QueryRow, name: &str) -> Option<&'a PropertyValue> {
    row.columns
        .as_ref()
        .and_then(|cols| cols.iter().find(|(k, _)| k == name).map(|(_, v)| v))
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

fn create_node_vt(db: &AletheiaDB, label: &str, key: &str, val: i64, vf: Timestamp) -> NodeId {
    db.create_node_with_options(
        label,
        PropertyMapBuilder::new().insert(key, val).build(),
        WriteRequestOptions::new().with_valid_from(vf),
    )
    .expect("create node")
}

fn update_node_vt(db: &AletheiaDB, id: NodeId, key: &str, val: i64, vf: Timestamp) {
    db.update_node_with_options(
        id,
        PropertyMapBuilder::new().insert(key, val).build(),
        WriteRequestOptions::new().with_valid_from(vf),
    )
    .expect("update node");
}

fn create_edge_vt(db: &AletheiaDB, src: NodeId, dst: NodeId, label: &str, vf: Timestamp) -> EdgeId {
    db.create_edge_with_options(
        src,
        dst,
        label,
        PropertyMapBuilder::new().build(),
        WriteRequestOptions::new().with_valid_from(vf),
    )
    .expect("create edge")
}

// ---------------------------------------------------------------------------
// AC 1/2/4: event-aligned join over a traversal — "customer tier at order time".
// ---------------------------------------------------------------------------

#[test]
fn events_customer_tier_at_each_order_time() {
    let db = AletheiaDB::new().expect("db");
    // Customer with a tier that upgrades mid-year.
    let customer = create_node_vt(&db, "Customer", "tier", 1, ts("2024-01-01T00:00:00Z"));
    update_node_vt(&db, customer, "tier", 2, ts("2024-06-01T00:00:00Z"));
    // Order placed (v1) in Feb, amended (v2) in Sep, amended (v3) in ... only 2 events in range.
    let order = create_node_vt(&db, "Purchase", "total", 10, ts("2024-02-01T00:00:00Z"));
    update_node_vt(&db, order, "total", 20, ts("2024-09-01T00:00:00Z"));
    // Order -> Customer edge (valid all year).
    create_edge_vt(
        &db,
        order,
        customer,
        "PLACED_BY",
        ts("2024-01-01T00:00:00Z"),
    );

    let result = rows(
        &db,
        "MATCH (o:Purchase)-[:PLACED_BY]->(c:Customer) \
         ALIGN EVENTS DRIVER o \
         OVER VALID_TIME FROM '2024-01-01T00:00:00Z' TO '2025-01-01T00:00:00Z' \
         RETURN o.total, c.tier AS tier",
    );

    // Two driver (order) change-points in range: Feb-01, Sep-01.
    assert_eq!(result.len(), 2, "one row per order event");
    assert_eq!(
        col_str(&result[0], "align_valid_time").as_deref(),
        Some("2024-02-01T00:00:00.000000Z")
    );
    // Feb order: order total 10, customer still tier 1 (upgrade is Jun).
    assert_eq!(col_i64(&result[0], "o.total"), Some(10));
    assert_eq!(col_i64(&result[0], "tier"), Some(1));
    // Sep order: order total 20, customer now tier 2.
    assert_eq!(
        col_str(&result[1], "align_valid_time").as_deref(),
        Some("2024-09-01T00:00:00.000000Z")
    );
    assert_eq!(col_i64(&result[1], "o.total"), Some(20));
    assert_eq!(col_i64(&result[1], "tier"), Some(2));
}

// ---------------------------------------------------------------------------
// AC 3: participant absent (order placed before customer existed) -> null.
// ---------------------------------------------------------------------------

#[test]
fn events_participant_absent_is_null() {
    let db = AletheiaDB::new().expect("db");
    let customer = create_node_vt(&db, "Customer", "tier", 1, ts("2024-03-01T00:00:00Z"));
    let order = create_node_vt(&db, "Purchase", "total", 10, ts("2024-01-15T00:00:00Z"));
    create_edge_vt(
        &db,
        order,
        customer,
        "PLACED_BY",
        ts("2024-01-01T00:00:00Z"),
    );

    let result = rows(
        &db,
        "MATCH (o:Purchase)-[:PLACED_BY]->(c:Customer) \
         ALIGN EVENTS DRIVER o \
         OVER VALID_TIME FROM '2024-01-01T00:00:00Z' TO '2024-12-01T00:00:00Z' \
         RETURN o.total, c.tier AS tier",
    );
    assert_eq!(result.len(), 1);
    // Order placed Jan-15, before the customer record existed (Mar-01).
    assert_eq!(col_i64(&result[0], "o.total"), Some(10));
    assert!(is_null(&result[0], "tier"), "customer absent -> null tier");
}

// ---------------------------------------------------------------------------
// AC 6: interval-overlap hand-computed fixture with overlapping, adjacent, and
// gapped versions across two entities.
// ---------------------------------------------------------------------------

#[test]
fn overlap_two_entities_hand_computed() {
    let db = AletheiaDB::new().expect("db");
    // Product price: 100 from Jan, 150 from Apr.
    let product = create_node_vt(&db, "Product", "price", 100, ts("2024-01-01T00:00:00Z"));
    update_node_vt(&db, product, "price", 150, ts("2024-04-01T00:00:00Z"));
    // Supplier rating: 5 from Feb (did not exist in January -> Jan is a gap).
    let supplier = create_node_vt(&db, "Supplier", "rating", 5, ts("2024-02-01T00:00:00Z"));
    update_node_vt(&db, supplier, "rating", 4, ts("2024-05-01T00:00:00Z"));
    create_edge_vt(
        &db,
        product,
        supplier,
        "SUPPLIED_BY",
        ts("2024-01-01T00:00:00Z"),
    );

    let result = rows(
        &db,
        "MATCH (p:Product)-[:SUPPLIED_BY]->(s:Supplier) \
         ALIGN OVERLAP \
         OVER VALID_TIME FROM '2024-01-01T00:00:00Z' TO '2024-06-01T00:00:00Z' \
         RETURN p.price AS price, s.rating AS rating",
    );

    // Boundaries in range: Jan-01(from), Feb-01(supplier), Apr-01(product),
    // May-01(supplier). Sub-intervals:
    //  [Jan,Feb) -> supplier absent -> DROPPED (gap)
    //  [Feb,Apr) -> price 100, rating 5
    //  [Apr,May) -> price 150, rating 5
    //  [May,Jun) -> price 150, rating 4
    assert_eq!(
        result.len(),
        3,
        "three co-hold sub-intervals (Jan gap dropped)"
    );

    assert_eq!(
        col_str(&result[0], "overlap_from").as_deref(),
        Some("2024-02-01T00:00:00.000000Z")
    );
    assert_eq!(
        col_str(&result[0], "overlap_to").as_deref(),
        Some("2024-04-01T00:00:00.000000Z")
    );
    assert_eq!(col_i64(&result[0], "price"), Some(100));
    assert_eq!(col_i64(&result[0], "rating"), Some(5));

    assert_eq!(col_i64(&result[1], "price"), Some(150));
    assert_eq!(col_i64(&result[1], "rating"), Some(5));

    assert_eq!(
        col_str(&result[2], "overlap_to").as_deref(),
        Some("2024-06-01T00:00:00.000000Z")
    );
    assert_eq!(col_i64(&result[2], "price"), Some(150));
    assert_eq!(col_i64(&result[2], "rating"), Some(4));
}

// ---------------------------------------------------------------------------
// AC 2/6: an edge that appears mid-range gates the overlap. The relationship's
// validity (here, "appears in March") is honored at each alignment instant, so
// no sub-interval before the edge existed is emitted.
//
// (The symmetric "edge disappears" half is covered by the pure-core unit test
// `overlap_edge_appears_and_disappears_mid_range`: a *fully* retracted edge is
// removed from the current adjacency index, so — like #3225 AS OF traversal,
// which enumerates candidates from current state — it can no longer be bound
// by a MATCH; the gating math itself handles closed intervals. This is a
// documented v1 limitation.)
// ---------------------------------------------------------------------------

#[test]
fn overlap_edge_appears_mid_range_gates_the_join() {
    let db = AletheiaDB::new().expect("db");
    let product = create_node_vt(&db, "Product", "price", 100, ts("2024-01-01T00:00:00Z"));
    let supplier = create_node_vt(&db, "Supplier", "rating", 5, ts("2024-01-01T00:00:00Z"));
    // Edge appears in March and remains valid (current) -> present [Mar, now).
    create_edge_vt(
        &db,
        product,
        supplier,
        "SUPPLIED_BY",
        ts("2024-03-01T00:00:00Z"),
    );

    let result = rows(
        &db,
        "MATCH (p:Product)-[:SUPPLIED_BY]->(s:Supplier) \
         ALIGN OVERLAP \
         OVER VALID_TIME FROM '2024-01-01T00:00:00Z' TO '2024-12-01T00:00:00Z' \
         RETURN p.price AS price, s.rating AS rating",
    );

    // The edge is not valid before March, so [Jan, Mar) is dropped; the single
    // co-hold sub-interval begins exactly when the edge appears.
    assert_eq!(
        result.len(),
        1,
        "edge gates the join to start at its appearance"
    );
    assert_eq!(
        col_str(&result[0], "overlap_from").as_deref(),
        Some("2024-03-01T00:00:00.000000Z")
    );
    assert_eq!(
        col_str(&result[0], "overlap_to").as_deref(),
        Some("2024-12-01T00:00:00.000000Z")
    );
    assert_eq!(col_i64(&result[0], "price"), Some(100));
    assert_eq!(col_i64(&result[0], "rating"), Some(5));
}

// ---------------------------------------------------------------------------
// AC 3: transaction-time pinning — a later correction does not rewrite the
// analytics pinned before it was recorded.
// ---------------------------------------------------------------------------

#[test]
fn events_as_of_system_time_pins_belief() {
    let db = AletheiaDB::new().expect("db");
    let customer = create_node_vt(&db, "Customer", "tier", 1, ts("2024-01-01T00:00:00Z"));
    let order = create_node_vt(&db, "Purchase", "total", 10, ts("2024-02-01T00:00:00Z"));
    create_edge_vt(
        &db,
        order,
        customer,
        "PLACED_BY",
        ts("2024-01-01T00:00:00Z"),
    );
    // Pin the transaction time NOW, before recording a correction.
    let pin = Timestamp::from(aletheiadb::core::temporal::time::now().wallclock());
    // Later correction: backdate the tier to 9 effective Jan-01 (a restatement).
    update_node_vt(&db, customer, "tier", 9, ts("2024-01-01T00:00:00Z"));

    // As of the pin, the correction is invisible -> tier is still 1.
    let pinned = rows(
        &db,
        &format!(
            "MATCH (o:Purchase)-[:PLACED_BY]->(c:Customer) \
             ALIGN EVENTS DRIVER o \
             OVER VALID_TIME FROM '2024-01-01T00:00:00Z' TO '2024-12-01T00:00:00Z' \
             AS OF SYSTEM_TIME {} \
             RETURN c.tier AS tier",
            pin.wallclock()
        ),
    );
    assert_eq!(pinned.len(), 1);
    assert_eq!(
        col_i64(&pinned[0], "tier"),
        Some(1),
        "pinned belief excludes the later correction"
    );

    // With no tx-time pin (now), the correction IS visible -> tier is 9.
    let current = rows(
        &db,
        "MATCH (o:Purchase)-[:PLACED_BY]->(c:Customer) \
         ALIGN EVENTS DRIVER o \
         OVER VALID_TIME FROM '2024-01-01T00:00:00Z' TO '2024-12-01T00:00:00Z' \
         RETURN c.tier AS tier",
    );
    assert_eq!(col_i64(&current[0], "tier"), Some(9));
}

// ---------------------------------------------------------------------------
// AC 1: single bound node (degenerate join) — overlap over one entity's history.
// ---------------------------------------------------------------------------

#[test]
fn overlap_single_node_history_tiles_range() {
    let db = AletheiaDB::new().expect("db");
    let p = create_node_vt(&db, "Product", "price", 100, ts("2024-01-01T00:00:00Z"));
    update_node_vt(&db, p, "price", 200, ts("2024-02-01T00:00:00Z"));
    update_node_vt(&db, p, "price", 300, ts("2024-03-01T00:00:00Z"));

    let result = rows(
        &db,
        "MATCH (p:Product) \
         ALIGN OVERLAP \
         OVER VALID_TIME FROM '2024-01-01T00:00:00Z' TO '2024-04-01T00:00:00Z' \
         RETURN p.price AS price",
    );
    assert_eq!(result.len(), 3);
    assert_eq!(col_i64(&result[0], "price"), Some(100));
    assert_eq!(col_i64(&result[1], "price"), Some(200));
    assert_eq!(col_i64(&result[2], "price"), Some(300));
}

// ---------------------------------------------------------------------------
// AC 5: structured errors for malformed specs.
// ---------------------------------------------------------------------------

fn expect_invalid(db: &AletheiaDB, aql: &str) {
    // The error may surface at plan/convert time (`execute_aql`) or, defensively,
    // while draining rows (`collect_all`). Either is acceptable.
    let err = match db.execute_aql(aql) {
        Ok(results) => match results.collect_all() {
            Ok(_) => panic!("expected a structured query error, got success"),
            Err(e) => e,
        },
        Err(e) => e,
    };
    assert!(
        matches!(
            err,
            Error::Query(QueryError::InvalidParameter { .. })
                | Error::Query(QueryError::UnsupportedFeature { .. })
        ),
        "expected a structured query error, got {err:?}"
    );
}

#[test]
fn errors_are_structured() {
    let db = AletheiaDB::new().expect("db");
    let _ = create_node_vt(&db, "Product", "price", 1, ts("2024-01-01T00:00:00Z"));

    // Unknown RETURN variable.
    expect_invalid(
        &db,
        "MATCH (p:Product) ALIGN OVERLAP \
         OVER VALID_TIME FROM '2024-01-01T00:00:00Z' TO '2024-02-01T00:00:00Z' \
         RETURN q.price",
    );
    // Empty/backwards range.
    expect_invalid(
        &db,
        "MATCH (p:Product) ALIGN OVERLAP \
         OVER VALID_TIME FROM '2024-02-01T00:00:00Z' TO '2024-01-01T00:00:00Z' \
         RETURN p.price",
    );
    // Driver variable not a participant.
    expect_invalid(
        &db,
        "MATCH (p:Product) ALIGN EVENTS DRIVER x \
         OVER VALID_TIME FROM '2024-01-01T00:00:00Z' TO '2024-02-01T00:00:00Z' \
         RETURN p.price",
    );
    // Unsupported multi-hop pattern.
    expect_invalid(
        &db,
        "MATCH (a:Product)-[:R]->(b:Product)-[:R]->(c:Product) ALIGN OVERLAP \
         OVER VALID_TIME FROM '2024-01-01T00:00:00Z' TO '2024-02-01T00:00:00Z' \
         RETURN a.price",
    );
}
