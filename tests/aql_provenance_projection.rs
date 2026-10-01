//! Integration tests for AQL provenance accessors in `RETURN` / `ORDER BY`
//! projection (Issue #3354, projection half).
//!
//! The WHERE-filtering half (Issue #3354a) shipped previously
//! (`tests/aql_provenance_where.rs`). This suite covers the complementary
//! ability to *project* a provenance accessor (`source(n)` / `confidence(n)` /
//! `reason(n)`) as an output column and to `ORDER BY` one, end-to-end through
//! `execute_aql`.
//!
//! To keep the returned entity observable alongside the projected columns, a
//! projection that also returns a bare entity emits the row through the
//! bindings+columns shape (mirroring #549 multi-var / #558 aggregation), so the
//! entity survives and the columns carry the provenance values.

use aletheiadb::AletheiaDB;
use aletheiadb::PropertyMapBuilder;
use aletheiadb::api::transaction::WriteRequestOptions;
use aletheiadb::core::NodeId;
use aletheiadb::core::property::PropertyValue;
use aletheiadb::core::provenance::Provenance;
use aletheiadb::query::executor::{EntityResult, QueryRow};

fn prov(source: Option<&str>, confidence: Option<f64>, reason: Option<&str>) -> Provenance {
    let mut b = Provenance::builder();
    if let Some(s) = source {
        b = b.source(s);
    }
    if let Some(c) = confidence {
        b = b.confidence(c);
    }
    if let Some(r) = reason {
        b = b.note(r);
    }
    b.build().expect("valid provenance")
}

fn create_person(db: &AletheiaDB, name: &str, provenance: Option<Provenance>) -> NodeId {
    let props = PropertyMapBuilder::new().insert("name", name).build();
    let mut opts = WriteRequestOptions::new();
    if let Some(p) = provenance {
        opts = opts.with_provenance(p);
    }
    db.create_node_with_options("Person", props, opts)
        .expect("create person")
}

fn make_db() -> AletheiaDB {
    let db = AletheiaDB::new().expect("db");
    create_person(
        &db,
        "alice",
        Some(prov(Some("hr-system"), Some(0.95), Some("verified"))),
    );
    create_person(
        &db,
        "bob",
        Some(prov(Some("crm-sync"), Some(0.60), Some("imported"))),
    );
    create_person(
        &db,
        "carol",
        Some(prov(Some("hr-system"), Some(0.80), None)),
    );
    create_person(&db, "dave", None); // unattributed
    db
}

/// Collect all rows of an AQL query.
fn rows(db: &AletheiaDB, aql: &str) -> Vec<QueryRow> {
    db.execute_aql(aql)
        .expect("query executes")
        .collect_all()
        .expect("collect rows")
}

/// The `name` property of whatever entity a row carries (from `entity` or the
/// first node binding), or `None`.
fn row_name(row: &QueryRow) -> Option<String> {
    let node = match &row.entity {
        EntityResult::Node(n) => Some(n),
        _ => row.bindings.as_ref().and_then(|b| {
            b.iter().find_map(|(_, e)| match e {
                EntityResult::Node(n) => Some(n),
                _ => None,
            })
        }),
    }?;
    node.get_property("name").map(|v| format!("{v:?}"))
}

/// Look up a projected column value by name in a row.
fn col<'a>(row: &'a QueryRow, name: &str) -> Option<&'a PropertyValue> {
    row.columns
        .as_ref()
        .and_then(|cols| cols.iter().find(|(k, _)| k == name).map(|(_, v)| v))
}

#[test]
fn return_entity_and_source_projects_column_and_keeps_entity() {
    let db = make_db();
    let rows = rows(&db, "MATCH (n:Person) RETURN n, source(n)");
    assert_eq!(rows.len(), 4, "one row per person");
    // Every row must still expose its entity (via bindings) AND the projected
    // source column.
    let alice = rows
        .iter()
        .find(|r| row_name(r).as_deref() == Some("String(\"alice\")"))
        .expect("alice row present with observable entity");
    assert_eq!(
        col(alice, "source(n)"),
        Some(&PropertyValue::String("hr-system".into()))
    );
    // Bob's source differs.
    let bob = rows
        .iter()
        .find(|r| row_name(r).as_deref() == Some("String(\"bob\")"))
        .expect("bob row");
    assert_eq!(
        col(bob, "source(n)"),
        Some(&PropertyValue::String("crm-sync".into()))
    );
}

#[test]
fn source_alias_is_column_name() {
    let db = make_db();
    let rows = rows(&db, "MATCH (n:Person) RETURN source(n) AS src");
    let alice = rows
        .iter()
        .find(|r| col(r, "src") == Some(&PropertyValue::String("hr-system".into())));
    assert!(alice.is_some(), "aliased column `src` carries the source");
}

