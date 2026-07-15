//! openCypher compatibility suite (Issue #561).
//!
//! A table-driven contract that pins the **currently-supported read-only Cypher
//! subset** and asserts that **unsupported constructs are rejected with
//! structured errors** — never silently answered with wrong data.
//!
//! # What this suite guarantees
//!
//! - Every construct documented as *supported* in
//!   [`docs/guides/cypher-compatibility.md`](../../docs/guides/cypher-compatibility.md)
//!   parses and (where deterministic) executes against a known seed graph with
//!   the exact expected row count.
//! - Every construct documented as *intentionally unsupported* is rejected with
//!   the precise [`CypherError`] variant (and an intent-pinning message
//!   substring), asserted at the `parse_cypher` boundary — because at the
//!   `execute_cypher` boundary the variants collapse into the coarser
//!   `QueryError` family and precise intent would be lost.
//!
//! # Structure
//!
//! - [`compat_db`] builds a deterministic graph (automated test-data setup).
//! - [`Case`] + [`Expect`] model one corpus entry.
//! - `supported_execute_cases` / `supported_parse_cases` / `rejected_cases`
//!   return the curated corpus, grouped by category.
//! - Three runner `#[test]`s each iterate their slice, **accumulate every
//!   failure**, and panic once with an interpretable multi-line report so a CI
//!   failure names exactly which cases regressed.

use std::collections::HashMap;

use crate::AletheiaDB;
use crate::core::property::{PropertyMap, PropertyMapBuilder};

use super::converter::{CypherParameterValue, parse_cypher, parse_cypher_with_params};
use super::error::CypherError;

// ---------------------------------------------------------------------------
// Automated seed graph
// ---------------------------------------------------------------------------

/// Build the deterministic compatibility seed graph.
///
/// Nodes:
/// - `Person` Alice {name:'Alice', age:30, dept:'Eng'}
/// - `Person` Bob   {name:'Bob',   age:25, dept:'Eng'}
/// - `Person` Carol {name:'Carol', age:40, dept:'Sales'}
/// - `Person` Dave  {name:'Dave',  age:35, dept:'Sales'}
/// - `Company` Acme   {name:'Acme',   cid:1}
/// - `Company` Globex {name:'Globex', cid:2}
///
/// No node carries an `email` property (pins openCypher-correct `IS NULL` /
/// `IS NOT NULL` on an absent property: `IS NULL` matches all 4, `IS NOT NULL`
/// matches 0 -- fixed in #3511).
///
/// Edges:
/// - Alice -[:KNOWS]-> Bob -[:KNOWS]-> Carol -[:KNOWS]-> Dave (a linear chain)
/// - Alice -[:WORKS_AT]-> Acme
/// - Dave  -[:WORKS_AT]-> Globex
fn compat_db() -> AletheiaDB {
    let db = AletheiaDB::new().expect("create in-memory db");

    let person = |name: &str, age: i64, dept: &str| {
        PropertyMapBuilder::new()
            .insert("name", name)
            .insert("age", age)
            .insert("dept", dept)
            .build()
    };

    let alice = db
        .create_node("Person", person("Alice", 30, "Eng"))
        .unwrap();
    let bob = db.create_node("Person", person("Bob", 25, "Eng")).unwrap();
    let carol = db
        .create_node("Person", person("Carol", 40, "Sales"))
        .unwrap();
    let dave = db
        .create_node("Person", person("Dave", 35, "Sales"))
        .unwrap();

    let acme = db
        .create_node(
            "Company",
            PropertyMapBuilder::new()
                .insert("name", "Acme")
                .insert("cid", 1i64)
                .build(),
        )
        .unwrap();
    let globex = db
        .create_node(
            "Company",
            PropertyMapBuilder::new()
                .insert("name", "Globex")
                .insert("cid", 2i64)
                .build(),
        )
        .unwrap();

    db.create_edge(alice, bob, "KNOWS", PropertyMap::new())
        .unwrap();
    db.create_edge(bob, carol, "KNOWS", PropertyMap::new())
        .unwrap();
    db.create_edge(carol, dave, "KNOWS", PropertyMap::new())
        .unwrap();
    db.create_edge(alice, acme, "WORKS_AT", PropertyMap::new())
        .unwrap();
    db.create_edge(dave, globex, "WORKS_AT", PropertyMap::new())
        .unwrap();

    db
}

// ---------------------------------------------------------------------------
// Case model
// ---------------------------------------------------------------------------

