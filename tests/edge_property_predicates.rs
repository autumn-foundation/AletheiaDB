//! Edge-property WHERE filtering + ORDER BY in AQL and Cypher (Issue #3622,
//! completing the SQL lane #3648 for the graph query languages).
//!
//! Approach A: `r.<userprop>` resolves against the edge's properties; reserved
//! fields (`type`/`label`/`source`/`target`/`id`) resolve structurally
//! (shadowing user props), mirroring the SQL `edge_structural_value` semantics.
//! Both languages route edge-variable leaves through the shared openCypher edge
//! semantics, so SQL == AQL == Cypher: `Eq` on an absent property excludes,
//! `Ne` on an absent property includes, and ORDER BY places nulls last for ASC /
//! first for DESC. Unsupported forms return a structured error, never a silent
//! 0-row / ignored clause. Edge predicates respect bi-temporal AS-OF
//! coordinates (the edge is reconstructed at the query's coordinate).
//!
//! Both languages run against ONE shared graph helper, and a parity harness
//! asserts the AQL and Cypher forms of each supported query return an identical
//! result set.
//!
//! ## Implementation notes / documented asymmetries
//!
//! * **AQL** attaches the traversed edge to a dedicated `QueryRow::edge` side
//!   channel (populated by `TraversalIterator`, reconstructed at the query
//!   coordinate); the converter wraps a WHERE leaf on the single-hop
//!   relationship variable in `Predicate::EdgeScoped` and an ORDER BY key in
//!   `SortKey::EdgeProperty`.
//! * **Cypher** multi-variable queries reject temporal `AS OF` uniformly (both
//!   node and edge) via the multi_pattern house rule, so an edge-variable WHERE
//!   combined with AS OF is a *structured error* (never a silently-wrong row) --
//!   `cypher_bitemporal_edge_where_as_of_is_structured_error` pins this. Real
//!   AS-OF edge predicates are therefore an AQL capability; the bi-temporal
//!   assertions below run through AQL.
//! * The multikey ORDER BY test returns the full node (`RETURN b`) rather than a
//!   narrowed `RETURN b.name`, because AQL's `Project` narrows the node entity
//!   before `Sort`, so a NODE sort key projected away resolves to null -- a
//!   pre-existing AQL limitation orthogonal to the edge lane. The EDGE sort key
//!   needs no such accommodation because it rides the preserved `edge` channel.
//! * Bi-temporal tests use single-version edges in distinct valid windows: the
//!   engine does not reconstruct a *superseded* valid-time edge version (an
//!   engine-level gap outside this lane), so "AS OF sees a value changed by a
//!   later valid-time update" is deliberately not asserted (see the SCOPE NOTE).
#![cfg(feature = "cypher")]

use aletheiadb::AletheiaDB;
use aletheiadb::PropertyMapBuilder;
use aletheiadb::api::transaction::WriteRequestOptions;
use aletheiadb::core::error::{Error, QueryError};
use aletheiadb::core::property::PropertyValue;
use aletheiadb::core::provenance::Provenance;
use aletheiadb::core::temporal::Timestamp;
use aletheiadb::query::executor::{EntityResult, QueryRow};
use aletheiadb::query::temporal_window::parse_boundary_micros;

// ---------------------------------------------------------------------------
// Fixtures & helpers
// ---------------------------------------------------------------------------

/// Node ids of the standard graph (see [`std_graph`]). Only `alice`/`carol` are
/// referenced (reserved `r.source`/`r.target` tests); the rest document the
/// fixture.
#[allow(dead_code)]
struct Ids {
    alice: u64,
    bob: u64,
    carol: u64,
    dave: u64,
    erin: u64,
}

/// The standard non-temporal graph used by the WHERE / ORDER BY cases.
///
/// Nodes (`Person`): Alice(age 30), Bob(25), Carol(17), Dave(40), Erin(22).
/// Edges out of Alice (KNOWS created in this order: Bob, Carol, Dave, Erin, so
/// the traversal order is Bob, Carol, Dave, Erin):
///
/// | target | KNOWS.since | KNOWS.role |
/// |--------|-------------|------------|
/// | Bob    | 2021        | engineer   |
/// | Carol  | 2019        | manager    |
/// | Dave   | 2022        | engineer   |
/// | Erin   | (absent)    | intern     |
///
/// Plus one different-type edge Alice-FOLLOWS{since:2020, role:'peer'}->Bob to
/// exercise the reserved `r.type` field. The `since` order deliberately does
/// NOT follow creation order, so ORDER BY over `since` reorders vs the traversal
/// order (which makes a broken/no-op sort observably wrong).
fn std_graph() -> (AletheiaDB, Ids) {
    let db = AletheiaDB::new().expect("db");
    let person = |db: &AletheiaDB, name: &str, age: i64| {
        db.create_node(
            "Person",
            PropertyMapBuilder::new()
                .insert("name", name)
                .insert("age", age)
                .build(),
        )
        .expect("create person")
    };
    let alice = person(&db, "Alice", 30);
    let bob = person(&db, "Bob", 25);
    let carol = person(&db, "Carol", 17);
    let dave = person(&db, "Dave", 40);
    let erin = person(&db, "Erin", 22);

    let knows = |db: &AletheiaDB, tgt, props: PropertyMapBuilder| {
        db.create_edge(alice, tgt, "KNOWS", props.build())
            .expect("create KNOWS");
    };
    knows(
        &db,
        bob,
        PropertyMapBuilder::new()
            .insert("since", 2021)
            .insert("role", "engineer"),
    );
    knows(
        &db,
        carol,
        PropertyMapBuilder::new()
            .insert("since", 2019)
            .insert("role", "manager"),
    );
    knows(
        &db,
        dave,
        PropertyMapBuilder::new()
            .insert("since", 2022)
            .insert("role", "engineer"),
    );
    // Erin: KNOWS edge with NO `since` (absent-property cases).
    knows(
        &db,
        erin,
        PropertyMapBuilder::new().insert("role", "intern"),
    );

    // Different edge type for the reserved `r.type` field.
    db.create_edge(
        alice,
        bob,
        "FOLLOWS",
        PropertyMapBuilder::new()
            .insert("since", 2020)
            .insert("role", "peer")
            .build(),
    )
    .expect("create FOLLOWS");

    let ids = Ids {
        alice: alice.as_u64(),
        bob: bob.as_u64(),
        carol: carol.as_u64(),
        dave: dave.as_u64(),
        erin: erin.as_u64(),
    };
    (db, ids)
}

/// Microseconds since epoch for an RFC 3339 instant.
fn micros(rfc: &str) -> i64 {
    parse_boundary_micros(rfc).expect("valid rfc3339")
}

/// A `Timestamp` at an RFC 3339 instant.
fn ts(rfc: &str) -> Timestamp {
    Timestamp::from(micros(rfc))
}

/// Extract a node's `name` from a result row, robust to every row shape the
/// pipeline produces: a scalar `name` column (`RETURN b.name AS name`, the
/// multi-pattern/aggregate path), a single Node entity (`RETURN b` on the
/// single-entity path), or a Node bound in `bindings` (`RETURN b` on the
/// multi-pattern path). The row ORDER is always preserved by the callers.
fn row_name(row: &QueryRow) -> Option<String> {
    let as_str = |v: &PropertyValue| match v {
        PropertyValue::String(s) => Some(s.to_string()),
        _ => None,
    };
    // 1. Scalar projection column named "name".
    if let Some(cols) = row.columns.as_ref()
        && let Some((_, v)) = cols.iter().find(|(k, _)| k == "name")
        && let Some(s) = as_str(v)
    {
        return Some(s);
    }
    // 2. Single Node entity.
    if let Some(n) = row.entity.as_node()
        && let Some(v) = n.get_property("name")
    {
        return as_str(v);
    }
    // 3. A Node bound in a multi-pattern binding.
    if let Some(bindings) = row.bindings.as_ref() {
        for (_, e) in bindings {
            if let EntityResult::Node(n) = e
                && let Some(v) = n.get_property("name")
            {
                return as_str(v);
            }
        }
    }
    None
}

/// Run an AQL query and return the ordered list of projected `name` values.
/// A parse/convert/execute error is surfaced (with the query) so RED evidence
/// distinguishes "wrong rows" from "failed to execute".
fn aql_names(db: &AletheiaDB, q: &str) -> Vec<String> {
    match db.execute_aql(q).and_then(|r| r.collect_all()) {
        Ok(rows) => rows.iter().filter_map(row_name).collect(),
        Err(e) => panic!("AQL failed to execute [{q}]: {e:?}"),
    }
}