#[test]
fn confidence_projects_numeric_value() {
    let db = make_db();
    let rows = rows(&db, "MATCH (n:Person) RETURN n, confidence(n)");
    let alice = rows
        .iter()
        .find(|r| row_name(r).as_deref() == Some("String(\"alice\")"))
        .expect("alice row");
    match col(alice, "confidence(n)") {
        Some(PropertyValue::Float(f)) => assert!((f - 0.95).abs() < 1e-9, "got {f}"),
        other => panic!("expected numeric Float confidence, got {other:?}"),
    }
}

#[test]
fn unattributed_projects_null() {
    let db = make_db();
    let rows = rows(&db, "MATCH (n:Person) RETURN n, source(n), confidence(n)");
    let dave = rows
        .iter()
        .find(|r| row_name(r).as_deref() == Some("String(\"dave\")"))
        .expect("dave row present");
    assert_eq!(col(dave, "source(n)"), Some(&PropertyValue::Null));
    assert_eq!(col(dave, "confidence(n)"), Some(&PropertyValue::Null));
}

#[test]
fn missing_field_of_partial_bundle_projects_null() {
    let db = make_db();
    // carol has a bundle but no reason -> reason projects Null (distinct from
    // dave, who has no bundle at all).
    let rows = rows(&db, "MATCH (n:Person) RETURN n, reason(n)");
    let carol = rows
        .iter()
        .find(|r| row_name(r).as_deref() == Some("String(\"carol\")"))
        .expect("carol row");
    assert_eq!(col(carol, "reason(n)"), Some(&PropertyValue::Null));
}

#[test]
fn order_by_confidence_desc_sorts_by_projected_provenance() {
    let db = make_db();
    let rows = rows(
        &db,
        "MATCH (n:Person) RETURN n, confidence(n) ORDER BY confidence(n) DESC",
    );
    let order: Vec<String> = rows.iter().filter_map(row_name).collect();
    // alice 0.95, carol 0.80, bob 0.60, then dave (null) last for DESC? Null
    // placement: nulls first for DESC. So dave (null) leads, then desc values.
    assert_eq!(
        order,
        vec![
            "String(\"dave\")",
            "String(\"alice\")",
            "String(\"carol\")",
            "String(\"bob\")",
        ],
        "DESC: nulls first (openCypher), then descending confidence"
    );
}

#[test]
fn order_by_confidence_not_in_return_sorts_and_hides_column() {
    let db = make_db();
    // confidence(n) drives the ordering but is NOT a RETURN item -> it must not
    // appear as an output column.
    let rows = rows(&db, "MATCH (n:Person) RETURN n ORDER BY confidence(n)");
    let order: Vec<String> = rows.iter().filter_map(row_name).collect();
    // ASC: ascending values then nulls last. bob 0.60, carol 0.80, alice 0.95, dave null.
    assert_eq!(
        order,
        vec![
            "String(\"bob\")",
            "String(\"carol\")",
            "String(\"alice\")",
            "String(\"dave\")",
        ]
    );
    // No provenance column leaked into output.
    for r in &rows {
        assert!(
            col(r, "confidence(n)").is_none(),
            "ORDER BY-only accessor must not surface as a column"
        );
    }
}

#[test]
fn reason_projects_real_string_value() {
    let db = make_db();
    // alice's reason is a real string ("verified") -- assert the projected value,
    // not just the Null case.
    let rows = rows(&db, "MATCH (n:Person) RETURN n, reason(n)");
    let alice = rows
        .iter()
        .find(|r| row_name(r).as_deref() == Some("String(\"alice\")"))
        .expect("alice row");
    assert_eq!(
        col(alice, "reason(n)"),
        Some(&PropertyValue::String("verified".into()))
    );
}

#[test]
fn source_only_projection_without_entity_is_columns_row() {
    let db = make_db();
    // No bare entity returned -> a pure columns row (like aggregation output).
    let rows = rows(&db, "MATCH (n:Person) RETURN source(n)");
    assert_eq!(rows.len(), 4);
    // Every row is a pure columns row: no bindings, entity is Null (matching this
    // test's name -- a columns-only shape).
    for r in &rows {
        assert!(r.bindings.is_none(), "columns-only row carries no bindings");
        assert!(
            matches!(r.entity, EntityResult::Null),
            "columns-only row has a Null entity"
        );
    }
    let sources: Vec<PropertyValue> = rows
        .iter()
        .filter_map(|r| col(r, "source(n)").cloned())
        .collect();
    assert!(sources.contains(&PropertyValue::String("hr-system".into())));
    assert!(sources.contains(&PropertyValue::Null)); // dave
}