/// The kind of structured rejection a case expects, mapped to the precise
/// [`CypherError`] variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RejectKind {
    /// [`CypherError::ParseError`].
    Parse,
    /// [`CypherError::UnsupportedFeature`].
    Unsupported,
    /// [`CypherError::SemanticError`].
    Semantic,
    /// [`CypherError::ParameterError`].
    Parameter,
    /// [`CypherError::InvalidTemporalClause`].
    Temporal,
    /// [`CypherError::InvalidTimestamp`].
    Timestamp,
}

impl RejectKind {
    fn matches(self, err: &CypherError) -> bool {
        match self {
            RejectKind::Parse => matches!(err, CypherError::ParseError { .. }),
            RejectKind::Unsupported => matches!(err, CypherError::UnsupportedFeature(_)),
            RejectKind::Semantic => matches!(err, CypherError::SemanticError(_)),
            RejectKind::Parameter => matches!(err, CypherError::ParameterError(_)),
            RejectKind::Temporal => matches!(err, CypherError::InvalidTemporalClause(_)),
            RejectKind::Timestamp => matches!(err, CypherError::InvalidTimestamp(_)),
        }
    }
}

/// The expectation pinned for a corpus entry.
enum Expect {
    /// Executes against the seed graph and returns exactly this many rows.
    Executes(usize),
    /// Executes and returns at least this many rows (for cases whose exact
    /// count is awkward; the corpus prefers `Executes`).
    #[allow(dead_code)]
    ExecutesAtLeast(usize),
    /// Parses and lowers (`parse_cypher`) without error — used for constructs
    /// awkward to execute deterministically (temporal, vector).
    Parses,
    /// Rejected with the given [`RejectKind`]; the error message must contain
    /// the substring to pin intent.
    Rejected(RejectKind, &'static str),
}

/// One corpus entry: a category tag, a human name, the query, optional
/// parameter bindings, and the expectation.
struct Case {
    category: &'static str,
    name: &'static str,
    query: &'static str,
    params: Vec<(&'static str, CypherParameterValue)>,
    expect: Expect,
}

impl Case {
    fn new(
        category: &'static str,
        name: &'static str,
        query: &'static str,
        expect: Expect,
    ) -> Self {
        Self {
            category,
            name,
            query,
            params: Vec::new(),
            expect,
        }
    }