/// Run a Cypher query and return the ordered list of projected `name` values.
fn cypher_names(db: &AletheiaDB, q: &str) -> Vec<String> {
    match db.execute_cypher(q).and_then(|r| r.collect_all()) {
        Ok(rows) => rows.iter().filter_map(row_name).collect(),
        Err(e) => panic!("Cypher failed to execute [{q}]: {e:?}"),
    }
}

/// Sort a name list (for unordered / set comparison of WHERE results).
fn sorted(mut v: Vec<String>) -> Vec<String> {
    v.sort();
    v
}

fn owned(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

/// True if an AQL query is a structured error, at parse/convert time OR at
/// collection time. Used by the "never silently wrong" cases: a form we must
/// reject must NOT return Ok rows.
fn aql_is_error(db: &AletheiaDB, q: &str) -> bool {
    match db.execute_aql(q) {
        Err(_) => true,
        Ok(results) => results.collect_all().is_err(),
    }
}

/// True if a Cypher query is a structured error (parse/convert or collect).
fn cypher_is_error(db: &AletheiaDB, q: &str) -> bool {
    match db.execute_cypher(q) {
        Err(_) => true,
        Ok(results) => results.collect_all().is_err(),
    }
}

/// Full result rows of an AQL query (for row-count / edge-channel inspection).
fn aql_rows(db: &AletheiaDB, q: &str) -> Vec<QueryRow> {
    match db.execute_aql(q).and_then(|r| r.collect_all()) {
        Ok(rows) => rows,
        Err(e) => panic!("AQL failed to execute [{q}]: {e:?}"),
    }
}

/// Full result rows of a Cypher query.
fn cypher_rows(db: &AletheiaDB, q: &str) -> Vec<QueryRow> {
    match db.execute_cypher(q).and_then(|r| r.collect_all()) {
        Ok(rows) => rows,
        Err(e) => panic!("Cypher failed to execute [{q}]: {e:?}"),
    }
}

/// The `since` value on an AQL row's traversed-edge side channel (Issue #3622),
/// or `None` if the row carries no edge / the edge has no integer `since`.
fn aql_row_edge_since(row: &QueryRow) -> Option<i64> {
    match row.edge.as_ref()?.get_property("since") {
        Some(PropertyValue::Int(v)) => Some(*v),
        _ => None,
    }
}

/// The structured error of an AQL query (parse/convert/execute), or `None` if it
/// succeeded.
fn aql_error(db: &AletheiaDB, q: &str) -> Option<Error> {
    db.execute_aql(q).and_then(|r| r.collect_all()).err()
}

/// Whether an error is the structured "single, fixed-length hop" rejection
/// emitted for an edge-variable predicate/ORDER BY on an unsupported
/// (multi-hop / variable-length) pattern (Issue #3622 F7).
fn is_unsupported_hop_error(e: &Error) -> bool {
    matches!(
        e,
        Error::Query(QueryError::UnsupportedFeature { feature })
            if feature.contains("single, fixed-length hop")
    )
}

/// Parity harness with row-count + no-drop rigor (Issue #3622 F12): asserts the
/// AQL and Cypher forms return the same number of rows, that every row yields a
/// name (no silently-dropped nameless row), and that the sorted name multisets
/// are identical AND equal `expected`.
fn assert_where_parity(db: &AletheiaDB, aql: &str, cypher: &str, expected: &[&str]) {
    let a_rows = aql_rows(db, aql);
    let c_rows = cypher_rows(db, cypher);
    let a: Vec<String> = a_rows.iter().filter_map(row_name).collect();
    let c: Vec<String> = c_rows.iter().filter_map(row_name).collect();
    assert_eq!(
        a.len(),
        a_rows.len(),
        "AQL dropped a nameless row for [{aql}]"
    );
    assert_eq!(
        c.len(),
        c_rows.len(),
        "Cypher dropped a nameless row for [{cypher}]"
    );
    assert_eq!(
        a_rows.len(),
        c_rows.len(),
        "row-count parity AQL [{aql}] vs Cypher [{cypher}]"
    );
    assert_eq!(sorted(a.clone()), sorted(c), "name multiset parity");
    assert_eq!(sorted(a), owned(expected), "matches expected");
}

/// The KNOWS-only start pattern shared by most WHERE / ORDER BY cases.
const KNOWS: &str = "MATCH (a:Person {name: 'Alice'})-[r:KNOWS]->(b:Person)";
/// Unlabeled relationship pattern (for reserved `r.type`).
const ANYREL: &str = "MATCH (a:Person {name: 'Alice'})-[r]->(b:Person)";

// ===========================================================================
// WHERE cases -- each as an AQL test, a Cypher test, and a parity assertion.
// ===========================================================================

// --- eq (r.since = 2019); also proves absent-prop Eq EXCLUDES Erin ----------

#[test]
fn aql_where_eq() {
    let (db, _) = std_graph();
    let got = aql_names(
        &db,
        &format!("{KNOWS} WHERE r.since = 2019 RETURN b.name AS name"),
    );
    assert_eq!(sorted(got), owned(&["Carol"]));
}

#[test]
fn cypher_where_eq() {
    let (db, _) = std_graph();
    let got = cypher_names(
        &db,
        &format!("{KNOWS} WHERE r.since = 2019 RETURN b.name AS name"),
    );
    assert_eq!(sorted(got), owned(&["Carol"]));
}

#[test]
fn parity_where_eq() {
    let (db, _) = std_graph();
    let q = format!("{KNOWS} WHERE r.since = 2019 RETURN b.name AS name");
    assert_eq!(sorted(aql_names(&db, &q)), sorted(cypher_names(&db, &q)));
}

// --- range (r.since > 2020) -------------------------------------------------

#[test]
fn aql_where_range() {
    let (db, _) = std_graph();
    let got = aql_names(
        &db,
        &format!("{KNOWS} WHERE r.since > 2020 RETURN b.name AS name"),
    );
    assert_eq!(sorted(got), owned(&["Bob", "Dave"]));
}

#[test]
fn cypher_where_range() {
    let (db, _) = std_graph();
    let got = cypher_names(
        &db,
        &format!("{KNOWS} WHERE r.since > 2020 RETURN b.name AS name"),
    );
    assert_eq!(sorted(got), owned(&["Bob", "Dave"]));
}

#[test]
fn parity_where_range() {
    let (db, _) = std_graph();
    let q = format!("{KNOWS} WHERE r.since > 2020 RETURN b.name AS name");
    assert_eq!(sorted(aql_names(&db, &q)), sorted(cypher_names(&db, &q)));
}

// --- absent-prop Ne INCLUDES (r.since <> 2019 keeps Erin, whose edge has no
//     `since`), while Bob(2019... no, 2021) etc. openCypher: Ne on absent -> true.

#[test]
fn aql_where_ne_absent_includes() {
    let (db, _) = std_graph();
    let got = aql_names(
        &db,
        &format!("{KNOWS} WHERE r.since <> 2019 RETURN b.name AS name"),
    );
    // Carol(2019) excluded; Bob(2021), Dave(2022) differ; Erin(absent) INCLUDED.
    assert_eq!(sorted(got), owned(&["Bob", "Dave", "Erin"]));
}

#[test]
fn cypher_where_ne_absent_includes() {
    let (db, _) = std_graph();
    let got = cypher_names(
        &db,
        &format!("{KNOWS} WHERE r.since <> 2019 RETURN b.name AS name"),
    );
    assert_eq!(sorted(got), owned(&["Bob", "Dave", "Erin"]));
}

#[test]
fn parity_where_ne_absent_includes() {
    let (db, _) = std_graph();
    let q = format!("{KNOWS} WHERE r.since <> 2019 RETURN b.name AS name");
    assert_eq!(sorted(aql_names(&db, &q)), sorted(cypher_names(&db, &q)));
}

// --- reserved r.type = 'KNOWS' (must return the KNOWS edges, NOT 0 rows) -----

#[test]
fn aql_where_reserved_type() {
    let (db, _) = std_graph();
    let got = aql_names(
        &db,
        &format!("{ANYREL} WHERE r.type = 'KNOWS' RETURN b.name AS name"),
    );
    assert_eq!(sorted(got), owned(&["Bob", "Carol", "Dave", "Erin"]));
}

#[test]
fn cypher_where_reserved_type() {
    let (db, _) = std_graph();
    let got = cypher_names(
        &db,
        &format!("{ANYREL} WHERE r.type = 'KNOWS' RETURN b.name AS name"),
    );
    assert_eq!(sorted(got), owned(&["Bob", "Carol", "Dave", "Erin"]));
}

#[test]
fn parity_where_reserved_type() {
    let (db, _) = std_graph();
    let q = format!("{ANYREL} WHERE r.type = 'KNOWS' RETURN b.name AS name");
    assert_eq!(sorted(aql_names(&db, &q)), sorted(cypher_names(&db, &q)));
}

// --- reserved r.source / r.target = endpoint id -----------------------------

#[test]
fn aql_where_reserved_source() {
    let (db, ids) = std_graph();
    let got = aql_names(
        &db,
        &format!(
            "{KNOWS} WHERE r.source = {} RETURN b.name AS name",
            ids.alice
        ),
    );
    assert_eq!(sorted(got), owned(&["Bob", "Carol", "Dave", "Erin"]));
}

#[test]
fn cypher_where_reserved_source() {
    let (db, ids) = std_graph();
    let got = cypher_names(
        &db,
        &format!(
            "{KNOWS} WHERE r.source = {} RETURN b.name AS name",
            ids.alice
        ),
    );
    assert_eq!(sorted(got), owned(&["Bob", "Carol", "Dave", "Erin"]));
}

#[test]
fn aql_where_reserved_target() {
    let (db, ids) = std_graph();
    let got = aql_names(
        &db,
        &format!(
            "{KNOWS} WHERE r.target = {} RETURN b.name AS name",
            ids.carol
        ),
    );
    assert_eq!(sorted(got), owned(&["Carol"]));
}

#[test]
fn cypher_where_reserved_target() {
    let (db, ids) = std_graph();
    let got = cypher_names(
        &db,
        &format!(
            "{KNOWS} WHERE r.target = {} RETURN b.name AS name",
            ids.carol
        ),
    );
    assert_eq!(sorted(got), owned(&["Carol"]));
}

#[test]
fn parity_where_reserved_target() {
    let (db, ids) = std_graph();
    let q = format!(
        "{KNOWS} WHERE r.target = {} RETURN b.name AS name",
        ids.carol
    );
    assert_eq!(sorted(aql_names(&db, &q)), sorted(cypher_names(&db, &q)));
}

// --- edge-prop AND node-prop: each leaf must hit the right entity -----------
// Dave passes the EDGE leaf (since 2022 > 2020) but fails the NODE leaf
// (age 40 not < 30); Carol passes the node leaf (17 < 30) but fails the edge
// leaf (2019 not > 2020). Only Bob (2021 > 2020 AND 25 < 30) survives.

#[test]
fn aql_where_edge_and_node() {
    let (db, _) = std_graph();
    let got = aql_names(
        &db,
        &format!("{KNOWS} WHERE r.since > 2020 AND b.age < 30 RETURN b.name AS name"),
    );
    assert_eq!(sorted(got), owned(&["Bob"]));
}

#[test]
fn cypher_where_edge_and_node() {
    let (db, _) = std_graph();
    let got = cypher_names(
        &db,
        &format!("{KNOWS} WHERE r.since > 2020 AND b.age < 30 RETURN b.name AS name"),
    );
    assert_eq!(sorted(got), owned(&["Bob"]));
}

#[test]
fn parity_where_edge_and_node() {
    let (db, _) = std_graph();
    let q = format!("{KNOWS} WHERE r.since > 2020 AND b.age < 30 RETURN b.name AS name");
    assert_eq!(sorted(aql_names(&db, &q)), sorted(cypher_names(&db, &q)));
}

// --- OR over edge props -----------------------------------------------------

#[test]
fn aql_where_or() {
    let (db, _) = std_graph();
    let got = aql_names(
        &db,
        &format!("{KNOWS} WHERE r.since = 2021 OR r.role = 'manager' RETURN b.name AS name"),
    );
    assert_eq!(sorted(got), owned(&["Bob", "Carol"]));
}

#[test]
fn cypher_where_or() {
    let (db, _) = std_graph();
    let got = cypher_names(
        &db,
        &format!("{KNOWS} WHERE r.since = 2021 OR r.role = 'manager' RETURN b.name AS name"),
    );
    assert_eq!(sorted(got), owned(&["Bob", "Carol"]));
}

#[test]
fn parity_where_or() {
    let (db, _) = std_graph();
    let q = format!("{KNOWS} WHERE r.since = 2021 OR r.role = 'manager' RETURN b.name AS name");
    assert_eq!(sorted(aql_names(&db, &q)), sorted(cypher_names(&db, &q)));
}

// --- NOT over an edge prop (absent -> NOT(false) = true, so Erin included) ---

#[test]
fn aql_where_not() {
    let (db, _) = std_graph();
    let got = aql_names(
        &db,
        &format!("{KNOWS} WHERE NOT (r.since = 2021) RETURN b.name AS name"),
    );
    assert_eq!(sorted(got), owned(&["Carol", "Dave", "Erin"]));
}

#[test]
fn cypher_where_not() {
    let (db, _) = std_graph();
    let got = cypher_names(
        &db,
        &format!("{KNOWS} WHERE NOT (r.since = 2021) RETURN b.name AS name"),
    );
    assert_eq!(sorted(got), owned(&["Carol", "Dave", "Erin"]));
}

#[test]
fn parity_where_not() {
    let (db, _) = std_graph();
    let q = format!("{KNOWS} WHERE NOT (r.since = 2021) RETURN b.name AS name");
    assert_eq!(sorted(aql_names(&db, &q)), sorted(cypher_names(&db, &q)));
}

// --- string op on an edge prop (STARTS WITH) --------------------------------

#[test]
fn aql_where_starts_with() {
    let (db, _) = std_graph();
    let got = aql_names(
        &db,
        &format!("{KNOWS} WHERE r.role STARTS WITH 'eng' RETURN b.name AS name"),
    );
    assert_eq!(sorted(got), owned(&["Bob", "Dave"]));
}

#[test]
fn cypher_where_starts_with() {
    let (db, _) = std_graph();
    let got = cypher_names(
        &db,
        &format!("{KNOWS} WHERE r.role STARTS WITH 'eng' RETURN b.name AS name"),
    );
    assert_eq!(sorted(got), owned(&["Bob", "Dave"]));
}

#[test]
fn parity_where_starts_with() {
    let (db, _) = std_graph();
    let q = format!("{KNOWS} WHERE r.role STARTS WITH 'eng' RETURN b.name AS name");
    assert_eq!(sorted(aql_names(&db, &q)), sorted(cypher_names(&db, &q)));
}

// ===========================================================================
// ORDER BY cases (ordered comparisons; null placement matters).
// ===========================================================================

// --- ORDER BY r.since DESC (nulls FIRST) ------------------------------------

#[test]
fn aql_order_by_since_desc() {
    let (db, _) = std_graph();
    let got = aql_names(
        &db,
        &format!("{KNOWS} RETURN b.name AS name ORDER BY r.since DESC"),
    );
    assert_eq!(got, owned(&["Erin", "Dave", "Bob", "Carol"]));
}

#[test]
fn cypher_order_by_since_desc() {
    let (db, _) = std_graph();
    let got = cypher_names(
        &db,
        &format!("{KNOWS} RETURN b.name AS name ORDER BY r.since DESC"),
    );
    assert_eq!(got, owned(&["Erin", "Dave", "Bob", "Carol"]));
}

#[test]
fn parity_order_by_since_desc() {
    let (db, _) = std_graph();
    let q = format!("{KNOWS} RETURN b.name AS name ORDER BY r.since DESC");
    assert_eq!(aql_names(&db, &q), cypher_names(&db, &q));
}

// --- ORDER BY r.since ASC (nulls LAST) --------------------------------------

#[test]
fn aql_order_by_since_asc() {
    let (db, _) = std_graph();
    let got = aql_names(
        &db,
        &format!("{KNOWS} RETURN b.name AS name ORDER BY r.since ASC"),
    );
    assert_eq!(got, owned(&["Carol", "Bob", "Dave", "Erin"]));
}

#[test]
fn cypher_order_by_since_asc() {
    let (db, _) = std_graph();
    let got = cypher_names(
        &db,
        &format!("{KNOWS} RETURN b.name AS name ORDER BY r.since ASC"),
    );
    assert_eq!(got, owned(&["Carol", "Bob", "Dave", "Erin"]));
}

#[test]
fn parity_order_by_since_asc() {
    let (db, _) = std_graph();
    let q = format!("{KNOWS} RETURN b.name AS name ORDER BY r.since ASC");
    assert_eq!(aql_names(&db, &q), cypher_names(&db, &q));
}

// --- reserved ORDER BY r.target DESC (by endpoint id, distinct per edge) -----
// Ids ascend Bob<Carol<Dave<Erin, so DESC = [Erin, Dave, Carol, Bob] -- which
// differs from the traversal order, so a no-op/null sort is observably wrong.

#[test]
fn aql_order_by_reserved_target_desc() {
    let (db, _) = std_graph();
    let got = aql_names(
        &db,
        &format!("{KNOWS} RETURN b.name AS name ORDER BY r.target DESC"),
    );
    assert_eq!(got, owned(&["Erin", "Dave", "Carol", "Bob"]));
}

#[test]
fn cypher_order_by_reserved_target_desc() {
    let (db, _) = std_graph();
    let got = cypher_names(
        &db,
        &format!("{KNOWS} RETURN b.name AS name ORDER BY r.target DESC"),
    );
    assert_eq!(got, owned(&["Erin", "Dave", "Carol", "Bob"]));
}

#[test]
fn parity_order_by_reserved_target_desc() {
    let (db, _) = std_graph();
    let q = format!("{KNOWS} RETURN b.name AS name ORDER BY r.target DESC");
    assert_eq!(aql_names(&db, &q), cypher_names(&db, &q));
}

// --- ORDER BY edge prop composed with WHERE edge prop + LIMIT ----------------
// WHERE since > 2019 -> {Bob(2021), Dave(2022)}; ORDER BY since ASC -> Bob,Dave;
// LIMIT 1 -> [Bob] (proves the sort runs before the limit over the edge stream).

#[test]
fn aql_order_by_where_limit() {
    let (db, _) = std_graph();
    let got = aql_names(
        &db,
        &format!("{KNOWS} WHERE r.since > 2019 RETURN b.name AS name ORDER BY r.since ASC LIMIT 1"),
    );
    assert_eq!(got, owned(&["Bob"]));
}

#[test]
fn cypher_order_by_where_limit() {
    let (db, _) = std_graph();
    let got = cypher_names(
        &db,
        &format!("{KNOWS} WHERE r.since > 2019 RETURN b.name AS name ORDER BY r.since ASC LIMIT 1"),
    );
    assert_eq!(got, owned(&["Bob"]));
}

#[test]
fn parity_order_by_where_limit() {
    let (db, _) = std_graph();
    let q =
        format!("{KNOWS} WHERE r.since > 2019 RETURN b.name AS name ORDER BY r.since ASC LIMIT 1");
    assert_eq!(aql_names(&db, &q), cypher_names(&db, &q));
}

// --- multi-key ORDER BY b.age, r.since (each key resolves against its entity)
// Dedicated graph: an age tie (Bea & Cy at 25) must be broken by the EDGE key
// r.since, and the tie's creation order is the REVERSE of the since order, so a
// missing edge-key resolution is observably wrong.

fn multikey_graph() -> AletheiaDB {
    let db = AletheiaDB::new().expect("db");
    let alice = db
        .create_node(
            "Person",
            PropertyMapBuilder::new().insert("name", "Alice").build(),
        )
        .expect("alice");
    let mk = |name: &str, age: i64, since: i64| {
        let n = db
            .create_node(
                "Person",
                PropertyMapBuilder::new()
                    .insert("name", name)
                    .insert("age", age)
                    .build(),
            )
            .expect("node");
        db.create_edge(
            alice,
            n,
            "KNOWS",
            PropertyMapBuilder::new().insert("since", since).build(),
        )
        .expect("edge");
    };
    // Creation order Amy, Cy, Bea, Deb -- within the age-25 tie, Cy is created
    // before Bea, but Bea(since 2019) must sort before Cy(since 2023).
    mk("Amy", 17, 2030);
    mk("Cy", 25, 2023);
    mk("Bea", 25, 2019);
    mk("Deb", 40, 2001);
    db
}

#[test]
fn aql_order_by_multikey() {
    let db = multikey_graph();
    let got = aql_names(
        &db,
        "MATCH (a:Person {name: 'Alice'})-[r:KNOWS]->(b:Person) \
         RETURN b ORDER BY b.age ASC, r.since ASC",
    );
    assert_eq!(got, owned(&["Amy", "Bea", "Cy", "Deb"]));
}

#[test]
fn cypher_order_by_multikey() {
    let db = multikey_graph();
    let got = cypher_names(
        &db,
        "MATCH (a:Person {name: 'Alice'})-[r:KNOWS]->(b:Person) \
         RETURN b ORDER BY b.age ASC, r.since ASC",
    );
    assert_eq!(got, owned(&["Amy", "Bea", "Cy", "Deb"]));
}

#[test]
fn parity_order_by_multikey() {
    let db = multikey_graph();
    let q = "MATCH (a:Person {name: 'Alice'})-[r:KNOWS]->(b:Person) \
             RETURN b ORDER BY b.age ASC, r.since ASC";
    assert_eq!(aql_names(&db, q), cypher_names(&db, q));
}

// ===========================================================================
// Bi-temporal: edge predicates must respect AS-OF coordinates.
// ===========================================================================
//
// SCOPE NOTE. The lane-relevant bi-temporal requirement is that an edge
// predicate / sort key is evaluated against the edge *as visible at the query's
// AS-OF coordinate* -- an edge not valid at that coordinate is excluded (not
// matched). These tests assert exactly that, using single-version edges created
// at DISTINCT valid-times.
//
// It deliberately does NOT assert "AS OF sees a superseded value after a
// valid-time UPDATE": empirically (confirmed via `get_edge_at_valid_time` on
// this trunk), after `update_edge_with_valid_time` the engine does not
// reconstruct the earlier valid-time edge version -- `get_edge_at_valid_time`
// at a pre-update coordinate returns `EdgeNotFound`, and the AS-OF traversal
// drops the edge entirely. That is an engine-level historical-edge
// reconstruction gap OUTSIDE the edge-property WHERE/ORDER lane, so pinning a
// superseded value here would be a test the lane cannot make green.

/// Bi-temporal graph with two single-version KNOWS edges in DISTINCT valid
/// windows. Nodes are valid from 2018 (present across the window):
///
/// * Alice-KNOWS{since:2019}->Bob, valid from **2020**
/// * Alice-KNOWS{since:2025}->Carol, valid from **2024**
///
/// So AS OF 2019 -> neither edge; AS OF 2022 -> only Bob's edge; AS OF 2026 ->
/// both (validated empirically before adopting this fixture).
fn bitemporal_graph() -> AletheiaDB {
    let db = AletheiaDB::new().expect("db");
    let tn = ts("2018-01-01T00:00:00Z");
    let node = |name: &str| {
        db.create_node_with_valid_time(
            "Person",
            PropertyMapBuilder::new().insert("name", name).build(),
            Some(tn),
        )
        .expect("node")
    };
    let alice = node("Alice");
    let bob = node("Bob");
    let carol = node("Carol");
    // Create Carol's edge FIRST so the traversal (creation) order is Carol, Bob
    // -- the REVERSE of the ascending `since` order (Bob 2019 < Carol 2025). A
    // no-op / broken sort therefore yields the wrong order at AS OF 2026, making
    // the ordering assertion genuinely discriminating.
    db.create_edge_with_valid_time(
        alice,
        carol,
        "KNOWS",
        PropertyMapBuilder::new().insert("since", 2025).build(),
        Some(ts("2024-01-01T00:00:00Z")),
    )
    .expect("carol edge");
    db.create_edge_with_valid_time(
        alice,
        bob,
        "KNOWS",
        PropertyMapBuilder::new().insert("since", 2019).build(),
        Some(ts("2020-01-01T00:00:00Z")),
    )
    .expect("bob edge");
    db
}

const BT_KNOWS: &str = "MATCH (a:Person {name: 'Alice'})-[r:KNOWS]->(b:Person)";

#[test]
fn aql_bitemporal_where_predicate_evaluated_at_as_of() {
    let db = bitemporal_graph();
    // AS OF 2022: only Bob's edge (since 2019) is valid. Carol's edge (since
    // 2025) is not yet valid, so a predicate that WOULD match it returns nothing
    // -- the predicate is gated by AS-OF visibility, not applied to a phantom.
    let vt = micros("2022-01-01T00:00:00Z");
    assert_eq!(
        aql_names(
            &db,
            &format!("AS OF {vt} {BT_KNOWS} WHERE r.since = 2019 RETURN b.name AS name")
        ),
        owned(&["Bob"])
    );
    assert!(
        aql_names(
            &db,
            &format!("AS OF {vt} {BT_KNOWS} WHERE r.since = 2025 RETURN b.name AS name")
        )
        .is_empty(),
        "Carol's since=2025 edge is not valid at 2022, so it must not match"
    );
}

#[test]
fn aql_bitemporal_where_later_coordinate_sees_both_edges() {
    let db = bitemporal_graph();
    // AS OF 2026: both edges valid. Each predicate hits its own edge's value.
    let vt = micros("2026-01-01T00:00:00Z");
    assert_eq!(
        aql_names(
            &db,
            &format!("AS OF {vt} {BT_KNOWS} WHERE r.since = 2025 RETURN b.name AS name")
        ),
        owned(&["Carol"])
    );
    assert_eq!(
        aql_names(
            &db,
            &format!("AS OF {vt} {BT_KNOWS} WHERE r.since = 2019 RETURN b.name AS name")
        ),
        owned(&["Bob"])
    );
}

#[test]
fn aql_bitemporal_as_of_before_valid_from_excludes_edge() {
    let db = bitemporal_graph();
    // Before either edge's valid_from: no edge exists, so even a predicate on a
    // value one of them WILL later hold yields nothing.
    let vt = micros("2019-06-01T00:00:00Z");
    assert!(
        aql_names(
            &db,
            &format!("AS OF {vt} {BT_KNOWS} WHERE r.since = 2019 RETURN b.name AS name")
        )
        .is_empty()
    );
}

#[test]
fn aql_bitemporal_order_by_respects_as_of_visibility() {
    let db = bitemporal_graph();
    // AS OF 2022: only Bob visible -> ORDER BY r.since -> [Bob].
    let early = micros("2022-01-01T00:00:00Z");
    assert_eq!(
        aql_names(
            &db,
            &format!("AS OF {early} {BT_KNOWS} RETURN b.name AS name ORDER BY r.since ASC")
        ),
        owned(&["Bob"])
    );
    // AS OF 2026: both visible -> ORDER BY r.since ASC -> Bob(2019) then Carol(2025).
    let late = micros("2026-01-01T00:00:00Z");
    assert_eq!(
        aql_names(
            &db,
            &format!("AS OF {late} {BT_KNOWS} RETURN b.name AS name ORDER BY r.since ASC")
        ),
        owned(&["Bob", "Carol"])
    );
}

#[test]
fn cypher_bitemporal_edge_where_as_of_is_structured_error() {
    // DOCUMENTED LIMITATION: the Cypher multi-variable evaluator (which binds
    // edge variables) rejects temporal `AS OF` qualifiers, so an edge-variable
    // WHERE combined with AS OF is currently un-expressible in Cypher -- but it
    // must fail as a *structured error*, never a silently-wrong row.
    let db = bitemporal_graph();
    let q = format!(
        "{BT_KNOWS} WHERE r.since = 2019 AS OF SYSTEM_TIME '2021-01-01T00:00:00Z' RETURN b"
    );
    assert!(
        cypher_is_error(&db, &q),
        "Cypher edge-var WHERE + AS OF must be a structured error, not silent rows"
    );
}

// ===========================================================================
// Never-silently-wrong: unsupported forms must be a structured error.
// ===========================================================================

#[test]
fn aql_variable_depth_edge_predicate_is_error() {
    // A variable-depth relationship with a predicate on the relationship
    // variable cannot bind a single edge -- it must be rejected, not applied to
    // the target node (silent wrong/zero rows).
    let (db, _) = std_graph();
    let q = "MATCH (a:Person {name: 'Alice'})-[r:KNOWS*1..3]->(b:Person) \
         WHERE r.since > 2020 RETURN b.name AS name";
    assert!(
        aql_is_error(&db, q),
        "AQL variable-depth edge-var predicate must be a structured error"
    );
}

#[test]
fn cypher_variable_depth_edge_predicate_is_error() {
    // Cypher already rejects variable-length relationships inside a bound
    // pattern via the multi_pattern house rule -- assert it stays a structured
    // error (regression guard for the "never silently wrong" contract).
    let (db, _) = std_graph();
    let q = "MATCH (a:Person {name: 'Alice'})-[r:KNOWS*1..3]->(b:Person) \
         WHERE r.since > 2020 RETURN b";
    assert!(
        cypher_is_error(&db, q),
        "Cypher variable-depth edge-var predicate must be a structured error"
    );
}

#[test]
fn aql_multihop_relationship_var_predicate_is_error() {
    // A predicate leaf on a relationship variable within a MULTI-hop AQL pattern
    // (two relationship variables) has no edge channel to bind against and must
    // be rejected, not silently applied to a node.
    let (db, _) = std_graph();
    let q = "MATCH (a:Person {name: 'Alice'})-[r:KNOWS]->(b:Person)-[s:KNOWS]->(c:Person) \
             WHERE r.since > 2020 RETURN c.name AS name";
    assert!(
        aql_is_error(&db, q),
        "AQL multi-hop relationship-var predicate must be a structured error"
    );
}

// ===========================================================================
// Regression guards -- these should PASS immediately (node path untouched).
// ===========================================================================

#[test]
fn regression_node_only_where_projection_aql() {
    let (db, _) = std_graph();
    let got = aql_names(&db, "MATCH (n:Person) WHERE n.age > 18 RETURN n");
    assert_eq!(sorted(got), owned(&["Alice", "Bob", "Dave", "Erin"]));
}

#[test]
fn regression_node_only_where_projection_cypher() {
    let (db, _) = std_graph();
    let got = cypher_names(&db, "MATCH (n:Person) WHERE n.age > 18 RETURN n");
    assert_eq!(sorted(got), owned(&["Alice", "Bob", "Dave", "Erin"]));
}

#[test]
fn regression_node_only_where_projection_parity() {
    let (db, _) = std_graph();
    let q = "MATCH (n:Person) WHERE n.age > 18 RETURN n";
    assert_eq!(sorted(aql_names(&db, q)), sorted(cypher_names(&db, q)));
}

/// `RETURN r ORDER BY r.since` in Cypher is already GREEN (the multi_pattern
/// evaluator binds the edge variable and sorts by its own property). This guard
/// pins that behavior: the returned edges' `since` values are in ascending
/// order with the absent value (Erin's edge) placed last.
#[test]
fn regression_cypher_return_edge_order_by_since_is_green() {
    let (db, _) = std_graph();
    let rows = db
        .execute_cypher(&format!("{KNOWS} RETURN r ORDER BY r.since ASC"))
        .expect("cypher executes")
        .collect_all()
        .expect("collect");

    let sinces: Vec<Option<i64>> = rows
        .iter()
        .map(|row| {
            let edge = row
                .bindings
                .as_ref()
                .and_then(|b| {
                    b.iter().find_map(|(_, e)| match e {
                        EntityResult::Edge(edge) => Some(edge),
                        _ => None,
                    })
                })
                .expect("row binds an edge r");
            match edge.get_property("since") {
                Some(PropertyValue::Int(v)) => Some(*v),
                None => None,
                other => panic!("unexpected since {other:?}"),
            }
        })
        .collect();

    assert_eq!(
        sinces,
        vec![Some(2019), Some(2021), Some(2022), None],
        "edges ordered by since ASC, null (Erin) last"
    );
}
// ===========================================================================
// F1 -- relationship-distinct emission: parallel edges + self-loop.
// ===========================================================================

const PAR_KNOWS: &str = "MATCH (a:Person {name: 'Alice'})-[r:KNOWS]->(b:Person)";

/// Two PARALLEL KNOWS edges Alice->Bob with different `since` (a multigraph).
fn parallel_graph() -> AletheiaDB {
    let db = AletheiaDB::new().expect("db");
    let alice = db
        .create_node(
            "Person",
            PropertyMapBuilder::new().insert("name", "Alice").build(),
        )
        .expect("alice");
    let bob = db
        .create_node(
            "Person",
            PropertyMapBuilder::new().insert("name", "Bob").build(),
        )
        .expect("bob");
    db.create_edge(
        alice,
        bob,
        "KNOWS",
        PropertyMapBuilder::new().insert("since", 2019).build(),
    )
    .expect("edge 2019");
    db.create_edge(
        alice,
        bob,
        "KNOWS",
        PropertyMapBuilder::new().insert("since", 2023).build(),
    )
    .expect("edge 2023");
    db
}

#[test]
fn aql_parallel_edges_both_surface() {
    let db = parallel_graph();
    // Reference r (so the edge var binds -> relationship-distinct emission).
    let rows = aql_rows(&db, &format!("{PAR_KNOWS} WHERE r.since >= 0 RETURN b"));
    assert_eq!(
        rows.len(),
        2,
        "both parallel edges surface (relationship-distinct), not node-distinct"
    );
    let mut sinces: Vec<i64> = rows.iter().filter_map(aql_row_edge_since).collect();
    sinces.sort();
    assert_eq!(sinces, vec![2019, 2023]);
}

#[test]
fn cypher_parallel_edges_both_surface() {
    let db = parallel_graph();
    // Reference r so the row set is per-edge (Cypher binds the edge variable).
    let rows = cypher_rows(&db, &format!("{PAR_KNOWS} WHERE r.since >= 0 RETURN b"));
    assert_eq!(rows.len(), 2);
}

#[test]
fn aql_parallel_edge_where_picks_right_edge() {
    let db = parallel_graph();
    // WHERE selects the matching edge even though both target the same node --
    // a node-distinct traversal binding only the first-discovered edge would
    // drop Bob if that edge was the 2023 one.
    let r19 = aql_rows(&db, &format!("{PAR_KNOWS} WHERE r.since = 2019 RETURN b"));
    assert_eq!(r19.len(), 1);
    assert_eq!(aql_row_edge_since(&r19[0]), Some(2019));
    let r23 = aql_rows(&db, &format!("{PAR_KNOWS} WHERE r.since = 2023 RETURN b"));
    assert_eq!(r23.len(), 1);
    assert_eq!(aql_row_edge_since(&r23[0]), Some(2023));
}

#[test]
fn cypher_parallel_edge_where_picks_right_edge() {
    let db = parallel_graph();
    assert_eq!(
        cypher_names(&db, &format!("{PAR_KNOWS} WHERE r.since = 2019 RETURN b")),
        owned(&["Bob"])
    );
}

#[test]
fn parity_different_type_parallel_edge_where() {
    // std_graph: Alice-KNOWS{since:2021}->Bob AND Alice-FOLLOWS{since:2020}->Bob.
    // `-[r]->` WHERE r.since = 2020 must surface Bob via the FOLLOWS edge; a
    // node-distinct traversal binding the first KNOWS(2021) edge would drop it.
    let (db, _) = std_graph();
    assert_where_parity(
        &db,
        &format!("{ANYREL} WHERE r.since = 2020 RETURN b.name AS name"),
        &format!("{ANYREL} WHERE r.since = 2020 RETURN b.name AS name"),
        &["Bob"],
    );
}

/// Alice with a KNOWS self-loop (source == target).
fn self_loop_graph() -> (AletheiaDB, u64) {
    let db = AletheiaDB::new().expect("db");
    let alice = db
        .create_node(
            "Person",
            PropertyMapBuilder::new().insert("name", "Alice").build(),
        )
        .expect("alice");
    db.create_edge(
        alice,
        alice,
        "KNOWS",
        PropertyMapBuilder::new().insert("since", 2021).build(),
    )
    .expect("self-loop");
    (db, alice.as_u64())
}

#[test]
fn aql_self_loop_emits_row() {
    let (db, _) = self_loop_graph();
    // The edge var is referenced, so the self-loop (source == target) emits one
    // relationship-distinct row bound to its edge.
    assert_eq!(
        aql_names(
            &db,
            &format!("{PAR_KNOWS} WHERE r.since = 2021 RETURN b.name AS name")
        ),
        owned(&["Alice"])
    );
}

#[test]
fn cypher_self_loop_emits_row() {
    let (db, _) = self_loop_graph();
    assert_eq!(
        cypher_names(
            &db,
            &format!("{PAR_KNOWS} WHERE r.since = 2021 RETURN b.name AS name")
        ),
        owned(&["Alice"])
    );
}

#[test]
fn aql_self_loop_reserved_source_equals_target() {
    let (db, alice) = self_loop_graph();
    // On a self-loop, structural r.source == r.target == alice.
    assert_eq!(
        aql_names(
            &db,
            &format!(
                "{PAR_KNOWS} WHERE r.source = {alice} AND r.target = {alice} RETURN b.name AS name"
            )
        ),
        owned(&["Alice"])
    );
    assert!(
        aql_names(
            &db,
            &format!("{PAR_KNOWS} WHERE r.target = 999999 RETURN b.name AS name")
        )
        .is_empty()
    );
}

// ===========================================================================
// F5 -- reserved-field SHADOWING: structural fields beat like-named user props.
// ===========================================================================

const SH_KNOWS: &str = "MATCH (a:Person {name: 'Alice'})-[r:KNOWS]->(b:Person)";

/// Two KNOWS edges carrying like-named USER props `type`/`source` set to
/// reordering/mismatching values, so a flipped precedence would change results.
fn shadow_graph() -> (AletheiaDB, u64) {
    let db = AletheiaDB::new().expect("db");
    let person = |name: &str| {
        db.create_node(
            "Person",
            PropertyMapBuilder::new().insert("name", name).build(),
        )
        .expect("node")
    };
    let alice = person("Alice");
    let bob = person("Bob");
    let carol = person("Carol");
    db.create_edge(
        alice,
        bob,
        "KNOWS",
        PropertyMapBuilder::new()
            .insert("type", "bogus")
            .insert("source", 2)
            .insert("since", 2021)
            .build(),
    )
    .expect("bob edge");
    db.create_edge(
        alice,
        carol,
        "KNOWS",
        PropertyMapBuilder::new()
            .insert("type", "bogus")
            .insert("source", 1)
            .insert("since", 2019)
            .build(),
    )
    .expect("carol edge");
    (db, alice.as_u64())
}

#[test]
fn aql_reserved_type_shadows_user_prop() {
    let (db, _) = shadow_graph();
    // Structural label wins: `r.type = 'KNOWS'` matches, `r.type = 'bogus'`
    // (the USER prop value) does NOT.
    assert_eq!(
        sorted(aql_names(
            &db,
            &format!("{SH_KNOWS} WHERE r.type = 'KNOWS' RETURN b.name AS name")
        )),
        owned(&["Bob", "Carol"])
    );
    assert!(
        aql_names(
            &db,
            &format!("{SH_KNOWS} WHERE r.type = 'bogus' RETURN b.name AS name")
        )
        .is_empty()
    );
}

#[test]
fn cypher_reserved_type_shadows_user_prop() {
    let (db, _) = shadow_graph();
    assert_eq!(
        sorted(cypher_names(
            &db,
            &format!("{SH_KNOWS} WHERE r.type = 'KNOWS' RETURN b.name AS name")
        )),
        owned(&["Bob", "Carol"])
    );
    assert!(
        cypher_names(
            &db,
            &format!("{SH_KNOWS} WHERE r.type = 'bogus' RETURN b.name AS name")
        )
        .is_empty()
    );
}

#[test]
fn parity_reserved_type_shadowing() {
    let (db, _) = shadow_graph();
    assert_where_parity(
        &db,
        &format!("{SH_KNOWS} WHERE r.type = 'KNOWS' RETURN b.name AS name"),
        &format!("{SH_KNOWS} WHERE r.type = 'KNOWS' RETURN b.name AS name"),
        &["Bob", "Carol"],
    );
}

#[test]
fn aql_reserved_source_shadows_user_prop() {
    let (db, alice) = shadow_graph();
    // Structural source (== alice) wins over the user `source` (1 / 2).
    assert_eq!(
        sorted(aql_names(
            &db,
            &format!("{SH_KNOWS} WHERE r.source = {alice} RETURN b.name AS name")
        )),
        owned(&["Bob", "Carol"])
    );
    // The user `source` value 1 (Carol's edge) must NOT match once shadowed.
    assert!(
        aql_names(
            &db,
            &format!("{SH_KNOWS} WHERE r.source = 1 RETURN b.name AS name")
        )
        .is_empty()
    );
}

#[test]
fn aql_order_by_reserved_source_shadows_user_prop() {
    let (db, _) = shadow_graph();
    // Structural source is the same (alice) for both edges -> a stable no-op
    // sort preserves creation order [Bob, Carol]. Ordering by the USER `source`
    // (Bob=2, Carol=1) would instead give [Carol, Bob], so this pins the
    // structural-wins precedence for ORDER BY.
    assert_eq!(
        aql_names(
            &db,
            &format!("{SH_KNOWS} RETURN b.name AS name ORDER BY r.source ASC")
        ),
        owned(&["Bob", "Carol"])
    );
}

#[test]
fn cypher_order_by_reserved_source_shadows_user_prop() {
    let (db, _) = shadow_graph();
    assert_eq!(
        cypher_names(
            &db,
            &format!("{SH_KNOWS} RETURN b.name AS name ORDER BY r.source ASC")
        ),
        owned(&["Bob", "Carol"])
    );
}

// ===========================================================================
// F6/F7 -- ORDER BY reject path + structured error KIND for reject cases.
// ===========================================================================

#[test]
fn aql_multihop_relationship_var_order_by_is_error() {
    let (db, _) = std_graph();
    let q = "MATCH (a:Person {name: 'Alice'})-[r:KNOWS]->(b:Person)-[s:KNOWS]->(c:Person) \
             RETURN c.name AS name ORDER BY r.since";
    assert!(aql_is_error(&db, q));
}

#[test]
fn aql_variable_depth_edge_order_by_is_error() {
    let (db, _) = std_graph();
    let q = "MATCH (a:Person {name: 'Alice'})-[r:KNOWS*1..3]->(b:Person) \
             RETURN b.name AS name ORDER BY r.since";
    assert!(aql_is_error(&db, q));
}

#[test]
fn aql_reject_error_kind_is_unsupported_single_hop() {
    // F7: pin the reject reason (not merely "some Err") on all four shapes.
    let (db, _) = std_graph();
    let cases = [
        "MATCH (a:Person {name: 'Alice'})-[r:KNOWS*1..3]->(b:Person) \
         WHERE r.since > 2020 RETURN b.name AS name",
        "MATCH (a:Person {name: 'Alice'})-[r:KNOWS]->(b:Person)-[s:KNOWS]->(c:Person) \
         WHERE r.since > 2020 RETURN c.name AS name",
        "MATCH (a:Person {name: 'Alice'})-[r:KNOWS*1..3]->(b:Person) \
         RETURN b.name AS name ORDER BY r.since",
        "MATCH (a:Person {name: 'Alice'})-[r:KNOWS]->(b:Person)-[s:KNOWS]->(c:Person) \
         RETURN c.name AS name ORDER BY r.since",
    ];
    for q in cases {
        let e = aql_error(&db, q).unwrap_or_else(|| panic!("expected an error for [{q}]"));
        assert!(
            is_unsupported_hop_error(&e),
            "expected the 'single, fixed-length hop' UnsupportedFeature for [{q}], got {e:?}"
        );
    }
}

// ===========================================================================
// F8 -- reserved r.id and r.label.
// ===========================================================================

const ID_KNOWS: &str = "MATCH (a:Person {name: 'Alice'})-[r:KNOWS]->(b:Person)";

/// Alice-KNOWS->Bob then Alice-KNOWS->Carol; returns (db, bob_edge_id, carol_edge_id).
fn id_graph() -> (AletheiaDB, u64, u64) {
    let db = AletheiaDB::new().expect("db");
    let person = |name: &str| {
        db.create_node(
            "Person",
            PropertyMapBuilder::new().insert("name", name).build(),
        )
        .expect("node")
    };
    let alice = person("Alice");
    let bob = person("Bob");
    let carol = person("Carol");
    let e_bob = db
        .create_edge(
            alice,
            bob,
            "KNOWS",
            PropertyMapBuilder::new().insert("since", 2021).build(),
        )
        .expect("bob edge");
    let e_carol = db
        .create_edge(
            alice,
            carol,
            "KNOWS",
            PropertyMapBuilder::new().insert("since", 2019).build(),
        )
        .expect("carol edge");
    (db, e_bob.as_u64(), e_carol.as_u64())
}

#[test]
fn aql_where_reserved_id() {
    let (db, e_bob, _) = id_graph();
    assert_eq!(
        aql_names(
            &db,
            &format!("{ID_KNOWS} WHERE r.id = {e_bob} RETURN b.name AS name")
        ),
        owned(&["Bob"])
    );
}

#[test]
fn cypher_where_reserved_id() {
    let (db, e_bob, _) = id_graph();
    assert_eq!(
        cypher_names(
            &db,
            &format!("{ID_KNOWS} WHERE r.id = {e_bob} RETURN b.name AS name")
        ),
        owned(&["Bob"])
    );
}

#[test]
fn aql_order_by_reserved_id() {
    let (db, _, _) = id_graph();
    // Bob's edge is created first (lower id), so ORDER BY r.id ASC -> [Bob, Carol].
    assert_eq!(
        aql_names(
            &db,
            &format!("{ID_KNOWS} RETURN b.name AS name ORDER BY r.id ASC")
        ),
        owned(&["Bob", "Carol"])
    );
}

#[test]
fn parity_where_reserved_label() {
    let (db, _) = std_graph();
    // `r.label` is the structural label alias of `r.type`.
    assert_where_parity(
        &db,
        &format!("{KNOWS} WHERE r.label = 'KNOWS' RETURN b.name AS name"),
        &format!("{KNOWS} WHERE r.label = 'KNOWS' RETURN b.name AS name"),
        &["Bob", "Carol", "Dave", "Erin"],
    );
}

// ===========================================================================
// F9 -- edge leaf AND node-provenance leaf in one WHERE (each routes to its
// own entity while the edge channel is populated).
// ===========================================================================

fn prov_graph() -> AletheiaDB {
    let db = AletheiaDB::new().expect("db");
    let alice = db
        .create_node(
            "Person",
            PropertyMapBuilder::new().insert("name", "Alice").build(),
        )
        .expect("alice");
    let person_conf = |name: &str, conf: f64| {
        db.create_node_with_options(
            "Person",
            PropertyMapBuilder::new().insert("name", name).build(),
            WriteRequestOptions::new().with_provenance(
                Provenance::builder()
                    .confidence(conf)
                    .build()
                    .expect("valid provenance"),
            ),
        )
        .expect("node")
    };
    let bob = person_conf("Bob", 0.95);
    let carol = person_conf("Carol", 0.95);
    let dave = person_conf("Dave", 0.5);
    let knows = |tgt, since: i64| {
        db.create_edge(
            alice,
            tgt,
            "KNOWS",
            PropertyMapBuilder::new().insert("since", since).build(),
        )
        .expect("edge");
    };
    knows(bob, 2021);
    knows(carol, 2019);
    knows(dave, 2022);
    db
}

#[test]
fn aql_where_edge_and_provenance() {
    let db = prov_graph();
    // Edge leaf (r.since) routes to the edge channel; node-provenance leaf
    // (confidence(b)) routes to the target node's provenance. Carol fails the
    // EDGE leaf (2019 !> 2020); Dave fails the NODE-provenance leaf (0.5 < 0.9).
    assert_eq!(
        aql_names(
            &db,
            "MATCH (a:Person {name: 'Alice'})-[r:KNOWS]->(b:Person) \
             WHERE r.since > 2020 AND confidence(b) >= 0.9 RETURN b.name AS name"
        ),
        owned(&["Bob"])
    );
}

// ===========================================================================
// F10 -- negative / float edge property (range WHERE + ORDER BY).
// ===========================================================================

const W_KNOWS: &str = "MATCH (a:Person {name: 'Alice'})-[r:KNOWS]->(b:Person)";

fn weight_graph() -> AletheiaDB {
    let db = AletheiaDB::new().expect("db");
    let person = |name: &str| {
        db.create_node(
            "Person",
            PropertyMapBuilder::new().insert("name", name).build(),
        )
        .expect("node")
    };
    let alice = person("Alice");
    let bob = person("Bob");
    let carol = person("Carol");
    let dave = person("Dave");
    let knows = |tgt, w: f64| {
        db.create_edge(
            alice,
            tgt,
            "KNOWS",
            PropertyMapBuilder::new().insert("weight", w).build(),
        )
        .expect("edge");
    };
    knows(bob, -1.5);
    knows(carol, 2.5);
    knows(dave, 0.0);
    db
}

#[test]
fn parity_where_negative_float_weight() {
    let db = weight_graph();
    // Compare with a float literal `0.0`: AQL's WHERE comparators do not coerce
    // Float-vs-Int operands (a pre-existing AQL limitation orthogonal to this
    // lane -- it affects node comparisons too), so a type-matched literal keeps
    // the test in-lane while still exercising a negative float edge property.
    assert_where_parity(
        &db,
        &format!("{W_KNOWS} WHERE r.weight < 0.0 RETURN b.name AS name"),
        &format!("{W_KNOWS} WHERE r.weight < 0.0 RETURN b.name AS name"),
        &["Bob"],
    );
}

#[test]
fn aql_order_by_float_weight() {
    let db = weight_graph();
    assert_eq!(
        aql_names(
            &db,
            &format!("{W_KNOWS} RETURN b.name AS name ORDER BY r.weight ASC")
        ),
        owned(&["Bob", "Dave", "Carol"])
    );
}

#[test]
fn cypher_order_by_float_weight() {
    let db = weight_graph();
    assert_eq!(
        cypher_names(
            &db,
            &format!("{W_KNOWS} RETURN b.name AS name ORDER BY r.weight ASC")
        ),
        owned(&["Bob", "Dave", "Carol"])
    );
}

// ===========================================================================
// F11 -- edge-prop IN / CONTAINS / IS NULL parity (plain + negated), incl.
// the absent-property node-semantics that F2 fixes on the Cypher side.
// ===========================================================================

#[test]
fn parity_where_in() {
    let (db, _) = std_graph();
    assert_where_parity(
        &db,
        &format!("{KNOWS} WHERE r.role IN ['engineer', 'manager'] RETURN b.name AS name"),
        &format!("{KNOWS} WHERE r.role IN ['engineer', 'manager'] RETURN b.name AS name"),
        &["Bob", "Carol", "Dave"],
    );
}

#[test]
fn parity_where_not_in_absent_includes() {
    let (db, _) = std_graph();
    // Erin's edge has no `since` -> node-semantics `IN` is definite false ->
    // NOT includes Erin (F2). Dave (2022 not in list) also included.
    assert_where_parity(
        &db,
        &format!("{KNOWS} WHERE NOT (r.since IN [2019, 2021]) RETURN b.name AS name"),
        &format!("{KNOWS} WHERE NOT (r.since IN [2019, 2021]) RETURN b.name AS name"),
        &["Dave", "Erin"],
    );
}

#[test]
fn parity_where_contains() {
    let (db, _) = std_graph();
    assert_where_parity(
        &db,
        &format!("{KNOWS} WHERE r.role CONTAINS 'eng' RETURN b.name AS name"),
        &format!("{KNOWS} WHERE r.role CONTAINS 'eng' RETURN b.name AS name"),
        &["Bob", "Dave"],
    );
}

#[test]
fn parity_where_not_contains_absent_includes() {
    let (db, _) = std_graph();
    // No edge carries `nope` -> node-semantics CONTAINS is definite false ->
    // NOT includes every KNOWS target (F2 Cypher fix; AQL already did this).
    assert_where_parity(
        &db,
        &format!("{KNOWS} WHERE NOT (r.nope CONTAINS 'x') RETURN b.name AS name"),
        &format!("{KNOWS} WHERE NOT (r.nope CONTAINS 'x') RETURN b.name AS name"),
        &["Bob", "Carol", "Dave", "Erin"],
    );
}

#[test]
fn parity_where_is_null() {
    let (db, _) = std_graph();
    assert_where_parity(
        &db,
        &format!("{KNOWS} WHERE r.since IS NULL RETURN b.name AS name"),
        &format!("{KNOWS} WHERE r.since IS NULL RETURN b.name AS name"),
        &["Erin"],
    );
}

#[test]
fn parity_where_is_not_null() {
    let (db, _) = std_graph();
    assert_where_parity(
        &db,
        &format!("{KNOWS} WHERE r.since IS NOT NULL RETURN b.name AS name"),
        &format!("{KNOWS} WHERE r.since IS NOT NULL RETURN b.name AS name"),
        &["Bob", "Carol", "Dave"],
    );
}

// ===========================================================================
// F13 -- parity_where_reserved_source (sibling of type/target).
// ===========================================================================

#[test]
fn parity_where_reserved_source() {
    let (db, ids) = std_graph();
    assert_where_parity(
        &db,
        &format!(
            "{KNOWS} WHERE r.source = {} RETURN b.name AS name",
            ids.alice
        ),
        &format!(
            "{KNOWS} WHERE r.source = {} RETURN b.name AS name",
            ids.alice
        ),
        &["Bob", "Carol", "Dave", "Erin"],
    );
}

// ===========================================================================
// F15 -- unknown-variable WHERE/ORDER BY leaf is a structured error (AQL).
// ===========================================================================

#[test]
fn aql_where_unknown_variable_is_error() {
    let (db, _) = std_graph();
    // `rr` is not bound by the pattern (typo for `r`): reject, do not silently
    // treat it as a node filter.
    let q = format!("{KNOWS} WHERE rr.since = 2019 RETURN b.name AS name");
    let e = aql_error(&db, &q).expect("expected an error for an unknown variable");
    assert!(
        matches!(e, Error::Query(QueryError::SyntaxError { ref message }) if message.contains("unknown variable")),
        "expected an unknown-variable SyntaxError, got {e:?}"
    );
}

#[test]
fn aql_order_by_unknown_variable_is_error() {
    let (db, _) = std_graph();
    let q = format!("{KNOWS} RETURN b.name AS name ORDER BY rr.since");
    assert!(aql_is_error(&db, &q));
}

#[test]
fn aql_where_known_node_var_still_allowed() {
    // A declared node variable (even the anchor `a`) is NOT rejected -- only
    // genuinely unknown variables are (F15 conservative contract).
    let (db, _) = std_graph();
    // `a` is the declared anchor node; referencing it in WHERE must still
    // parse/execute (prior behavior preserved), NOT raise the unknown-variable
    // rejection. (Its leaf resolves against the row entity as before -- that
    // non-terminal-node semantics is pre-existing and out of this lane's scope;
    // here we only assert it is not rejected.)
    let q = format!("{KNOWS} WHERE a.name = 'Alice' RETURN b.name AS name");
    assert!(
        aql_error(&db, &q).is_none(),
        "a declared node variable must not be rejected as unknown"
    );
}

// ===========================================================================
// F16 -- cross-entity property comparison `r.x = b.y` is a structured error.
// ===========================================================================

#[test]
fn aql_cross_entity_comparison_is_error() {
    let (db, _) = std_graph();
    let q = format!("{KNOWS} WHERE r.since = b.age RETURN b.name AS name");
    let e = aql_error(&db, &q).expect("expected an error for cross-entity comparison");
    assert!(
        matches!(e, Error::Query(QueryError::UnsupportedFeature { ref feature }) if feature.contains("two property accesses")),
        "expected the cross-entity UnsupportedFeature, got {e:?}"
    );
}

// ===========================================================================
// F3 -- node/edge AS-OF consistency (target node reconstructed at coordinate).
// ===========================================================================

/// Alice (valid 2018), Bob node valid only from 2024, and a single-version edge
/// Alice-KNOWS{since:2019}->Bob valid from 2020. So at AS OF 2022 the edge is
/// valid but Bob's node is not yet -- a coordinate-consistent read must exclude
/// the row (F3), whereas a current-state target read would wrongly surface Bob.
fn f3_graph() -> AletheiaDB {
    let db = AletheiaDB::new().expect("db");
    let alice = db
        .create_node_with_valid_time(
            "Person",
            PropertyMapBuilder::new().insert("name", "Alice").build(),
            Some(ts("2018-01-01T00:00:00Z")),
        )
        .expect("alice");
    let bob = db
        .create_node_with_valid_time(
            "Person",
            PropertyMapBuilder::new()
                .insert("name", "Bob")
                .insert("age", 25)
                .build(),
            Some(ts("2024-01-01T00:00:00Z")),
        )
        .expect("bob");
    db.create_edge_with_valid_time(
        alice,
        bob,
        "KNOWS",
        PropertyMapBuilder::new().insert("since", 2019).build(),
        Some(ts("2020-01-01T00:00:00Z")),
    )
    .expect("edge");
    db
}

#[test]
fn aql_bitemporal_target_node_reconstructed_at_coordinate() {
    let db = f3_graph();
    // AS OF 2022: edge valid, Bob's node not yet valid -> row excluded (the node
    // is read AS-OF the coordinate, consistent with the reconstructed edge).
    let early = micros("2022-01-01T00:00:00Z");
    assert!(
        aql_names(
            &db,
            &format!("AS OF {early} {BT_KNOWS} WHERE r.since = 2019 RETURN b.name AS name")
        )
        .is_empty(),
        "target node not valid at the coordinate -> excluded (F3 consistency)"
    );
    // AS OF 2025: both valid -> Bob surfaces, and his reconstructed node carries
    // age 25 (node property read at the coordinate).
    let late = micros("2025-01-01T00:00:00Z");
    assert_eq!(
        aql_names(
            &db,
            &format!(
                "AS OF {late} {BT_KNOWS} WHERE r.since = 2019 AND b.age = 25 RETURN b.name AS name"
            )
        ),
        owned(&["Bob"])
    );
}