#[test]
fn where_provenance_filter_composes_with_projection() {
    let db = make_db();
    // WHERE narrows to the two hr-system rows (alice, carol); the projected
    // confidence column must still be correct on the surviving rows.
    let rows = rows(
        &db,
        "MATCH (n:Person) WHERE source(n) = 'hr-system' RETURN n, confidence(n)",
    );
    let mut names: Vec<String> = rows.iter().filter_map(row_name).collect();
    names.sort();
    assert_eq!(
        names,
        vec![
            "String(\"alice\")".to_string(),
            "String(\"carol\")".to_string()
        ],
        "filter narrows to the two hr-system rows"
    );
    let alice = rows
        .iter()
        .find(|r| row_name(r).as_deref() == Some("String(\"alice\")"))
        .expect("alice row");
    match col(alice, "confidence(n)") {
        Some(PropertyValue::Float(f)) => assert!((f - 0.95).abs() < 1e-9, "got {f}"),
        other => panic!("expected 0.95, got {other:?}"),
    }
}

#[test]
fn order_by_confidence_desc_with_skip_limit_pages_projected_rows() {
    // ProjectProvenance runs LAST (after Sort/Skip/Limit), so it is strictly 1:1
    // over the paged rows. Full DESC order is [dave(null), alice 0.95, carol 0.80,
    // bob 0.60]; SKIP 1 LIMIT 2 keeps exactly [alice, carol], each carrying its
    // own projected confidence column.
    let db = make_db();
    let rows = rows(
        &db,
        "MATCH (n:Person) RETURN n, confidence(n) ORDER BY confidence(n) DESC SKIP 1 LIMIT 2",
    );
    let order: Vec<String> = rows.iter().filter_map(row_name).collect();
    assert_eq!(
        order,
        vec![
            "String(\"alice\")".to_string(),
            "String(\"carol\")".to_string()
        ],
        "SKIP 1 LIMIT 2 over the DESC ordering keeps alice then carol"
    );
    // Each surviving row carries the correct projected confidence.
    match col(&rows[0], "confidence(n)") {
        Some(PropertyValue::Float(f)) => assert!((f - 0.95).abs() < 1e-9, "alice got {f}"),
        other => panic!("expected alice 0.95, got {other:?}"),
    }
    match col(&rows[1], "confidence(n)") {
        Some(PropertyValue::Float(f)) => assert!((f - 0.80).abs() < 1e-9, "carol got {f}"),
        other => panic!("expected carol 0.80, got {other:?}"),
    }
}

#[test]
fn order_by_source_string_places_two_nulls_last() {
    // Two unattributed nodes exercise the (None, None) comparison arm and the
    // String ORDER BY path with openCypher null placement (nulls last for ASC).
    let db = AletheiaDB::new().expect("db");
    create_person(&db, "alice", Some(prov(Some("hr-system"), None, None)));
    create_person(&db, "bob", Some(prov(Some("crm-sync"), None, None)));
    create_person(&db, "dave", None); // unattributed
    create_person(&db, "erin", None); // second unattributed -> two nulls
    let rows = rows(&db, "MATCH (n:Person) RETURN n ORDER BY source(n)");
    let order: Vec<String> = rows.iter().filter_map(row_name).collect();
    // ASC on the string source: crm-sync (bob) < hr-system (alice), then the two
    // nulls last, stable in insertion order (dave before erin).
    assert_eq!(
        order,
        vec![
            "String(\"bob\")",
            "String(\"alice\")",
            "String(\"dave\")",
            "String(\"erin\")",
        ]
    );
}

#[test]
fn rejects_property_and_accessor_mix() {
    // (a) A property projection mixed with an accessor is a structured error, not
    // a silently-dropped property.
    let db = make_db();
    assert!(
        db.execute_aql("MATCH (n:Person) RETURN n.name, source(n)")
            .is_err(),
        "property + accessor mix must be rejected"
    );
}

#[test]
fn rejects_multi_variable_accessor_mismatch() {
    // (c) The accessor names a variable other than the single bound entity: must
    // be rejected rather than resolving the wrong (or a nonexistent) entity.
    let db = make_db();
    assert!(
        db.execute_aql("MATCH (n:Person) RETURN n, source(m)")
            .is_err(),
        "accessor over a non-bound variable must be rejected"
    );
}