    fn with_params(mut self, params: Vec<(&'static str, CypherParameterValue)>) -> Self {
        self.params = params;
        self
    }

    fn param_map(&self) -> HashMap<String, CypherParameterValue> {
        self.params
            .iter()
            .map(|(k, v)| ((*k).to_string(), v.clone()))
            .collect()
    }
}

// ---------------------------------------------------------------------------
// Corpus: supported (executing) cases
// ---------------------------------------------------------------------------

fn supported_execute_cases() -> Vec<Case> {
    use Expect::Executes;

    vec![
        // ---- basic ---------------------------------------------------------
        Case::new(
            "basic",
            "labeled_scan_person",
            "MATCH (n:Person) RETURN n",
            Executes(4),
        ),
        Case::new(
            "basic",
            "labeled_scan_company",
            "MATCH (c:Company) RETURN c",
            Executes(2),
        ),
        Case::new(
            "basic",
            "unlabeled_scan_all",
            "MATCH (n) RETURN n",
            Executes(6),
        ),
        Case::new(
            "basic",
            "inline_prop_single",
            "MATCH (n:Person {name: 'Alice'}) RETURN n",
            Executes(1),
        ),
        Case::new(
            "basic",
            "inline_prop_dept",
            "MATCH (n:Person {dept: 'Eng'}) RETURN n",
            Executes(2),
        ),
        // ---- where ---------------------------------------------------------
        Case::new(
            "where",
            "gt",
            "MATCH (n:Person) WHERE n.age > 30 RETURN n",
            Executes(2),
        ),
        Case::new(
            "where",
            "gte",
            "MATCH (n:Person) WHERE n.age >= 30 RETURN n",
            Executes(3),
        ),
        Case::new(
            "where",
            "lt",
            "MATCH (n:Person) WHERE n.age < 30 RETURN n",
            Executes(1),
        ),
        Case::new(
            "where",
            "lte",
            "MATCH (n:Person) WHERE n.age <= 30 RETURN n",
            Executes(2),
        ),
        Case::new(
            "where",
            "eq",
            "MATCH (n:Person) WHERE n.age = 30 RETURN n",
            Executes(1),
        ),
        Case::new(
            "where",
            "neq",
            "MATCH (n:Person) WHERE n.age <> 30 RETURN n",
            Executes(3),
        ),
        Case::new(
            "where",
            "and",
            "MATCH (n:Person) WHERE n.age > 30 AND n.dept = 'Sales' RETURN n",
            Executes(2),
        ),
        Case::new(
            "where",
            "or",
            "MATCH (n:Person) WHERE n.age < 30 OR n.age > 35 RETURN n",
            Executes(2),
        ),
        Case::new(
            "where",
            "not",
            "MATCH (n:Person) WHERE NOT n.dept = 'Eng' RETURN n",
            Executes(2),
        ),
        Case::new(
            "where",
            "in_two",
            "MATCH (n:Person) WHERE n.dept IN ['Eng', 'Sales'] RETURN n",
            Executes(4),
        ),
        Case::new(
            "where",
            "in_one",
            "MATCH (n:Person) WHERE n.dept IN ['Eng'] RETURN n",
            Executes(2),
        ),
        Case::new(
            "where",
            "starts_with",
            "MATCH (n:Person) WHERE n.name STARTS WITH 'A' RETURN n",
            Executes(1),
        ),
        Case::new(
            "where",
            "ends_with",
            "MATCH (n:Person) WHERE n.name ENDS WITH 'e' RETURN n",
            Executes(2),
        ),
        Case::new(
            "where",
            "contains",
            "MATCH (n:Person) WHERE n.name CONTAINS 'ar' RETURN n",
            Executes(1),
        ),
        // `IS NULL` / `IS NOT NULL` are openCypher-correct for both *present*
        // and *absent* properties. Present: `name` is set on all 4 Persons, so
        // `IS NULL` -> 0, `IS NOT NULL` -> 4. Absent: no Person has `email`, so
        // (per openCypher, a missing property IS null) `IS NULL` -> 4,
        // `IS NOT NULL` -> 0. The absent-property cases were previously an
        // openCypher deviation (inverted); fixed in #3511.
        Case::new(
            "where",
            "is_null_present_prop",
            "MATCH (n:Person) WHERE n.name IS NULL RETURN n",
            Executes(0),
        ),
        Case::new(
            "where",
            "is_not_null_present_prop",
            "MATCH (n:Person) WHERE n.name IS NOT NULL RETURN n",
            Executes(4),
        ),
        Case::new(
            "where",
            "is_null_absent_prop",
            "MATCH (n:Person) WHERE n.email IS NULL RETURN n",
            Executes(4),
        ),
        Case::new(
            "where",
            "is_not_null_absent_prop",
            "MATCH (n:Person) WHERE n.email IS NOT NULL RETURN n",
            Executes(0),
        ),
        // ---- patterns ------------------------------------------------------
        Case::new(
            "patterns",
            "out_edge_typed",
            "MATCH (a:Person {name: 'Alice'})-[:KNOWS]->(b) RETURN b",
            Executes(1),
        ),
        Case::new(
            "patterns",
            "out_edge_works_at",
            "MATCH (a:Person {name: 'Alice'})-[:WORKS_AT]->(c) RETURN c",
            Executes(1),
        ),
        Case::new(
            "patterns",
            "out_edge_all_persons",
            "MATCH (a:Person)-[:KNOWS]->(b) RETURN b",
            Executes(3),
        ),
        Case::new(
            "patterns",
            "out_edge_typed_target_label",
            "MATCH (a:Person)-[:WORKS_AT]->(c:Company) RETURN c",
            Executes(2),
        ),
        Case::new(
            "patterns",
            "in_edge",
            "MATCH (a:Person {name: 'Bob'})<-[:KNOWS]-(x) RETURN x",
            Executes(1),
        ),
        Case::new(
            "patterns",
            "undirected_edge",
            "MATCH (a:Person {name: 'Bob'})-[:KNOWS]-(x) RETURN x",
            Executes(2),
        ),
        // ---- varlength -----------------------------------------------------
        Case::new(
            "varlength",
            "range_1_2",
            "MATCH (a:Person {name: 'Alice'})-[:KNOWS*1..2]->(b) RETURN b",
            Executes(2),
        ),
        Case::new(
            "varlength",
            "range_1_3",
            "MATCH (a:Person {name: 'Alice'})-[:KNOWS*1..3]->(b) RETURN b",
            Executes(3),
        ),
        Case::new(
            "varlength",
            "exact_1",
            "MATCH (a:Person {name: 'Alice'})-[:KNOWS*1]->(b) RETURN b",
            Executes(1),
        ),
        Case::new(
            "varlength",
            "exact_2",
            "MATCH (a:Person {name: 'Alice'})-[:KNOWS*2]->(b) RETURN b",
            Executes(1),
        ),
        Case::new(
            "varlength",
            "unbounded_star",
            "MATCH (a:Person {name: 'Alice'})-[:KNOWS*]->(b) RETURN b",
            Executes(3),
        ),
        Case::new(
            "varlength",
            "min_only",
            "MATCH (a:Person {name: 'Alice'})-[:KNOWS*2..]->(b) RETURN b",
            Executes(2),
        ),
        Case::new(
            "varlength",
            "max_only",
            "MATCH (a:Person {name: 'Alice'})-[:KNOWS*..2]->(b) RETURN b",
            Executes(2),
        ),
        // ---- projection ----------------------------------------------------
        Case::new(
            "projection",
            "limit",
            "MATCH (n:Person) RETURN n LIMIT 2",
            Executes(2),
        ),
        Case::new(
            "projection",
            "skip",
            "MATCH (n:Person) RETURN n SKIP 2",
            Executes(2),
        ),
        Case::new(
            "projection",
            "skip_limit",
            "MATCH (n:Person) RETURN n SKIP 1 LIMIT 2",
            Executes(2),
        ),
        Case::new(
            "projection",
            "distinct",
            "MATCH (n:Person) RETURN DISTINCT n",
            Executes(4),
        ),
        Case::new(
            "projection",
            "alias",
            "MATCH (n:Person) RETURN n AS person",
            Executes(4),
        ),
        // ---- ordering ------------------------------------------------------
        Case::new(
            "ordering",
            "order_asc",
            "MATCH (n:Person) RETURN n ORDER BY n.age",
            Executes(4),
        ),
        Case::new(
            "ordering",
            "order_desc",
            "MATCH (n:Person) RETURN n ORDER BY n.age DESC",
            Executes(4),
        ),
        Case::new(
            "ordering",
            "order_multi_key",
            "MATCH (n:Person) RETURN n ORDER BY n.dept, n.age DESC",
            Executes(4),
        ),
        // ---- aggregation (row-count assertions) ----------------------------
        Case::new(
            "aggregation",
            "count_star",
            "MATCH (n:Person) RETURN count(*)",
            Executes(1),
        ),
        Case::new(
            "aggregation",
            "count_var",
            "MATCH (n:Person) RETURN count(n)",
            Executes(1),
        ),
        Case::new(
            "aggregation",
            "count_distinct",
            "MATCH (n:Person) RETURN count(DISTINCT n.dept)",
            Executes(1),
        ),
        Case::new(
            "aggregation",
            "sum",
            "MATCH (n:Person) RETURN sum(n.age)",
            Executes(1),
        ),
        Case::new(
            "aggregation",
            "avg",
            "MATCH (n:Person) RETURN avg(n.age)",
            Executes(1),
        ),
        Case::new(
            "aggregation",
            "min_max",
            "MATCH (n:Person) RETURN min(n.age), max(n.age)",
            Executes(1),
        ),
        Case::new(
            "aggregation",
            "collect",
            "MATCH (n:Person) RETURN collect(n.name)",
            Executes(1),
        ),
        Case::new(
            "aggregation",
            "grouped_count",
            "MATCH (n:Person) RETURN n.dept, count(*)",
            Executes(2),
        ),
        Case::new(
            "aggregation",
            "grouped_count_ordered",
            "MATCH (n:Person) RETURN n.dept, count(*) ORDER BY count(*) DESC",
            Executes(2),
        ),
        // ---- with ----------------------------------------------------------
        Case::new(
            "with",
            "with_where",
            "MATCH (a:Person) WITH a WHERE a.age > 30 RETURN a",
            Executes(2),
        ),
        Case::new(
            "with",
            "with_alias",
            "MATCH (a:Person) WITH a AS p WHERE p.age > 30 RETURN p",
            Executes(2),
        ),
        Case::new(
            "with",
            "with_chained",
            "MATCH (a:Person) WITH a WHERE a.age > 30 WITH a WHERE a.age < 40 RETURN a",
            Executes(1),
        ),
        // ---- unwind (standalone, via execute_cypher) -----------------------
        Case::new(
            "unwind",
            "list_three",
            "UNWIND [1, 2, 3] AS x RETURN x",
            Executes(3),
        ),
        Case::new(
            "unwind",
            "list_four",
            "UNWIND [1, 2, 3, 4] AS x RETURN x",
            Executes(4),
        ),
        Case::new(
            "unwind",
            "empty_list",
            "UNWIND [] AS x RETURN x",
            Executes(0),
        ),
        Case::new(
            "unwind",
            "distinct",
            "UNWIND [1, 1, 2] AS x RETURN DISTINCT x",
            Executes(2),
        ),
        Case::new(
            "unwind",
            "order_by",
            "UNWIND [3, 1, 2] AS x RETURN x ORDER BY x",
            Executes(3),
        ),
        // ---- optional-match ------------------------------------------------
        Case::new(
            "optional-match",
            "matched",
            "MATCH (a:Person {name: 'Alice'}) OPTIONAL MATCH (a)-[:KNOWS]->(x) RETURN x",
            Executes(1),
        ),
        Case::new(
            "optional-match",
            "unmatched_preserves_row",
            "MATCH (a:Person {name: 'Dave'}) OPTIONAL MATCH (a)-[:KNOWS]->(x) RETURN x",
            Executes(1),
        ),
        Case::new(
            "optional-match",
            "all_persons_left_outer",
            "MATCH (a:Person) OPTIONAL MATCH (a)-[:KNOWS]->(x) RETURN x",
            Executes(4),
        ),
        // ---- params --------------------------------------------------------
        Case::new(
            "params",
            "string_param",
            "MATCH (n:Person {name: $name}) RETURN n",
            Executes(1),
        )
        .with_params(vec![("name", CypherParameterValue::String("Alice".into()))]),
        Case::new(
            "params",
            "string_param_dept",
            "MATCH (n:Person {dept: $dept}) RETURN n",
            Executes(2),
        )
        .with_params(vec![("dept", CypherParameterValue::String("Eng".into()))]),
        // ---- write clauses (Issue #560) ------------------------------------
        // Row-count contracts against a FRESH seed graph per case (the runner
        // rebuilds `compat_db` per case so these mutations are isolated).
        // Deeper side-effect assertions live in `cypher::tests::mutations`.
        Case::new(
            "mutating",
            "create_returns_created",
            "CREATE (n:Person {name: 'Zed'}) RETURN n",
            Executes(1),
        ),
        Case::new(
            "mutating",
            "create_no_return",
            "CREATE (n:Person {name: 'Zed'})",
            Executes(0),
        ),
        // MERGE (Issue #3548): no `Zed` in the seed, so this creates and returns
        // one node (the match branch is exercised in `cypher::tests::mutations`).
        Case::new(
            "mutating",
            "merge",
            "MERGE (n:Person {name: 'Zed'}) RETURN n",
            Executes(1),
        ),
        Case::new(
            "mutating",
            "create_relationship",
            "CREATE (a:Team {name: 'X'})-[:HAS]->(b:Member {name: 'Y'}) RETURN a, b",
            Executes(1),
        ),
        Case::new(
            "mutating",
            "set_all_persons",
            "MATCH (n:Person) SET n.active = 1 RETURN n",
            Executes(4),
        ),
        Case::new(
            "mutating",
            "set_no_match",
            "MATCH (n:Person {name: 'Nobody'}) SET n.age = 1 RETURN n",
            Executes(0),
        ),
        Case::new(
            "mutating",
            "delete_relationship",
            "MATCH (a:Person {name: 'Alice'})-[r:KNOWS]->(b) DELETE r",
            Executes(0),
        ),
        Case::new(
            "mutating",
            "detach_delete_company",
            "MATCH (c:Company {name: 'Acme'}) DETACH DELETE c",
            Executes(0),
        ),
    ]
}

// ---------------------------------------------------------------------------
// Corpus: supported (convert-only / Parses) cases
// ---------------------------------------------------------------------------

fn supported_parse_cases() -> Vec<Case> {
    use Expect::Parses;

    vec![
        // ---- temporal ------------------------------------------------------
        Case::new(
            "temporal",
            "as_of_timestamp",
            "MATCH (n:Person) AS OF TIMESTAMP '2024-01-15T10:00:00Z' RETURN n",
            Parses,
        ),
        Case::new(
            "temporal",
            "as_of_valid_time",
            "MATCH (n:Person) AS OF VALID_TIME '2024-01-15' RETURN n",
            Parses,
        ),
        Case::new(
            "temporal",
            "for_system_time_as_of",
            "MATCH (n:Person) FOR SYSTEM_TIME AS OF '2024-01-15' RETURN n",
            Parses,
        ),
        Case::new(
            "temporal",
            "bitemporal",
            "MATCH (n:Person) AS OF VALID_TIME '2024-01-01' AS OF SYSTEM_TIME '2024-06-15' RETURN n",
            Parses,
        ),
        Case::new(
            "temporal",
            "between",
            "MATCH (n:Person) BETWEEN '2024-01-01' AND '2024-12-31' RETURN n",
            Parses,
        ),
        // #550 timestamp-format coverage: every documented AS OF TIMESTAMP
        // literal form (ISO 8601 with `Z`, ISO 8601 with numeric offset, date
        // only, and Unix microseconds) must parse+lower without error.
        Case::new(
            "temporal",
            "as_of_timestamp_offset",
            "MATCH (n:Person) AS OF TIMESTAMP '2024-01-15T10:00:00+00:00' RETURN n",
            Parses,
        ),
        Case::new(
            "temporal",
            "as_of_timestamp_date_only",
            "MATCH (n:Person) AS OF TIMESTAMP '2024-01-15' RETURN n",
            Parses,
        ),
        Case::new(
            "temporal",
            "as_of_timestamp_unix_micros",
            "MATCH (n:Person) AS OF TIMESTAMP '1705315200000000' RETURN n",
            Parses,
        ),
        // #551: the `FOR VALID_TIME AS OF` spelling (SQL:2011-style) is the
        // symmetric counterpart to `FOR SYSTEM_TIME AS OF` and lowers to a
        // valid-time AS OF context.
        Case::new(
            "temporal",
            "for_valid_time_as_of",
            "MATCH (n:Person) FOR VALID_TIME AS OF '2024-01-15' RETURN n",
            Parses,
        ),
        // ---- where null-check parse+convert (absent-prop semantics deviate;
        //      pinned in compat_known_deviations) -----------------------------
        Case::new(
            "where",
            "is_null_parses",
            "MATCH (n:Person) WHERE n.email IS NULL RETURN n",
            Parses,
        ),
        Case::new(
            "where",
            "is_not_null_parses",
            "MATCH (n:Person) WHERE n.email IS NOT NULL RETURN n",
            Parses,
        ),
        // ---- vector (convert-only; needs a seeded vector index to execute;
        //      execution is proven in tests.rs `test_e2e_vector_*`) -----------
        Case::new(
            "vector",
            "similarity_order_by",
            "MATCH (d:Document) RETURN d ORDER BY vector.similarity(d.embedding, $query) DESC LIMIT 10",
            Parses,
        )
        .with_params(vec![(
            "query",
            CypherParameterValue::Embedding(vec![0.1f32, 0.2, 0.3].into()),
        )]),
        // #554: an inline numeric vector literal is usable without a parameter.
        Case::new(
            "vector",
            "similarity_literal_embedding",
            "MATCH (d:Document) RETURN d ORDER BY vector.similarity(d.embedding, [0.1, 0.2, 0.3]) DESC LIMIT 10",
            Parses,
        ),
        // #553: each distance function name is recognized and selects a metric.
        Case::new(
            "vector",
            "cosine_order_by",
            "MATCH (d:Document) RETURN d ORDER BY vector.cosine(d.embedding, [0.1, 0.2, 0.3]) DESC LIMIT 10",
            Parses,
        ),
        Case::new(
            "vector",
            "euclidean_order_by",
            "MATCH (d:Document) RETURN d ORDER BY vector.euclidean(d.embedding, [0.1, 0.2, 0.3]) ASC LIMIT 10",
            Parses,
        ),
        Case::new(
            "vector",
            "dot_product_order_by",
            "MATCH (d:Document) RETURN d ORDER BY vector.dot_product(d.embedding, [0.1, 0.2, 0.3]) DESC LIMIT 10",
            Parses,
        ),
        // #553: similarity threshold in WHERE.
        Case::new(
            "vector",
            "similarity_threshold_where",
            "MATCH (d:Document) WHERE vector.similarity(d.embedding, [0.1, 0.2, 0.3]) > 0.8 RETURN d",
            Parses,
        ),
        // #555: aliased score projection, sortable by the alias.
        Case::new(
            "vector",
            "score_alias_projection",
            "MATCH (d:Document) RETURN d, vector.cosine(d.embedding, [0.1, 0.2, 0.3]) AS score ORDER BY score DESC LIMIT 10",
            Parses,
        ),
    ]
}

// ---------------------------------------------------------------------------
// Corpus: rejected cases (variant-precise, via parse_cypher)
// ---------------------------------------------------------------------------

fn rejected_cases() -> Vec<Case> {
    use Expect::Rejected;
    use RejectKind::{Parameter, Parse, Semantic, Temporal, Timestamp, Unsupported};

    vec![
        // ---- mutating clauses still intentionally unsupported ---------------
        // CREATE / MERGE / SET / DELETE / DETACH DELETE are now *supported* and
        // executed (see supported_execute_cases). REMOVE is deferred to a
        // follow-up (Issue #560): it needs a replace/tombstone write API the
        // native surface does not yet expose (update is PATCH-merge only) and
        // label mutation conflicts with the single-label model. It must still
        // reject cleanly rather than partially apply.
        Case::new(
            "mutating",
            "remove",
            "MATCH (n:Person) REMOVE n.age",
            Rejected(Parse, ""),
        ),
        Case::new(
            "mutating",
            "foreach",
            "FOREACH (x IN [1] | SET x.a = 1)",
            Rejected(Parse, ""),
        ),
        Case::new("mutating", "call", "CALL db.labels()", Rejected(Parse, "")),
        Case::new(
            "mutating",
            "load_csv",
            "LOAD CSV FROM 'f.csv' AS row RETURN row",
            Rejected(Parse, ""),
        ),
        // ---- structural parse errors ---------------------------------------
        Case::new(
            "structural",
            "multiple_plain_match",
            "MATCH (a:Person) MATCH (b:Person) RETURN a",
            Rejected(Parse, ""),
        ),
        Case::new(
            "structural",
            "trailing_tokens",
            "MATCH (n:Person) RETURN n EXTRA",
            Rejected(Parse, "unexpected tokens after end of statement"),
        ),
        Case::new(
            "structural",
            "count_distinct_star",
            "MATCH (n:Person) RETURN count(DISTINCT *)",
            Rejected(Parse, "DISTINCT * is not allowed"),
        ),
        // ---- optional-match unsupported shapes -----------------------------
        Case::new(
            "optional-match",
            "comma_multi_pattern",
            "MATCH (a:Person) OPTIONAL MATCH (a), (b) RETURN a",
            Rejected(
                Unsupported,
                "comma-separated patterns in an OPTIONAL MATCH clause",
            ),
        ),
        Case::new(
            "optional-match",
            "labeled_first_node",
            "MATCH (a:Person) OPTIONAL MATCH (b:Person)-[:KNOWS]->(c) RETURN c",
            Rejected(
                Unsupported,
                "on the first node of a subsequent OPTIONAL MATCH",
            ),
        ),
        // ---- aggregation unsupported shapes --------------------------------
        Case::new(
            "aggregation",
            "group_by_whole_node",
            "MATCH (n:Person) RETURN n, count(*)",
            Rejected(
                Unsupported,
                "grouping by a whole node/edge is not supported",
            ),
        ),
        // `RETURN *` is accepted only as the SOLE return item; mixing it with
        // any other item (including an aggregate) is a ParseError. The
        // converter's "RETURN * combined with aggregation" UnsupportedFeature
        // guard is unreachable through the string parser (defense-in-depth for
        // hand-built ASTs) -- see docs/guides/cypher-compatibility.md.
        Case::new(
            "aggregation",
            "return_star_mixed_with_item",
            "MATCH (n:Person) RETURN *, count(*)",
            Rejected(Parse, "unexpected tokens after end of statement"),
        ),
        // ---- with unsupported shapes ---------------------------------------
        Case::new(
            "with",
            "with_multiple_items",
            "MATCH (a:Person)-[:KNOWS]->(b) WITH a, b RETURN a",
            Rejected(Unsupported, "WITH projecting multiple items"),
        ),
        Case::new(
            "with",
            "with_computed_projection",
            "MATCH (a:Person) WITH a.name AS x RETURN x",
            Rejected(Unsupported, "WITH can only project a bound variable"),
        ),
        Case::new(
            "with",
            "return_dropped_variable",
            "MATCH (a:Person) WITH a AS p WHERE p.age > 30 RETURN a",
            Rejected(Semantic, "not in scope"),
        ),
        // ---- params --------------------------------------------------------
        Case::new(
            "params",
            "unbound_parameter",
            "MATCH (n:Person {name: $name}) RETURN n",
            Rejected(Parameter, "unbound parameter"),
        ),
        // ---- temporal: intentionally-unsupported / invalid shapes ----------
        // #551/#552 design decision: transaction-time RANGE scans are NOT
        // exposed through Cypher. `FOR SYSTEM_TIME BETWEEN` is rejected at the
        // parser (it expects `AS OF` after `SYSTEM_TIME`), never silently
        // answered. (The tx-range context is reachable only via the
        // `QueryBuilder` API, where the planner rejects it as an
        // `UnsupportedFeature`; see the executor-level e2e test.)
        Case::new(
            "temporal",
            "for_system_time_between_rejected",
            "MATCH (n:Person) FOR SYSTEM_TIME BETWEEN '2024-01-01' AND '2024-03-31' RETURN n",
            Rejected(Parse, ""),
        ),
        // #552: `BETWEEN` start/end are validated -- an inverted range
        // (start > end) is rejected during lowering with a structured
        // `InvalidTemporalClause`, not silently treated as empty.
        Case::new(
            "temporal",
            "between_inverted_range",
            "MATCH (n:Person) BETWEEN '2024-12-31' AND '2024-01-01' RETURN n",
            Rejected(Temporal, "invalid time range"),
        ),
        // #550: an unparseable timestamp literal is rejected with a structured
        // `InvalidTimestamp` during lowering (not a silent now()-fallback).
        Case::new(
            "temporal",
            "as_of_timestamp_invalid",
            "MATCH (n:Person) AS OF TIMESTAMP 'not-a-timestamp' RETURN n",
            Rejected(Timestamp, "not-a-timestamp"),
        ),
    ]
}

// ---------------------------------------------------------------------------
// Runners
// ---------------------------------------------------------------------------

/// Report a formatted panic listing every accumulated failure, or return
/// quietly when there are none.
fn assert_no_failures(runner: &str, total: usize, failures: Vec<String>) {
    if failures.is_empty() {
        return;
    }
    let mut msg = format!(
        "\n{} compatibility failures: {} of {} cases failed\n",
        runner,
        failures.len(),
        total
    );
    for (i, f) in failures.iter().enumerate() {
        msg.push_str(&format!("  [{}] {}\n", i + 1, f));
    }
    panic!("{msg}");
}

#[test]
fn compat_supported_execute() {
    let cases = supported_execute_cases();
    let total = cases.len();
    let mut failures = Vec::new();

    for case in &cases {
        // A FRESH seed graph per case (Issue #560): the corpus now includes
        // executing write cases (CREATE / SET / DELETE / DETACH DELETE) that
        // mutate the graph, so cases must not share state. Read-only cases are
        // unaffected -- the seed is deterministic.
        let db = compat_db();
        let result = if case.params.is_empty() {
            db.execute_cypher(case.query)
        } else {
            db.execute_cypher_with_params(case.query, case.param_map())
        };

        let rows = match result.and_then(|r| r.collect_all()) {
            Ok(rows) => rows,
            Err(e) => {
                failures.push(format!(
                    "[{}/{}] query `{}` expected to execute but errored: {e}",
                    case.category, case.name, case.query
                ));
                continue;
            }
        };
        let got = rows.len();

        match case.expect {
            Expect::Executes(want) => {
                if got != want {
                    failures.push(format!(
                        "[{}/{}] query `{}` expected {want} rows, got {got}",
                        case.category, case.name, case.query
                    ));
                }
            }
            Expect::ExecutesAtLeast(min) => {
                if got < min {
                    failures.push(format!(
                        "[{}/{}] query `{}` expected >= {min} rows, got {got}",
                        case.category, case.name, case.query
                    ));
                }
            }
            _ => unreachable!("supported_execute_cases only holds Executes/ExecutesAtLeast"),
        }
    }

    assert_no_failures("supported-execute", total, failures);
}

#[test]
fn compat_supported_parse() {
    let cases = supported_parse_cases();
    let total = cases.len();
    let mut failures = Vec::new();

    for case in &cases {
        let result = if case.params.is_empty() {
            parse_cypher(case.query)
        } else {
            parse_cypher_with_params(case.query, case.param_map())
        };

        match case.expect {
            Expect::Parses => {
                if let Err(e) = result {
                    failures.push(format!(
                        "[{}/{}] query `{}` expected to parse+convert but errored: {e}",
                        case.category, case.name, case.query
                    ));
                }
            }
            _ => unreachable!("supported_parse_cases only holds Parses"),
        }
    }

    assert_no_failures("supported-parse", total, failures);
}

#[test]
fn compat_rejected() {
    let cases = rejected_cases();
    let total = cases.len();
    let mut failures = Vec::new();

    for case in &cases {
        let result = if case.params.is_empty() {
            parse_cypher(case.query)
        } else {
            parse_cypher_with_params(case.query, case.param_map())
        };

        let Expect::Rejected(kind, substr) = case.expect else {
            unreachable!("rejected_cases only holds Rejected");
        };

        match result {
            Ok(_) => failures.push(format!(
                "[{}/{}] query `{}` expected {kind:?} rejection but parsed successfully",
                case.category, case.name, case.query
            )),
            Err(e) => {
                if !kind.matches(&e) {
                    failures.push(format!(
                        "[{}/{}] query `{}` expected {kind:?}, got: {e}",
                        case.category, case.name, case.query
                    ));
                } else if !substr.is_empty() && !e.to_string().contains(substr) {
                    failures.push(format!(
                        "[{}/{}] query `{}` got {kind:?} but message `{e}` missing substring `{substr}`",
                        case.category, case.name, case.query
                    ));
                }
            }
        }
    }

    assert_no_failures("rejected", total, failures);
}
