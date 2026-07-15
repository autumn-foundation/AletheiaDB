//! Tests for the Neo4j APOC Cypher-script dump importer (Issue #3356).

use std::io::Write;
use std::path::PathBuf;

use tempfile::TempDir;

use super::neo4j::LabelStrategy;
use super::neo4j_cypher::Neo4jCypherOptions;
use super::*;
use crate::core::property::PropertyValue;
use crate::core::temporal::Timestamp;
use crate::test_utils::create_test_db;

/// Write `contents` to `name` inside `dir`, returning its path.
fn write_file(dir: &TempDir, name: &str, contents: &str) -> PathBuf {
    let path = dir.path().join(name);
    let mut file = std::fs::File::create(&path).expect("create temp file");
    file.write_all(contents.as_bytes())
        .expect("write temp file");
    path
}

/// Path to a committed golden fixture under `src/api/import/fixtures/`.
fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("src/api/import/fixtures")
        .join(name)
}

/// Resolve the node imported for a `UNIQUE IMPORT ID` key.
fn resolve(importer: &Importer<'_>, id: &str) -> NodeId {
    importer
        .resolve_key(id)
        .unwrap_or_else(|| panic!("key '{id}' should resolve"))
}

// A minimal, realistic apoc.export.cypher.all node+rel dump (no scaffolding).
const BASIC: &str = "\
:begin
CREATE (:`Person`:`UNIQUE IMPORT LABEL` {`name`:\"Alice\", `age`:30, `active`:true, `UNIQUE IMPORT ID`:0});
CREATE (:`Person`:`UNIQUE IMPORT LABEL` {`name`:\"Bob\", `age`:25, `score`:9.5, `UNIQUE IMPORT ID`:1});
CREATE (:`City`:`UNIQUE IMPORT LABEL` {`name`:\"Paris\", `UNIQUE IMPORT ID`:2});
:commit
:begin
MATCH (n1:`UNIQUE IMPORT LABEL`{`UNIQUE IMPORT ID`:0}), (n2:`UNIQUE IMPORT LABEL`{`UNIQUE IMPORT ID`:1}) CREATE (n1)-[:`KNOWS` {`since`:2020}]->(n2);
MATCH (n1:`UNIQUE IMPORT LABEL`{`UNIQUE IMPORT ID`:0}), (n2:`UNIQUE IMPORT LABEL`{`UNIQUE IMPORT ID`:2}) CREATE (n1)-[:`LIVES_IN`]->(n2);
:commit
";

// ---- Happy path (AC1, AC2) -------------------------------------------------

#[test]
fn nodes_and_relationships_round_trip() {
    let files = TempDir::new().unwrap();
    let (_tmp, db) = create_test_db().unwrap();
    let path = write_file(&files, "dump.cypher", BASIC);

    let mut importer = db.import();
    let report = importer
        .neo4j_import_cypher(&path, &Neo4jCypherOptions::new())
        .expect("import");

    assert_eq!(report.nodes_imported, 3);
    assert_eq!(report.relationships_imported, 2);
    assert!(report.zero_loss, "report: {report:?}");
    assert_eq!(db.node_count(), 3);

    let alice = resolve(&importer, "0");
    let node = db.get_node(alice).unwrap();
    assert!(node.has_label_str("Person"));
    assert_eq!(
        node.properties.get("name"),
        Some(&PropertyValue::string("Alice"))
    );
    assert_eq!(node.properties.get("age"), Some(&PropertyValue::Int(30)));
    assert_eq!(
        node.properties.get("active"),
        Some(&PropertyValue::Bool(true))
    );
    // UNIQUE IMPORT ID is consumed as the key, never stored.
    assert_eq!(node.properties.get("UNIQUE IMPORT ID"), None);

    let bob = resolve(&importer, "1");
    let bob_node = db.get_node(bob).unwrap();
    assert_eq!(
        bob_node.properties.get("score"),
        Some(&PropertyValue::Float(9.5))
    );

    // KNOWS edge Alice -> Bob with since=2020.
    let knows = db.get_outgoing_edges_with_label(alice, "KNOWS");
    assert_eq!(knows.len(), 1);
    assert_eq!(db.get_edge_target(knows[0]).unwrap(), bob);
    assert_eq!(
        db.get_edge(knows[0]).unwrap().properties.get("since"),
        Some(&PropertyValue::Int(2020))
    );
    let lives = db.get_outgoing_edges_with_label(alice, "LIVES_IN");
    assert_eq!(lives.len(), 1);
}

// Golden fixture: read a committed file (not an inline string).
#[test]
fn golden_fixture_round_trip() {
    let (_tmp, db) = create_test_db().unwrap();
    let path = fixture("apoc_basic.cypher");
    let mut importer = db.import();
    let report = importer
        .neo4j_import_cypher(&path, &Neo4jCypherOptions::new())
        .expect("golden import");
    assert_eq!(report.nodes_imported, 3);
    assert_eq!(report.relationships_imported, 2);
    assert!(report.zero_loss, "golden should be zero-loss: {report:?}");

    // Provenance stamps the fixture basename.
    let alice = resolve(&importer, "0");
    let prov = db.get_node_provenance(alice).unwrap().expect("provenance");
    assert_eq!(prov.source(), Some("neo4j-import::apoc_basic.cypher"));
}

// ---- Multi-label rule (AC2) ------------------------------------------------

#[test]
fn multi_label_first_wins_and_labels_preserved() {
    let files = TempDir::new().unwrap();
    let (_tmp, db) = create_test_db().unwrap();
    let dump = "CREATE (:`Person`:`Employee`:`Manager` {`name`:\"Al\", `UNIQUE IMPORT ID`:0});\n";
    let path = write_file(&files, "d.cypher", dump);
    let mut importer = db.import();
    importer
        .neo4j_import_cypher(&path, &Neo4jCypherOptions::new())
        .expect("import");
    let node = db.get_node(resolve(&importer, "0")).unwrap();
    assert!(node.has_label_str("Person"));
    let labels = node.properties.get("_labels").unwrap();
    let arr = labels.as_array().unwrap();
    assert_eq!(arr.len(), 3);
    assert_eq!(arr[0], PropertyValue::string("Person"));
    assert_eq!(arr[2], PropertyValue::string("Manager"));
}

// ---- Value coverage (AC2) --------------------------------------------------

#[test]
fn value_literals_parse() {
    let files = TempDir::new().unwrap();
    let (_tmp, db) = create_test_db().unwrap();
    let dump = "CREATE (:`T`:`UNIQUE IMPORT LABEL` {\
        `s`:\"hi\\nthere\\t\\\"q\\\"\\u0041\", \
        `i`:-7, `f`:1.5e3, `b`:false, `n`:null, \
        `tags`:[\"a\",\"b\"], `nums`:[1,2,3], \
        `bt`:`odd key`, `UNIQUE IMPORT ID`:0});\n";
    // Note: `bt`:`odd key` — backtick identifier as a *value* is invalid Cypher,
    // so use a string instead below; keep the escape/number/list coverage.
    let dump = dump.replace("`bt`:`odd key`, ", "");
    let path = write_file(&files, "d.cypher", &dump);
    let mut importer = db.import();
    importer
        .neo4j_import_cypher(&path, &Neo4jCypherOptions::new())
        .expect("import");
    let node = db.get_node(resolve(&importer, "0")).unwrap();
    assert_eq!(
        node.properties.get("s"),
        Some(&PropertyValue::string("hi\nthere\t\"q\"A"))
    );
    assert_eq!(node.properties.get("i"), Some(&PropertyValue::Int(-7)));
    assert_eq!(
        node.properties.get("f"),
        Some(&PropertyValue::Float(1500.0))
    );
    assert_eq!(node.properties.get("b"), Some(&PropertyValue::Bool(false)));
    // null -> absent.
    assert_eq!(node.properties.get("n"), None);
    let tags = node.properties.get("tags").unwrap().as_array().unwrap();
    assert_eq!(tags.len(), 2);
    // Numeric array WITHOUT the vector flag stays an Array, not a Vector.
    let nums = node.properties.get("nums").unwrap();
    assert!(nums.as_array().is_some(), "nums must be an Array");
    assert!(nums.as_vector().is_none(), "nums must NOT be a Vector");
}

#[test]
fn backtick_identifier_property_name() {
    let files = TempDir::new().unwrap();
    let (_tmp, db) = create_test_db().unwrap();
    let dump = "CREATE (:`P`:`UNIQUE IMPORT LABEL` {`odd key`:\"v\", `UNIQUE IMPORT ID`:0});\n";
    let path = write_file(&files, "d.cypher", dump);
    let mut importer = db.import();
    importer
        .neo4j_import_cypher(&path, &Neo4jCypherOptions::new())
        .expect("import");
    let node = db.get_node(resolve(&importer, "0")).unwrap();
    assert_eq!(
        node.properties.get("odd key"),
        Some(&PropertyValue::string("v"))
    );
}

#[test]
fn numeric_array_becomes_vector_only_with_flag() {
    let files = TempDir::new().unwrap();
    let (_tmp, db) = create_test_db().unwrap();
    let dump =
        "CREATE (:`Doc`:`UNIQUE IMPORT LABEL` {`emb`:[0.1,0.2,0.3], `UNIQUE IMPORT ID`:0});\n";
    let path = write_file(&files, "d.cypher", dump);
    let opts = Neo4jCypherOptions::new().vector_property("emb");
    let mut importer = db.import();
    importer.neo4j_import_cypher(&path, &opts).expect("import");
    let node = db.get_node(resolve(&importer, "0")).unwrap();
    let emb = node.properties.get("emb").unwrap();
    assert!(
        emb.as_vector().is_some(),
        "emb should be a Vector with flag"
    );
    assert_eq!(emb.as_vector().unwrap().len(), 3);
}

// ---- Transaction markers (AC1) ---------------------------------------------

#[test]
fn transaction_markers_ignored() {
    let files = TempDir::new().unwrap();
    let (_tmp, db) = create_test_db().unwrap();
    let dump = "\
:begin
CREATE (:`P`:`UNIQUE IMPORT LABEL` {`UNIQUE IMPORT ID`:0});
:rollback
:commit
:begin
CREATE (:`P`:`UNIQUE IMPORT LABEL` {`UNIQUE IMPORT ID`:1});
:commit
";
    let path = write_file(&files, "d.cypher", dump);
    let mut importer = db.import();
    let report = importer
        .neo4j_import_cypher(&path, &Neo4jCypherOptions::new())
        .expect("import");
    assert_eq!(report.nodes_imported, 2);
    assert!(report.zero_loss);
}

// ---- UNWIND batch form (AC1) -----------------------------------------------

#[test]
fn unwind_node_and_relationship_batches() {
    let files = TempDir::new().unwrap();
    let (_tmp, db) = create_test_db().unwrap();
    let dump = "\
:begin
UNWIND [{_id:0, properties:{name:\"Alice\"}}, {_id:1, properties:{name:\"Bob\"}}] AS row CREATE (n:`Person`{`UNIQUE IMPORT ID`: row._id}) SET n += row.properties;
:commit
:begin
UNWIND [{start:{_id:0}, end:{_id:1}, properties:{since:2021}}] AS row MATCH (start:`UNIQUE IMPORT LABEL`{`UNIQUE IMPORT ID`: row.start._id}) MATCH (end:`UNIQUE IMPORT LABEL`{`UNIQUE IMPORT ID`: row.end._id}) CREATE (start)-[r:`KNOWS`]->(end) SET r += row.properties;
:commit
";
    let path = write_file(&files, "d.cypher", dump);
    let mut importer = db.import();
    let report = importer
        .neo4j_import_cypher(&path, &Neo4jCypherOptions::new())
        .expect("import");
    assert_eq!(report.nodes_imported, 2);
    assert_eq!(report.relationships_imported, 1);
    assert!(report.zero_loss, "{report:?}");

    let alice = resolve(&importer, "0");
    assert_eq!(
        db.get_node(alice).unwrap().properties.get("name"),
        Some(&PropertyValue::string("Alice"))
    );
    let knows = db.get_outgoing_edges_with_label(alice, "KNOWS");
    assert_eq!(knows.len(), 1);
    assert_eq!(
        db.get_edge(knows[0]).unwrap().properties.get("since"),
        Some(&PropertyValue::Int(2021))
    );
}

// ---- Unsupported constructs (AC8) ------------------------------------------

#[test]
fn user_constraint_and_index_reported_not_dropped() {
    let files = TempDir::new().unwrap();
    let (_tmp, db) = create_test_db().unwrap();
    let dump = "\
CREATE (:`Person`:`UNIQUE IMPORT LABEL` {`UNIQUE IMPORT ID`:0});
CREATE CONSTRAINT ON (p:`Person`) ASSERT (p.`email`) IS UNIQUE;
CREATE INDEX FOR (p:`Person`) ON (p.`name`);
CALL db.myProc();
";
    let path = write_file(&files, "d.cypher", dump);
    let mut importer = db.import();
    let report = importer
        .neo4j_import_cypher(&path, &Neo4jCypherOptions::new())
        .expect("import");
    assert_eq!(report.nodes_imported, 1);
    assert!(!report.zero_loss, "unsupported present -> not zero-loss");
    // Three distinct unsupported constructs, each counted.
    let kinds: Vec<&str> = report
        .unsupported
        .iter()
        .map(|u| u.column.as_str())
        .collect();
    assert!(kinds.contains(&"CREATE CONSTRAINT"), "{kinds:?}");
    assert!(kinds.contains(&"CREATE INDEX"), "{kinds:?}");
    assert!(kinds.contains(&"CALL"), "{kinds:?}");
}

#[test]
fn apoc_scaffolding_is_lossless() {
    let (_tmp, db) = create_test_db().unwrap();
    let path = fixture("apoc_with_scaffolding.cypher");
    let mut importer = db.import();
    let report = importer
        .neo4j_import_cypher(&path, &Neo4jCypherOptions::new())
        .expect("import");
    assert!(
        report.zero_loss,
        "apoc's own UNIQUE IMPORT scaffolding must not count as loss: {report:?}"
    );
    assert!(report.unsupported.is_empty());
    assert_eq!(report.nodes_imported, 2);
    assert_eq!(report.relationships_imported, 1);
}

// ---- Unsupported values (AC8) ----------------------------------------------

#[test]
fn point_and_duration_values_reported() {
    let files = TempDir::new().unwrap();
    let (_tmp, db) = create_test_db().unwrap();
    let dump = "CREATE (:`Loc`:`UNIQUE IMPORT LABEL` {`name`:\"X\", \
        `pos`:point({`x`:1.0, `y`:2.0}), `dur`:duration('P1Y'), `UNIQUE IMPORT ID`:0});\n";
    let path = write_file(&files, "d.cypher", dump);
    let mut importer = db.import();
    let report = importer
        .neo4j_import_cypher(&path, &Neo4jCypherOptions::new())
        .expect("import");
    assert_eq!(report.nodes_imported, 1);
    assert!(!report.zero_loss);
    // The node still imports, minus the unsupported properties.
    let node = db.get_node(resolve(&importer, "0")).unwrap();
    assert_eq!(
        node.properties.get("name"),
        Some(&PropertyValue::string("X"))
    );
    assert_eq!(node.properties.get("pos"), None);
    assert_eq!(node.properties.get("dur"), None);
    let types: Vec<&str> = report
        .unsupported
        .iter()
        .map(|u| u.neo4j_type.as_str())
        .collect();
    assert!(types.contains(&"point"), "{types:?}");
    assert!(types.contains(&"duration"), "{types:?}");
}

#[test]
fn strict_types_rejects_unsupported_value() {
    let files = TempDir::new().unwrap();
    let (_tmp, db) = create_test_db().unwrap();
    let dump =
        "CREATE (:`Loc`:`UNIQUE IMPORT LABEL` {`pos`:point({`x`:1.0}), `UNIQUE IMPORT ID`:0});\n";
    let path = write_file(&files, "d.cypher", dump);
    let opts = Neo4jCypherOptions::new().strict_types(true);
    let mut importer = db.import();
    let err = importer
        .neo4j_import_cypher(&path, &opts)
        .expect_err("strict types should hard-error on point()");
    assert!(err.to_string().contains("point"), "was: {err}");
}

// ---- Temporal values (AC2) -------------------------------------------------

#[test]
fn temporal_constructors_map_to_micros() {
    let files = TempDir::new().unwrap();
    let (_tmp, db) = create_test_db().unwrap();
    let dump = "CREATE (:`E`:`UNIQUE IMPORT LABEL` \
        {`at`:datetime('2024-01-15T00:00:00Z'), `UNIQUE IMPORT ID`:0});\n";
    let path = write_file(&files, "d.cypher", dump);
    let mut importer = db.import();
    importer
        .neo4j_import_cypher(&path, &Neo4jCypherOptions::new())
        .expect("import");
    let node = db.get_node(resolve(&importer, "0")).unwrap();
    // 2024-01-15T00:00:00Z == 1_705_276_800_000_000 micros.
    assert_eq!(
        node.properties.get("at"),
        Some(&PropertyValue::Int(1_705_276_800_000_000))
    );
}

// ---- Valid-time (AC6) ------------------------------------------------------

#[test]
fn valid_from_property_backdates_and_is_not_stored() {
    let files = TempDir::new().unwrap();
    let (_tmp, db) = create_test_db().unwrap();
    let dump = "CREATE (:`Person`:`UNIQUE IMPORT LABEL` \
        {`name`:\"A\", `since`:datetime('2020-01-01T00:00:00Z'), `UNIQUE IMPORT ID`:0});\n";
    let path = write_file(&files, "d.cypher", dump);
    let opts = Neo4jCypherOptions::new().valid_from_property("since");
    let mut importer = db.import();
    importer.neo4j_import_cypher(&path, &opts).expect("import");
    let a = resolve(&importer, "0");
    let node = db.get_node(a).unwrap();
    assert_eq!(node.properties.get("since"), None);

    let ts_2021 = Timestamp::from(1_609_459_200_000_000i64);
    let ts_2019 = Timestamp::from(1_546_300_800_000_000i64);
    assert!(db.get_node_at_valid_time(a, ts_2021).is_ok());
    assert!(db.get_node_at_valid_time(a, ts_2019).is_err());
}

// ---- Provenance (AC5) ------------------------------------------------------

#[test]
fn provenance_stamped_on_nodes_and_edges() {
    let files = TempDir::new().unwrap();
    let (_tmp, db) = create_test_db().unwrap();
    let path = write_file(&files, "graph.cypher", BASIC);
    let mut importer = db.import();
    importer
        .neo4j_import_cypher(&path, &Neo4jCypherOptions::new())
        .expect("import");
    let alice = resolve(&importer, "0");
    let prov = db.get_node_provenance(alice).unwrap().expect("node prov");
    assert_eq!(prov.source(), Some("neo4j-import::graph.cypher"));
    let edge = db.get_outgoing_edges_with_label(alice, "KNOWS")[0];
    let eprov = db.get_edge_provenance(edge).unwrap().expect("edge prov");
    assert_eq!(eprov.source(), Some("neo4j-import::graph.cypher"));
}

// ---- Failure modes + error locations (AC3) ---------------------------------

#[test]
fn abort_mode_stops_on_malformed_statement() {
    let files = TempDir::new().unwrap();
    let (_tmp, db) = create_test_db().unwrap();
    let dump = "\
CREATE (:`P`:`UNIQUE IMPORT LABEL` {`name`:\"ok\", `UNIQUE IMPORT ID`:0});
CREATE (:`P`:`UNIQUE IMPORT LABEL` {`name`:, `UNIQUE IMPORT ID`:1});
";
    let path = write_file(&files, "d.cypher", dump);
    let mut importer = db.import();
    let err = importer
        .neo4j_import_cypher(&path, &Neo4jCypherOptions::new())
        .expect_err("malformed value should abort");
    // Error names the file and the offending line (statement 2 begins on line 2).
    let msg = err.to_string();
    assert!(msg.contains("d.cypher"), "was: {msg}");
    assert!(
        msg.contains("row 2") || msg.contains("line 2"),
        "was: {msg}"
    );
}

#[test]
fn skip_and_report_collects_malformed_with_location() {
    let files = TempDir::new().unwrap();
    let (_tmp, db) = create_test_db().unwrap();
    let dump = "\
CREATE (:`P`:`UNIQUE IMPORT LABEL` {`name`:\"ok\", `UNIQUE IMPORT ID`:0});
CREATE (:`P`:`UNIQUE IMPORT LABEL` {`name`:, `UNIQUE IMPORT ID`:1});
CREATE (:`P`:`UNIQUE IMPORT LABEL` {`name`:\"ok2\", `UNIQUE IMPORT ID`:2});
";
    let path = write_file(&files, "d.cypher", dump);
    let mut importer = db.import().failure_mode(FailureMode::SkipAndReport);
    let report = importer
        .neo4j_import_cypher(&path, &Neo4jCypherOptions::new())
        .expect("skip mode should not error");
    assert_eq!(report.nodes_imported, 2);
    assert_eq!(report.skipped.len(), 1);
    assert_eq!(report.skipped[0].row, 2);
    assert_eq!(report.skipped[0].file.as_deref(), Some("d.cypher"));
    assert!(!report.zero_loss);
}

#[test]
fn unresolved_relationship_endpoint_reported() {
    let files = TempDir::new().unwrap();
    let (_tmp, db) = create_test_db().unwrap();
    let dump = "\
CREATE (:`P`:`UNIQUE IMPORT LABEL` {`UNIQUE IMPORT ID`:0});
MATCH (a:`UNIQUE IMPORT LABEL`{`UNIQUE IMPORT ID`:0}), (b:`UNIQUE IMPORT LABEL`{`UNIQUE IMPORT ID`:999}) CREATE (a)-[:`KNOWS`]->(b);
";
    let path = write_file(&files, "d.cypher", dump);
    let mut importer = db.import().failure_mode(FailureMode::SkipAndReport);
    let report = importer
        .neo4j_import_cypher(&path, &Neo4jCypherOptions::new())
        .expect("skip mode");
    assert_eq!(report.nodes_imported, 1);
    assert_eq!(report.relationships_imported, 0);
    assert_eq!(report.unresolved_endpoints.len(), 1);
    assert_eq!(report.unresolved_endpoints[0].key, "999");
    assert!(!report.zero_loss);
}

// ---- Report serialization (AC4) --------------------------------------------

#[test]
fn report_serializes_to_json() {
    let files = TempDir::new().unwrap();
    let (_tmp, db) = create_test_db().unwrap();
    let path = write_file(&files, "d.cypher", BASIC);
    let mut importer = db.import();
    let report = importer
        .neo4j_import_cypher(&path, &Neo4jCypherOptions::new())
        .expect("import");
    let json = serde_json::to_string(&report).expect("serialize");
    assert!(json.contains("\"nodes_imported\":3"));
    assert!(json.contains("\"relationships_imported\":2"));
    assert!(json.contains("\"zero_loss\":true"));
    assert!(json.contains("label_mapping"));
    assert!(json.contains("type_mapping"));
}

// ---- Adversarial (no panics) -----------------------------------------------

#[test]
fn truncated_dump_errors_cleanly() {
    let (_tmp, db) = create_test_db().unwrap();
    let path = fixture("apoc_truncated.cypher");
    let mut importer = db.import();
    let err = importer
        .neo4j_import_cypher(&path, &Neo4jCypherOptions::new())
        .expect_err("truncated dump must error");
    assert!(err.to_string().contains("truncated"), "was: {err}");
}

#[test]
fn bad_string_escape_errors_not_panics() {
    let files = TempDir::new().unwrap();
    let (_tmp, db) = create_test_db().unwrap();
    let dump = "CREATE (:`P`:`UNIQUE IMPORT LABEL` {`s`:\"bad\\q\", `UNIQUE IMPORT ID`:0});\n";
    let path = write_file(&files, "d.cypher", dump);
    let mut importer = db.import();
    let err = importer
        .neo4j_import_cypher(&path, &Neo4jCypherOptions::new())
        .expect_err("bad escape should error");
    assert!(err.to_string().contains("escape"), "was: {err}");
}

#[test]
fn unbalanced_brackets_error_not_panics() {
    let files = TempDir::new().unwrap();
    let (_tmp, db) = create_test_db().unwrap();
    // Missing closing brace -> the statement never terminates -> truncated.
    let dump = "CREATE (:`P`:`UNIQUE IMPORT LABEL` {`name`:\"x\", `UNIQUE IMPORT ID`:0);\n";
    let path = write_file(&files, "d.cypher", dump);
    let mut importer = db.import();
    let err = importer
        .neo4j_import_cypher(&path, &Neo4jCypherOptions::new())
        .expect_err("unbalanced brace should error");
    // Either a truncation or a parse error; never a panic.
    assert!(!err.to_string().is_empty());
}

#[test]
fn huge_property_value_does_not_panic() {
    let files = TempDir::new().unwrap();
    let (_tmp, db) = create_test_db().unwrap();
    let big = "a".repeat(1_000_000);
    let dump =
        format!("CREATE (:`P`:`UNIQUE IMPORT LABEL` {{`s`:\"{big}\", `UNIQUE IMPORT ID`:0}});\n");
    let path = write_file(&files, "d.cypher", &dump);
    let mut importer = db.import();
    let report = importer
        .neo4j_import_cypher(&path, &Neo4jCypherOptions::new())
        .expect("huge value should import, not panic");
    assert_eq!(report.nodes_imported, 1);
    let node = db.get_node(resolve(&importer, "0")).unwrap();
    assert_eq!(
        node.properties.get("s").unwrap().as_str().unwrap().len(),
        1_000_000
    );
}

// ---- Scale smoke (AC7) -----------------------------------------------------

#[test]
fn scale_smoke_10k_nodes_50k_rels() {
    let files = TempDir::new().unwrap();
    let (_tmp, db) = create_test_db().unwrap();
    let n = 10_000usize;
    let mut buf = String::with_capacity(4 * 1024 * 1024);
    buf.push_str(":begin\n");
    for i in 0..n {
        buf.push_str(&format!(
            "CREATE (:`Person`:`UNIQUE IMPORT LABEL` {{`name`:\"N{i}\", `age`:{}, `UNIQUE IMPORT ID`:{i}}});\n",
            i % 100
        ));
    }
    buf.push_str(":commit\n:begin\n");
    for i in 0..n {
        for k in 1..=5 {
            let tgt = (i + k) % n;
            buf.push_str(&format!(
                "MATCH (a:`UNIQUE IMPORT LABEL`{{`UNIQUE IMPORT ID`:{i}}}), (b:`UNIQUE IMPORT LABEL`{{`UNIQUE IMPORT ID`:{tgt}}}) CREATE (a)-[:`KNOWS`]->(b);\n"
            ));
        }
    }
    buf.push_str(":commit\n");
    let path = write_file(&files, "scale.cypher", &buf);
    let mut importer = db.import();
    let report = importer
        .neo4j_import_cypher(&path, &Neo4jCypherOptions::new())
        .expect("scale import");
    assert_eq!(report.nodes_imported, n);
    assert_eq!(report.relationships_imported, n * 5);
    assert!(report.zero_loss, "{:?}", &report.unsupported);
    assert_eq!(db.node_count(), n);
}

// ---- Label strategy variants -----------------------------------------------

#[test]
fn label_strategy_concat() {
    let files = TempDir::new().unwrap();
    let (_tmp, db) = create_test_db().unwrap();
    let dump = "CREATE (:`Person`:`Employee` {`UNIQUE IMPORT ID`:0});\n";
    let path = write_file(&files, "d.cypher", dump);
    let opts = Neo4jCypherOptions::new().label_strategy(LabelStrategy::Concat);
    let mut importer = db.import();
    importer.neo4j_import_cypher(&path, &opts).expect("import");
    let node = db.get_node(resolve(&importer, "0")).unwrap();
    assert!(node.has_label_str("Person_Employee"));
}

#[test]
fn label_less_node_gets_default_label() {
    let files = TempDir::new().unwrap();
    let (_tmp, db) = create_test_db().unwrap();
    // Only the synthetic label -> a label-less Neo4j node.
    let dump = "CREATE (:`UNIQUE IMPORT LABEL` {`name`:\"x\", `UNIQUE IMPORT ID`:0});\n";
    let path = write_file(&files, "d.cypher", dump);
    let mut importer = db.import();
    importer
        .neo4j_import_cypher(&path, &Neo4jCypherOptions::new())
        .expect("import");
    let node = db.get_node(resolve(&importer, "0")).unwrap();
    assert!(node.has_label_str("Node"));
}