#[test]
fn rejects_edge_variable_accessor() {
    // MUST-FIX 3: edge-entity provenance projection is not supported in v1 (the
    // AQL positional pipeline never binds an edge as the row entity). An accessor
    // over an edge/traversal variable must REJECT with a structured error rather
    // than silently resolving the traversal-terminal node's provenance.
    let db = make_db();
    assert!(
        db.execute_aql("MATCH (a:Person)-[r:KNOWS]->(b:Person) RETURN r, source(r)")
            .is_err(),
        "edge-variable accessor must be rejected (node-entity-scoped in v1)"
    );
}

#[test]
fn as_of_projects_provenance_at_that_coordinate() {
    // The projected provenance reflects the version visible at the query's
    // bi-temporal coordinate, not the latest version. Recalling the superseded
    // v1 (confidence 0.50) requires anchoring BOTH dimensions before the update:
    // v1's valid interval is [t1, t2) and its transaction interval closed at the
    // update, so `(valid in [t1,t2), tx < update)` selects it, while
    // `(valid >= t2, tx = now)` selects the current v2 (0.95).
    use aletheiadb::core::temporal::Timestamp;
    use std::thread::sleep;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    // Wallclock microseconds since the Unix epoch -- the same scale the database
    // assigns transaction timestamps on.
    let now_micros = || -> i64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock after epoch")
            .as_micros() as i64
    };

    let db = AletheiaDB::new().expect("db");
    let t1: i64 = 1_000_000_000_000;
    let t2: i64 = 2_000_000_000_000;
    let t3: i64 = 3_000_000_000_000;
    let t_mid: i64 = 1_500_000_000_000; // strictly inside [t1, t2) -> selects v1

    let props = PropertyMapBuilder::new().insert("name", "alice").build();
    let node_id = db
        .create_node_with_options(
            "Person",
            props,
            WriteRequestOptions::new()
                .with_valid_from(Timestamp::from(t1))
                .with_provenance(prov(Some("hr-system"), Some(0.50), None)),
        )
        .expect("create v1");

    // Capture a transaction-time coordinate strictly between the create and the
    // update (small sleeps guarantee microsecond separation on the shared clock).
    sleep(Duration::from_millis(20));
    let tx_between: i64 = now_micros();
    sleep(Duration::from_millis(20));

    let props2 = PropertyMapBuilder::new().insert("name", "alice").build();
    db.update_node_with_options(
        node_id,
        props2,
        WriteRequestOptions::new()
            .with_valid_from(Timestamp::from(t2))
            .with_provenance(prov(Some("hr-system"), Some(0.95), None)),
    )
    .expect("update v2");

    // Before the update (valid in [t1,t2), tx before the update): confidence was
    // 0.50. AQL `AS OF <valid>, <tx>` anchors both dimensions.
    let before = rows(
        &db,
        &format!("AS OF {t_mid}, {tx_between} MATCH (n:Person) RETURN n, confidence(n)"),
    );
    let v = before
        .iter()
        .find_map(|r| col(r, "confidence(n)").cloned())
        .expect("a confidence column at the pre-update coordinate");
    match v {
        PropertyValue::Float(f) => {
            assert!((f - 0.50).abs() < 1e-9, "pre-update expected 0.50, got {f}")
        }
        other => panic!("expected Float, got {other:?}"),
    }

    // Current coordinate (valid >= t2, tx = now): confidence is 0.95.
    let after = rows(
        &db,
        &format!("AS OF {t3} MATCH (n:Person) RETURN n, confidence(n)"),
    );
    let v = after
        .iter()
        .find_map(|r| col(r, "confidence(n)").cloned())
        .expect("a confidence column at the current coordinate");
    match v {
        PropertyValue::Float(f) => {
            assert!((f - 0.95).abs() < 1e-9, "current expected 0.95, got {f}")
        }
        other => panic!("expected Float, got {other:?}"),
    }
}

#[test]
fn invalid_accessor_argument_is_error_not_silent() {
    let db = make_db();
    // A provenance-named accessor with a malformed argument must be a structured
    // error, never a silently dropped projection.
    for q in [
        "MATCH (n:Person) RETURN source(n.foo)",
        "MATCH (n:Person) RETURN confidence(n, m)",
        "MATCH (n:Person) RETURN source()",
    ] {
        assert!(db.execute_aql(q).is_err(), "expected error for: {q}");
    }
}

#[test]
fn plain_return_entity_unaffected() {
    // Regression: a RETURN with no provenance accessor is byte-identical to
    // before (plain entity rows, no columns/bindings).
    let db = make_db();
    let rows = rows(&db, "MATCH (n:Person) RETURN n");
    assert_eq!(rows.len(), 4);
    for r in &rows {
        assert!(matches!(r.entity, EntityResult::Node(_)));
        assert!(r.columns.is_none());
        assert!(r.bindings.is_none());
    }
}
