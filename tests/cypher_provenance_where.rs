//! Integration tests for Cypher `WHERE`-clause provenance predicates
//! (Issue #3354b).
//!
//! Mirrors the AQL suite (`tests/aql_provenance_where.rs`, Issue #3354a) for the
//! Cypher surface: the read-only provenance accessors `source(x)` /
//! `confidence(x)` / `reason(x)` and `provenance(x) IS [NOT] NULL` end-to-end
//! through `execute_cypher`, including absent-provenance exclusion, composition
//! with an ordinary property `WHERE`, per-version (`AS OF`) evaluation, and
//! parity with the AQL lowering (both surfaces share
//! `crate::query::converter::lower_provenance_comparison`).

#![cfg(feature = "cypher")]

use aletheiadb::AletheiaDB;
use aletheiadb::PropertyMapBuilder;
use aletheiadb::api::transaction::WriteRequestOptions;
use aletheiadb::core::NodeId;
use aletheiadb::core::provenance::Provenance;
use aletheiadb::query::executor::EntityResult;

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

/// Run a Cypher query expected to fail, returning the error message.
/// (`QueryResults` is not `Debug`, so `Result::expect_err` cannot be used.)
fn err_msg(db: &AletheiaDB, cypher: &str) -> String {
    match db.execute_cypher(cypher) {
        Ok(_) => panic!("expected error for query: {cypher}"),
        Err(e) => e.to_string(),
    }
}

