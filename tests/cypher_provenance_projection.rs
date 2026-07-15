//! Integration tests for Cypher provenance accessors in `RETURN` / `ORDER BY`
//! projection (Issue #3354, projection half). Mirrors the AQL suite
//! (`tests/aql_provenance_projection.rs`) for the Cypher surface, and adds a
//! cross-surface parity gate.

#![cfg(feature = "cypher")]

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

fn rows(db: &AletheiaDB, cypher: &str) -> Vec<QueryRow> {
    db.execute_cypher(cypher)
        .expect("query executes")
        .collect_all()
        .expect("collect rows")
}

fn err_msg(db: &AletheiaDB, cypher: &str) -> String {
    match db.execute_cypher(cypher) {
        Ok(_) => panic!("expected error for query: {cypher}"),
        Err(e) => e.to_string(),
    }
}

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

fn col<'a>(row: &'a QueryRow, name: &str) -> Option<&'a PropertyValue> {
    row.columns
        .as_ref()
        .and_then(|cols| cols.iter().find(|(k, _)| k == name).map(|(_, v)| v))
}

#[test]
fn return_entity_and_source_projects_column_and_keeps_entity() {
    let db = make_db();
    let rows = rows(&db, "MATCH (n:Person) RETURN n, source(n)");
    assert_eq!(rows.len(), 4);
    let alice = rows
        .iter()
        .find(|r| row_name(r).as_deref() == Some("String(\"alice\")"))
        .expect("alice row present with observable entity");
    assert_eq!(
        col(alice, "source(n)"),
        Some(&PropertyValue::String("hr-system".into()))
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
fn order_by_confidence_desc_sorts_by_projected_provenance() {
    let db = make_db();
    let rows = rows(
        &db,
        "MATCH (n:Person) RETURN n, confidence(n) ORDER BY confidence(n) DESC",
    );
    let order: Vec<String> = rows.iter().filter_map(row_name).collect();
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
    let rows = rows(&db, "MATCH (n:Person) RETURN n ORDER BY confidence(n)");
    let order: Vec<String> = rows.iter().filter_map(row_name).collect();
    assert_eq!(
        order,
        vec![
            "String(\"bob\")",
            "String(\"carol\")",
            "String(\"alice\")",
            "String(\"dave\")",
        ]
    );
    for r in &rows {
        assert!(
            col(r, "confidence(n)").is_none(),
            "ORDER BY-only accessor must not surface as a column"
        );
    }
}

#[test]
fn source_only_projection_without_entity_is_columns_row() {
    let db = make_db();
    let rows = rows(&db, "MATCH (n:Person) RETURN source(n)");
    assert_eq!(rows.len(), 4);
    let sources: Vec<PropertyValue> = rows
        .iter()
        .filter_map(|r| col(r, "source(n)").cloned())
        .collect();
    assert!(sources.contains(&PropertyValue::String("hr-system".into())));
    assert!(sources.contains(&PropertyValue::Null));
}

#[test]
fn as_of_projects_provenance_at_that_coordinate() {
    // Recalling the superseded v1 requires anchoring BOTH dimensions before the
    // update (see the AQL suite for the bitemporal rationale).
    use aletheiadb::core::temporal::Timestamp;
    use std::thread::sleep;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

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

    let before = rows(
        &db,
        &format!(
            "MATCH (n:Person) AS OF VALID_TIME '{t_mid}' AS OF SYSTEM_TIME '{tx_between}' \
             RETURN n, confidence(n)"
        ),
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

    let after = rows(
        &db,
        &format!("MATCH (n:Person) AS OF VALID_TIME '{t3}' RETURN n, confidence(n)"),
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
    for q in [
        "MATCH (n:Person) RETURN source(n.foo)",
        "MATCH (n:Person) RETURN confidence(n, m)",
    ] {
        assert!(db.execute_cypher(q).is_err(), "expected error for: {q}");
    }
    // A structured error, not a silent empty result.
    let msg = err_msg(&db, "MATCH (n:Person) RETURN source(n.foo)");
    assert!(!msg.is_empty());
}

#[test]
fn plain_return_entity_unaffected() {
    let db = make_db();
    let rows = rows(&db, "MATCH (n:Person) RETURN n");
    assert_eq!(rows.len(), 4);
    for r in &rows {
        assert!(matches!(r.entity, EntityResult::Node(_)));
        assert!(r.columns.is_none());
        assert!(r.bindings.is_none());
    }
}

#[test]
fn distinct_projection_unaffected() {
    // Regression: RETURN DISTINCT still dedupes by entity id; projection rides
    // on top without changing that.
    let db = make_db();
    let rows = rows(&db, "MATCH (n:Person) RETURN DISTINCT n, source(n)");
    assert_eq!(rows.len(), 4, "four distinct people");
}

/// Cross-surface parity: for the same fixture and equivalent queries, AQL and
/// Cypher must project the same provenance columns (criterion 3).
#[test]
fn cypher_aql_parity_over_projection_queries() {
    let db = make_db();
    // (cypher, aql, column name)
    let cases = [
        (
            "MATCH (n:Person) RETURN source(n)",
            "MATCH (n:Person) RETURN source(n)",
            "source(n)",
        ),
        (
            "MATCH (n:Person) RETURN confidence(n)",
            "MATCH (n:Person) RETURN confidence(n)",
            "confidence(n)",
        ),
        (
            "MATCH (n:Person) RETURN reason(n)",
            "MATCH (n:Person) RETURN reason(n)",
            "reason(n)",
        ),
    ];
    for (cypher, aql, colname) in cases {
        let mut via_cypher: Vec<String> = rows(&db, cypher)
            .iter()
            .map(|r| format!("{:?}", col(r, colname)))
            .collect();
        let mut via_aql: Vec<String> = db
            .execute_aql(aql)
            .expect("aql executes")
            .collect_all()
            .expect("collect")
            .iter()
            .map(|r| format!("{:?}", col(r, colname)))
            .collect();
        via_cypher.sort();
        via_aql.sort();
        assert_eq!(via_cypher, via_aql, "parity mismatch for {cypher}");
    }
}