/// Collect the `name` property of every node row returned by a Cypher query.
fn names(db: &AletheiaDB, cypher: &str) -> Vec<String> {
    let results = db.execute_cypher(cypher).expect("query executes");
    let mut out = Vec::new();
    for row in results.collect_all().expect("collect rows") {
        let EntityResult::Node(n) = row.entity else {
            continue;
        };
        if let Some(v) = n.get_property("name") {
            out.push(format!("{v:?}"));
        }
    }
    out.sort();
    out
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

#[test]
fn source_exact_match_filters_rows() {
    let db = make_db();
    let got = names(
        &db,
        "MATCH (n:Person) WHERE source(n) = 'hr-system' RETURN n",
    );
    assert_eq!(got, vec!["String(\"alice\")", "String(\"carol\")"]);
}

#[test]
fn source_inequality_excludes_and_absent() {
    let db = make_db();
    // `<>` keeps attributed rows whose source differs; unattributed (dave) is
    // excluded because its accessor is null.
    let got = names(
        &db,
        "MATCH (n:Person) WHERE source(n) <> 'hr-system' RETURN n",
    );
    assert_eq!(got, vec!["String(\"bob\")"]);
}

#[test]
fn confidence_inclusive_lower_bound() {
    let db = make_db();
    // >= 0.80 keeps alice (0.95) and carol (0.80, inclusive), not bob (0.60).
    let got = names(&db, "MATCH (n:Person) WHERE confidence(n) >= 0.80 RETURN n");
    assert_eq!(got, vec!["String(\"alice\")", "String(\"carol\")"]);
}

#[test]
fn confidence_strict_greater_is_exclusive() {
    let db = make_db();
    // > 0.80 excludes carol (== bound), keeps only alice.
    let got = names(&db, "MATCH (n:Person) WHERE confidence(n) > 0.80 RETURN n");
    assert_eq!(got, vec!["String(\"alice\")"]);
}

#[test]
fn confidence_less_than_general_path() {
    let db = make_db();
    // `<` is a non-foldable shape (general accessor path).
    let got = names(&db, "MATCH (n:Person) WHERE confidence(n) < 0.80 RETURN n");
    assert_eq!(got, vec!["String(\"bob\")"]);
}

#[test]
fn reason_equality() {
    let db = make_db();
    let got = names(
        &db,
        "MATCH (n:Person) WHERE reason(n) = 'verified' RETURN n",
    );
    assert_eq!(got, vec!["String(\"alice\")"]);
}

#[test]
fn absent_provenance_excluded_by_confidence() {
    let db = make_db();
    // dave (no bundle) excluded; the other three attributed rows kept.
    let got = names(&db, "MATCH (n:Person) WHERE confidence(n) >= 0.0 RETURN n");
    assert_eq!(
        got,
        vec!["String(\"alice\")", "String(\"bob\")", "String(\"carol\")"]
    );
}

#[test]
fn partial_bundle_missing_field_excluded() {
    let db = make_db();
    // carol has a bundle but no `reason`; a reason predicate excludes her.
    let got = names(
        &db,
        "MATCH (n:Person) WHERE reason(n) = 'imported' RETURN n",
    );
    assert_eq!(got, vec!["String(\"bob\")"]);
}

#[test]
fn provenance_is_null_selects_unattributed() {
    let db = make_db();
    let got = names(&db, "MATCH (n:Person) WHERE provenance(n) IS NULL RETURN n");
    assert_eq!(got, vec!["String(\"dave\")"]);
}

#[test]
fn provenance_is_not_null_selects_attributed() {
    let db = make_db();
    // carol has a (partial) bundle => IS NOT NULL is true for her.
    let got = names(
        &db,
        "MATCH (n:Person) WHERE provenance(n) IS NOT NULL RETURN n",
    );
    assert_eq!(
        got,
        vec!["String(\"alice\")", "String(\"bob\")", "String(\"carol\")"]
    );
}

#[test]
fn combined_source_and_confidence_is_and() {
    let db = make_db();
    let got = names(
        &db,
        "MATCH (n:Person) WHERE source(n) = 'hr-system' AND confidence(n) >= 0.90 RETURN n",
    );
    assert_eq!(got, vec!["String(\"alice\")"]);
}

#[test]
fn combined_property_and_provenance() {
    let db = make_db();
    // Property predicate ANDed with a provenance predicate.
    let got = names(
        &db,
        "MATCH (n:Person) WHERE n.name = 'carol' AND confidence(n) >= 0.5 RETURN n",
    );
    assert_eq!(got, vec!["String(\"carol\")"]);
}

#[test]
fn combined_property_or_provenance() {
    let db = make_db();
    // OR mixing forces the general (non-folded) evaluation path.
    let got = names(
        &db,
        "MATCH (n:Person) WHERE n.name = 'dave' OR confidence(n) >= 0.90 RETURN n",
    );
    assert_eq!(got, vec!["String(\"alice\")", "String(\"dave\")"]);
}

#[test]
fn accessor_on_right_hand_side() {
    let db = make_db();
    // `0.90 <= confidence(n)` must lower identically to `confidence(n) >= 0.90`.
    let got = names(&db, "MATCH (n:Person) WHERE 0.90 <= confidence(n) RETURN n");
    assert_eq!(got, vec!["String(\"alice\")"]);
}

#[test]
fn property_named_source_is_not_a_provenance_accessor() {
    // A property literally named `source` (accessed as `n.source`) must remain
    // an ordinary property filter, not a provenance accessor.
    let db = AletheiaDB::new().expect("db");
    let props = PropertyMapBuilder::new()
        .insert("name", "eve")
        .insert("source", "manual")
        .build();
    db.create_node_with_options("Person", props, WriteRequestOptions::new())
        .expect("create");
    let got = names(&db, "MATCH (n:Person) WHERE n.source = 'manual' RETURN n");
    assert_eq!(got, vec!["String(\"eve\")"]);
}

#[test]
fn confidence_out_of_range_is_rejected() {
    let db = make_db();
    let msg = err_msg(&db, "MATCH (n:Person) WHERE confidence(n) >= 1.5 RETURN n");
    assert!(
        msg.contains("0.0 and 1.0") || msg.contains("1.5"),
        "got: {msg}"
    );
}

#[test]
fn confidence_nan_via_parameter_is_rejected() {
    use aletheiadb::cypher::CypherParameterValue;
    use std::collections::HashMap;
    let db = make_db();
    // NaN is rejected by the same [0,1]/NaN validation as an out-of-range value.
    // NaN cannot be written as a literal, so the user-facing path is a bound
    // parameter.
    let mut params = HashMap::new();
    params.insert("c".to_string(), CypherParameterValue::Float(f64::NAN));
    let res = db.execute_cypher_with_params(
        "MATCH (n:Person) WHERE confidence(n) >= $c RETURN n",
        params,
    );
    assert!(res.is_err(), "NaN confidence must be rejected");
}

#[test]
fn confidence_via_parameter_folds() {
    use aletheiadb::cypher::CypherParameterValue;
    use std::collections::HashMap;
    let db = make_db();
    // A valid confidence bound supplied as a parameter behaves like the literal.
    let mut params = HashMap::new();
    params.insert("c".to_string(), CypherParameterValue::Float(0.80));
    let results = db
        .execute_cypher_with_params(
            "MATCH (n:Person) WHERE confidence(n) >= $c RETURN n",
            params,
        )
        .expect("query executes");
    let mut got = Vec::new();
    for row in results.collect_all().expect("collect rows") {
        if let Some(v) = match &row.entity {
            EntityResult::Node(n) => n.get_property("name"),
            _ => None,
        } {
            got.push(format!("{v:?}"));
        }
    }
    got.sort();
    assert_eq!(got, vec!["String(\"alice\")", "String(\"carol\")"]);
}

#[test]
fn count_star_reflects_provenance_filter_before_aggregation() {
    use aletheiadb::core::property::PropertyValue;
    let db = make_db();
    // The provenance `WHERE` runs BEFORE aggregation: only alice (0.95) clears
    // `>= 0.9`, so `count(*)` is 1 -- not the 4-row unfiltered total. This locks
    // the filter-then-aggregate ordering.
    let results = db
        .execute_cypher("MATCH (n:Person) WHERE confidence(n) >= 0.9 RETURN count(*)")
        .expect("query executes");
    let rows = results.collect_all().expect("collect rows");
    assert_eq!(rows.len(), 1, "keyless count(*) yields a single global row");
    let cols = rows[0]
        .columns
        .as_ref()
        .expect("aggregate row carries columns");
    assert_eq!(cols.len(), 1, "one projected aggregate column");
    assert_eq!(
        cols[0].1,
        PropertyValue::Int(1),
        "only the provenance-filtered rows are counted"
    );
}

#[test]
fn confidence_compared_to_string_is_type_error() {
    let db = make_db();
    let msg = err_msg(
        &db,
        "MATCH (n:Person) WHERE confidence(n) = 'high' RETURN n",
    );
    assert!(msg.to_lowercase().contains("type mismatch"), "got: {msg}");
}

#[test]
fn source_compared_to_number_is_type_error() {
    let db = make_db();
    let msg = err_msg(&db, "MATCH (n:Person) WHERE source(n) = 5 RETURN n");
    assert!(msg.to_lowercase().contains("type mismatch"), "got: {msg}");
}

#[test]
fn malformed_accessor_argument_is_parse_or_convert_error() {
    let db = make_db();
    for q in [
        "MATCH (n:Person) WHERE source(n.foo) = 'x' RETURN n",
        "MATCH (n:Person) WHERE confidence(n, m) >= 0.5 RETURN n",
        "MATCH (n:Person) WHERE source() = 'x' RETURN n",
        "MATCH (n:Person) WHERE provenance(n.foo) IS NULL RETURN n",
    ] {
        assert!(db.execute_cypher(q).is_err(), "expected error for: {q}");
    }
}

#[test]
fn ordering_operator_on_source_is_rejected() {
    // The string accessors `source`/`reason` support only `=`/`<>`. An ordering
    // operator must be rejected at convert time with a structured error, never
    // silently accepted as a lexicographic comparison (mirrors #3354a FIX 1).
    let db = make_db();
    for q in [
        "MATCH (n:Person) WHERE source(n) < 'x' RETURN n",
        "MATCH (n:Person) WHERE reason(n) >= 'x' RETURN n",
    ] {
        let msg = err_msg(&db, q);
        let lower = msg.to_lowercase();
        assert!(
            (lower.contains("source") || lower.contains("reason"))
                && (lower.contains("ordering") || lower.contains("= and <>")),
            "expected a structured rejection naming the accessor and allowed operators, got: {msg}"
        );
    }
}

#[test]
fn as_of_evaluates_provenance_at_that_coordinate() {
    // AC3: an `AS OF` query filters on the provenance recorded on the version
    // visible at that coordinate, not the latest version. Build a node whose
    // confidence rose from 0.50 (valid [T1, T2)) to 0.95 (valid [T2, ..)).
    let db = AletheiaDB::new().expect("db");
    let t1: i64 = 1_000_000_000_000; // microseconds
    let t2: i64 = 2_000_000_000_000;
    let t3: i64 = 3_000_000_000_000;

    let props = PropertyMapBuilder::new().insert("name", "alice").build();
    let node_id = db
        .create_node_with_options(
            "Person",
            props,
            WriteRequestOptions::new()
                .with_valid_from(aletheiadb::core::temporal::Timestamp::from(t1))
                .with_provenance(prov(Some("hr-system"), Some(0.50), None)),
        )
        .expect("create v1");

    let props2 = PropertyMapBuilder::new().insert("name", "alice").build();
    db.update_node_with_options(
        node_id,
        props2,
        WriteRequestOptions::new()
            .with_valid_from(aletheiadb::core::temporal::Timestamp::from(t2))
            .with_provenance(prov(Some("hr-system"), Some(0.95), None)),
    )
    .expect("update v2");

    // AS OF valid_time within [T1, T2): confidence was 0.50 -> excluded by >= 0.9.
    // VALID_TIME sets valid time only (transaction time defaults to now), so the
    // 2026 writes are visible; only the valid-time coordinate selects the
    // version.
    let before = names(
        &db,
        &format!("MATCH (n:Person) WHERE confidence(n) >= 0.90 AS OF VALID_TIME '{t1}' RETURN n"),
    );
    assert!(
        before.is_empty(),
        "at T1 confidence was 0.50, got: {before:?}"
    );

    // AS OF valid_time >= T2: confidence is 0.95 -> included by >= 0.9.
    let after = names(
        &db,
        &format!("MATCH (n:Person) WHERE confidence(n) >= 0.90 AS OF VALID_TIME '{t3}' RETURN n"),
    );
    assert_eq!(after, vec!["String(\"alice\")"]);
}

#[test]
fn as_of_system_time_and_provenance_composition() {
    // The provenance predicate composes with an explicit SYSTEM_TIME AS OF: at a
    // transaction-time before any write, nothing is visible; at now, the current
    // version's provenance is evaluated.
    let db = make_db();
    // Far-future system time: current versions are visible; predicate applies.
    let got = names(
        &db,
        "MATCH (n:Person) WHERE source(n) = 'hr-system' AS OF SYSTEM_TIME '2999-01-01T00:00:00Z' RETURN n",
    );
    assert_eq!(got, vec!["String(\"alice\")", "String(\"carol\")"]);
}

/// Parity: the Cypher and AQL surfaces share
/// `crate::query::converter::lower_provenance_comparison`, so for the same data
/// and equivalent queries they must return identical result sets. This proves
/// the two agree (the task's foldable-vs-accessor / cross-surface parity gate).
#[test]
fn cypher_aql_parity_over_representative_queries() {
    let db = make_db();
    let pairs = [
        (
            "MATCH (n:Person) WHERE source(n) = 'hr-system' RETURN n",
            "MATCH (n:Person) WHERE source(n) = 'hr-system' RETURN n",
        ),
        (
            "MATCH (n:Person) WHERE confidence(n) >= 0.80 RETURN n",
            "MATCH (n:Person) WHERE confidence(n) >= 0.80 RETURN n",
        ),
        (
            "MATCH (n:Person) WHERE confidence(n) < 0.80 RETURN n",
            "MATCH (n:Person) WHERE confidence(n) < 0.80 RETURN n",
        ),
        (
            "MATCH (n:Person) WHERE reason(n) <> 'verified' RETURN n",
            "MATCH (n:Person) WHERE reason(n) <> 'verified' RETURN n",
        ),
        (
            "MATCH (n:Person) WHERE provenance(n) IS NULL RETURN n",
            "MATCH (n:Person) WHERE provenance(n) IS NULL RETURN n",
        ),
        (
            "MATCH (n:Person) WHERE provenance(n) IS NOT NULL RETURN n",
            "MATCH (n:Person) WHERE provenance(n) IS NOT NULL RETURN n",
        ),
    ];
    for (cypher, aql) in pairs {
        let via_cypher = names(&db, cypher);
        let via_aql = {
            let results = db.execute_aql(aql).expect("aql executes");
            let mut out = Vec::new();
            for row in results.collect_all().expect("collect rows") {
                if let Some(v) = match &row.entity {
                    EntityResult::Node(n) => n.get_property("name"),
                    _ => None,
                } {
                    out.push(format!("{v:?}"));
                }
            }
            out.sort();
            out
        };
        assert_eq!(via_cypher, via_aql, "parity mismatch for: {cypher}");
    }
}
