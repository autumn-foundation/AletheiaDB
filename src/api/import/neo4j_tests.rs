//! Tests for the Neo4j CSV migration importer (Issue #3356).

use std::io::Write;
use std::path::PathBuf;

use tempfile::TempDir;

use super::neo4j::{LabelStrategy, Neo4jCsvOptions};
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

/// Business key as stored by the Neo4j importer (group-namespaced; `""` = global).
fn nkey(group: &str, id: &str) -> String {
    format!("{group}\0{id}")
}

fn resolve(importer: &Importer<'_>, group: &str, id: &str) -> NodeId {
    importer
        .resolve_key(&nkey(group, id))
        .unwrap_or_else(|| panic!("key '{id}' (group '{group}') should resolve"))
}

// ---- Header parsing --------------------------------------------------------

// 1 + 8: bare :ID (id not stored as property), untyped column defaults to string.
#[test]
fn bare_id_not_stored_untyped_defaults_string() {
    let files = TempDir::new().unwrap();
    let (_tmp, db) = create_test_db().unwrap();
    let nodes = write_file(&files, "nodes.csv", ":ID,:LABEL,name\nalice,Person,Alice\n");

    let mut importer = db.import();
    let report = importer
        .neo4j_nodes_from_csv(&nodes, &Neo4jCsvOptions::new())
        .expect("nodes import");
    assert_eq!(report.nodes_imported, 1);
    assert!(report.zero_loss);

    let alice = resolve(&importer, "", "alice");
    let node = db.get_node(alice).unwrap();
    // Untyped `name` stored as string.
    assert_eq!(
        node.properties.get("name"),
        Some(&PropertyValue::string("Alice"))
    );
    // Bare :ID is NOT stored as a property (no key by any obvious name).
    assert_eq!(node.properties.get(":ID"), None);
    assert_eq!(node.properties.get("id"), None);
}

// 2: named id column (personId:ID) also stores the id as a property.
#[test]
fn named_id_column_stores_id_property() {
    let files = TempDir::new().unwrap();
    let (_tmp, db) = create_test_db().unwrap();
    let nodes = write_file(&files, "nodes.csv", "personId:ID,:LABEL\nalice,Person\n");

    let mut importer = db.import();
    importer
        .neo4j_nodes_from_csv(&nodes, &Neo4jCsvOptions::new())
        .expect("nodes import");
    let alice = resolve(&importer, "", "alice");
    let node = db.get_node(alice).unwrap();
    assert_eq!(
        node.properties.get("personId"),
        Some(&PropertyValue::string("alice"))
    );
}

// 3: id-spaces — same raw id in two groups does not collide.
#[test]
fn id_spaces_do_not_collide() {
    let files = TempDir::new().unwrap();
    let (_tmp, db) = create_test_db().unwrap();
    let people = write_file(&files, "people.csv", ":ID(Person),:LABEL\nx1,Person\n");
    let places = write_file(&files, "places.csv", ":ID(Place),:LABEL\nx1,Place\n");
    let rels = write_file(
        &files,
        "rels.csv",
        ":START_ID(Person),:END_ID(Place),:TYPE\nx1,x1,LIVES_IN\n",
    );

    let mut importer = db.import();
    let report = importer
        .neo4j_import_csv(&[people, places], &[rels], &Neo4jCsvOptions::new())
        .expect("import");
    assert_eq!(report.nodes_imported, 2);
    assert_eq!(report.relationships_imported, 1);
    assert!(report.zero_loss);

    let person = resolve(&importer, "Person", "x1");
    let place = resolve(&importer, "Place", "x1");
    assert_ne!(person, place);
    let out = db.get_outgoing_edges(person);
    assert_eq!(out.len(), 1);
    assert_eq!(db.get_edge_target(out[0]).unwrap(), place);
}

// 4: missing :ID in a node file -> hard error.
#[test]
fn node_file_missing_id_errors() {
    let files = TempDir::new().unwrap();
    let (_tmp, db) = create_test_db().unwrap();
    let nodes = write_file(&files, "nodes.csv", ":LABEL,name\nPerson,Alice\n");
    let mut importer = db.import();
    let err = importer
        .neo4j_nodes_from_csv(&nodes, &Neo4jCsvOptions::new())
        .expect_err("should error");
    assert!(err.to_string().contains(":ID"), "was: {err}");
}

// 5: rel file missing :START_ID / :END_ID / :TYPE -> hard error naming the field.
#[test]
fn rel_file_missing_fields_errors() {
    let files = TempDir::new().unwrap();
    let (_tmp, db) = create_test_db().unwrap();
    let rels = write_file(&files, "rels.csv", ":START_ID,:END_ID\na,b\n");
    let mut importer = db.import();
    let err = importer
        .neo4j_edges_from_csv(&rels, &Neo4jCsvOptions::new())
        .expect_err("should error");
    assert!(err.to_string().contains(":TYPE"), "was: {err}");
}

// 6: :IGNORE column fully dropped.
#[test]
fn ignore_column_dropped() {
    let files = TempDir::new().unwrap();
    let (_tmp, db) = create_test_db().unwrap();
    let nodes = write_file(
        &files,
        "nodes.csv",
        ":ID,:LABEL,secret:IGNORE,name\na,Person,xxx,Alice\n",
    );
    let mut importer = db.import();
    importer
        .neo4j_nodes_from_csv(&nodes, &Neo4jCsvOptions::new())
        .expect("import");
    let node = db.get_node(resolve(&importer, "", "a")).unwrap();
    assert_eq!(node.properties.get("secret"), None);
    assert_eq!(
        node.properties.get("name"),
        Some(&PropertyValue::string("Alice"))
    );
}

// 30: truly empty file (no header) -> hard error.
#[test]
fn empty_file_no_header_errors() {
    let files = TempDir::new().unwrap();
    let (_tmp, db) = create_test_db().unwrap();
    let nodes = write_file(&files, "empty.csv", "");
    let mut importer = db.import();
    let err = importer
        .neo4j_nodes_from_csv(&nodes, &Neo4jCsvOptions::new())
        .expect_err("should error");
    let msg = err.to_string();
    assert!(msg.contains("header") || msg.contains(":ID"), "was: {msg}");
}

// 29: header-only file (0 data rows) -> rows_read=0, zero_loss=true.
#[test]
fn header_only_file_zero_rows() {
    let files = TempDir::new().unwrap();
    let (_tmp, db) = create_test_db().unwrap();
    let nodes = write_file(&files, "nodes.csv", ":ID,:LABEL,name\n");
    let mut importer = db.import();
    let report = importer
        .neo4j_nodes_from_csv(&nodes, &Neo4jCsvOptions::new())
        .expect("import");
    assert_eq!(report.rows_read, 0);
    assert_eq!(report.nodes_imported, 0);
    assert!(report.zero_loss);
}

// ---- Multi-label -----------------------------------------------------------

// 9: default first-label + _labels array property + coercion note.
#[test]
fn multi_label_first_plus_labels_property() {
    let files = TempDir::new().unwrap();
    let (_tmp, db) = create_test_db().unwrap();
    let nodes = write_file(&files, "nodes.csv", ":ID,:LABEL\na,Person;Employee\n");
    let mut importer = db.import();
    let report = importer
        .neo4j_nodes_from_csv(&nodes, &Neo4jCsvOptions::new())
        .expect("import");
    let node = db.get_node(resolve(&importer, "", "a")).unwrap();
    assert_eq!(node.label.to_string(), "Person");
    assert_eq!(
        node.properties.get("_labels"),
        Some(&PropertyValue::array(vec![
            PropertyValue::string("Person"),
            PropertyValue::string("Employee"),
        ]))
    );
    assert!(
        report
            .coerced
            .iter()
            .any(|c| c.kind == "multi_label_flattened" && c.count == 1)
    );
    assert!(
        report
            .label_mapping
            .iter()
            .any(|e| e.neo4j_labels == "Person;Employee"
                && e.aletheia_label == "Person"
                && e.count == 1)
    );
}

// 10: concat strategy -> single label Person_Employee, no _labels.
#[test]
fn multi_label_concat_strategy() {
    let files = TempDir::new().unwrap();
    let (_tmp, db) = create_test_db().unwrap();
    let nodes = write_file(&files, "nodes.csv", ":ID,:LABEL\na,Person;Employee\n");
    let opts = Neo4jCsvOptions::new().label_strategy(LabelStrategy::Concat);
    let mut importer = db.import();
    importer
        .neo4j_nodes_from_csv(&nodes, &opts)
        .expect("import");
    let node = db.get_node(resolve(&importer, "", "a")).unwrap();
    assert_eq!(node.label.to_string(), "Person_Employee");
    assert_eq!(node.properties.get("_labels"), None);
}

// 11: empty :LABEL cell -> row error.
#[test]
fn empty_label_cell_row_error() {
    let files = TempDir::new().unwrap();
    let (_tmp, db) = create_test_db().unwrap();
    let nodes = write_file(&files, "nodes.csv", ":ID,:LABEL\na,\n");
    let mut importer = db.import();
    let err = importer
        .neo4j_nodes_from_csv(&nodes, &Neo4jCsvOptions::new())
        .expect_err("should error");
    let msg = err.to_string();
    assert!(msg.contains("row 1"), "was: {msg}");
    assert!(msg.contains("label"), "was: {msg}");
}

// Property strategy: single-label node still gets _labels.
#[test]
fn label_strategy_property_always_stores_labels() {
    let files = TempDir::new().unwrap();
    let (_tmp, db) = create_test_db().unwrap();
    let nodes = write_file(&files, "nodes.csv", ":ID,:LABEL\na,Person\n");
    let opts = Neo4jCsvOptions::new().label_strategy(LabelStrategy::Property);
    let mut importer = db.import();
    importer
        .neo4j_nodes_from_csv(&nodes, &opts)
        .expect("import");
    let node = db.get_node(resolve(&importer, "", "a")).unwrap();
    assert_eq!(
        node.properties.get("_labels"),
        Some(&PropertyValue::array(vec![PropertyValue::string("Person")]))
    );
}

// ---- Types / coercion ------------------------------------------------------

// 12: scalar type coercions.
#[test]
fn scalar_type_coercions() {
    let files = TempDir::new().unwrap();
    let (_tmp, db) = create_test_db().unwrap();
    let nodes = write_file(
        &files,
        "nodes.csv",
        ":ID,:LABEL,age:int,big:long,tiny:short,score:float,ratio:double,active:boolean,grade:char\n\
         a,Person,30,9000000000,5,1.5,2.25,true,A\n",
    );
    let mut importer = db.import();
    let report = importer
        .neo4j_nodes_from_csv(&nodes, &Neo4jCsvOptions::new())
        .expect("import");
    let node = db.get_node(resolve(&importer, "", "a")).unwrap();
    assert_eq!(node.properties.get("age"), Some(&PropertyValue::Int(30)));
    assert_eq!(
        node.properties.get("big"),
        Some(&PropertyValue::Int(9_000_000_000))
    );
    assert_eq!(node.properties.get("tiny"), Some(&PropertyValue::Int(5)));
    assert_eq!(
        node.properties.get("score"),
        Some(&PropertyValue::Float(1.5))
    );
    assert_eq!(
        node.properties.get("ratio"),
        Some(&PropertyValue::Float(2.25))
    );
    assert_eq!(
        node.properties.get("active"),
        Some(&PropertyValue::Bool(true))
    );
    assert_eq!(
        node.properties.get("grade"),
        Some(&PropertyValue::string("A"))
    );
    assert!(report.coerced.iter().any(|c| c.kind == "char_to_string"));
}

// 13: date/datetime -> micros Int; time/localtime -> string.
#[test]
fn temporal_type_coercions() {
    let files = TempDir::new().unwrap();
    let (_tmp, db) = create_test_db().unwrap();
    let nodes = write_file(
        &files,
        "nodes.csv",
        ":ID,:LABEL,born:date,seen:datetime,clock:time\n\
         a,Person,2020-01-01,2020-01-01T00:00:00Z,13:00:00\n",
    );
    let mut importer = db.import();
    let report = importer
        .neo4j_nodes_from_csv(&nodes, &Neo4jCsvOptions::new())
        .expect("import");
    let node = db.get_node(resolve(&importer, "", "a")).unwrap();
    let micros_2020 = 1_577_836_800_000_000i64; // 2020-01-01T00:00:00Z
    assert_eq!(
        node.properties.get("born"),
        Some(&PropertyValue::Int(micros_2020))
    );
    assert_eq!(
        node.properties.get("seen"),
        Some(&PropertyValue::Int(micros_2020))
    );
    assert_eq!(
        node.properties.get("clock"),
        Some(&PropertyValue::string("13:00:00"))
    );
    assert!(
        report
            .coerced
            .iter()
            .any(|c| c.kind == "temporal_to_micros")
    );
    assert!(report.coerced.iter().any(|c| c.kind == "time_to_string"));
}

// 14: malformed typed value -> precise row error (abort).
#[test]
fn malformed_typed_value_row_error() {
    let files = TempDir::new().unwrap();
    let (_tmp, db) = create_test_db().unwrap();
    let nodes = write_file(&files, "nodes.csv", ":ID,:LABEL,age:int\na,Person,twelve\n");
    let mut importer = db.import();
    let err = importer
        .neo4j_nodes_from_csv(&nodes, &Neo4jCsvOptions::new())
        .expect_err("should error");
    let msg = err.to_string();
    assert!(msg.contains("row 1"), "was: {msg}");
    assert!(msg.contains("Int"), "was: {msg}");
    assert!(msg.contains("age"), "was: {msg}");
}

// ---- Unsupported types -----------------------------------------------------

// 7: unsupported type headers reported + counted (values dropped, not silent).
#[test]
fn unsupported_types_reported_and_counted() {
    let files = TempDir::new().unwrap();
    let (_tmp, db) = create_test_db().unwrap();
    let nodes = write_file(
        &files,
        "nodes.csv",
        ":ID,:LABEL,span:duration,loc:point,blob:byte[]\n\
         a,Person,P1D,\"POINT(1 2)\",AQID\n",
    );
    let mut importer = db.import();
    let report = importer
        .neo4j_nodes_from_csv(&nodes, &Neo4jCsvOptions::new())
        .expect("import");
    assert_eq!(report.nodes_imported, 1);
    assert!(!report.zero_loss);
    assert!(
        report
            .unsupported
            .iter()
            .any(|u| u.neo4j_type == "duration")
    );
    assert!(report.unsupported.iter().any(|u| u.neo4j_type == "point"));
    assert!(report.unsupported.iter().any(|u| u.neo4j_type == "byte[]"));
    // Values were dropped.
    let node = db.get_node(resolve(&importer, "", "a")).unwrap();
    assert_eq!(node.properties.get("span"), None);
    assert_eq!(node.properties.get("blob"), None);
}

// 7 (strict): --strict-types upgrades unsupported header to a hard error.
#[test]
fn strict_types_hard_error() {
    let files = TempDir::new().unwrap();
    let (_tmp, db) = create_test_db().unwrap();
    let nodes = write_file(
        &files,
        "nodes.csv",
        ":ID,:LABEL,span:duration\na,Person,P1D\n",
    );
    let opts = Neo4jCsvOptions::new().strict_types(true);
    let mut importer = db.import();
    let err = importer
        .neo4j_nodes_from_csv(&nodes, &opts)
        .expect_err("should hard-error");
    let msg = err.to_string();
    assert!(msg.contains("duration"), "was: {msg}");
    assert!(msg.contains("span:duration"), "was: {msg}");
    // Nothing was written.
    assert_eq!(db.node_count(), 0);
}

// ---- Arrays / vectors ------------------------------------------------------

// 15: int[] -> Array[Int].
#[test]
fn int_array_column() {
    let files = TempDir::new().unwrap();
    let (_tmp, db) = create_test_db().unwrap();
    let nodes = write_file(
        &files,
        "nodes.csv",
        ":ID,:LABEL,scores:int[]\na,Person,1;2;3\n",
    );
    let mut importer = db.import();
    importer
        .neo4j_nodes_from_csv(&nodes, &Neo4jCsvOptions::new())
        .expect("import");
    let node = db.get_node(resolve(&importer, "", "a")).unwrap();
    assert_eq!(
        node.properties.get("scores"),
        Some(&PropertyValue::array(vec![
            PropertyValue::Int(1),
            PropertyValue::Int(2),
            PropertyValue::Int(3),
        ]))
    );
}

// 16: custom --array-delimiter honored.
#[test]
fn custom_array_delimiter() {
    let files = TempDir::new().unwrap();
    let (_tmp, db) = create_test_db().unwrap();
    let nodes = write_file(
        &files,
        "nodes.csv",
        ":ID,:LABEL,tags:string[]\na,Person,x|y|z\n",
    );
    let opts = Neo4jCsvOptions::new().array_delimiter('|');
    let mut importer = db.import();
    importer
        .neo4j_nodes_from_csv(&nodes, &opts)
        .expect("import");
    let node = db.get_node(resolve(&importer, "", "a")).unwrap();
    assert_eq!(
        node.properties.get("tags"),
        Some(&PropertyValue::array(vec![
            PropertyValue::string("x"),
            PropertyValue::string("y"),
            PropertyValue::string("z"),
        ]))
    );
}

// 17: numeric[] -> Vector only with --vector-property; Array otherwise.
#[test]
fn numeric_array_vector_only_with_flag() {
    let files = TempDir::new().unwrap();
    let (_tmp, db) = create_test_db().unwrap();
    let nodes = write_file(
        &files,
        "nodes.csv",
        ":ID,:LABEL,emb:float[]\na,Person,0.1;0.2;0.3\n",
    );

    // Without the flag -> Array of Float.
    let mut importer = db.import();
    importer
        .neo4j_nodes_from_csv(&nodes, &Neo4jCsvOptions::new())
        .expect("import");
    let node = db.get_node(resolve(&importer, "", "a")).unwrap();
    assert!(matches!(
        node.properties.get("emb"),
        Some(PropertyValue::Array(_))
    ));

    // With the flag -> Vector.
    let (_tmp2, db2) = create_test_db().unwrap();
    let opts = Neo4jCsvOptions::new().vector_property("emb");
    let mut importer2 = db2.import();
    importer2
        .neo4j_nodes_from_csv(&nodes, &opts)
        .expect("import");
    let node2 = db2
        .get_node(importer2.resolve_key(&nkey("", "a")).unwrap())
        .unwrap();
    match node2.properties.get("emb") {
        Some(PropertyValue::Vector(v)) => {
            assert_eq!(v.len(), 3);
            assert!((v[0] - 0.1).abs() < 1e-6);
        }
        other => panic!("expected Vector, got {other:?}"),
    }
}

// 18: empty array cell -> absent property.
#[test]
fn empty_array_cell_absent() {
    let files = TempDir::new().unwrap();
    let (_tmp, db) = create_test_db().unwrap();
    let nodes = write_file(&files, "nodes.csv", ":ID,:LABEL,scores:int[]\na,Person,\n");
    let mut importer = db.import();
    importer
        .neo4j_nodes_from_csv(&nodes, &Neo4jCsvOptions::new())
        .expect("import");
    let node = db.get_node(resolve(&importer, "", "a")).unwrap();
    assert_eq!(node.properties.get("scores"), None);
}

// 19: malformed array element -> row error.
#[test]
fn malformed_array_element_row_error() {
    let files = TempDir::new().unwrap();
    let (_tmp, db) = create_test_db().unwrap();
    let nodes = write_file(
        &files,
        "nodes.csv",
        ":ID,:LABEL,scores:int[]\na,Person,1;x;3\n",
    );
    let mut importer = db.import();
    let err = importer
        .neo4j_nodes_from_csv(&nodes, &Neo4jCsvOptions::new())
        .expect_err("should error");
    let msg = err.to_string();
    assert!(
        msg.contains("row 1") && msg.contains("scores"),
        "was: {msg}"
    );
}

// ---- Quoting / delimiters (RFC 4180) ---------------------------------------

// 20 + 21: quoted field containing the delimiter + doubled-quote escape.
#[test]
fn rfc4180_quoting() {
    let files = TempDir::new().unwrap();
    let (_tmp, db) = create_test_db().unwrap();
    let nodes = write_file(
        &files,
        "nodes.csv",
        ":ID,:LABEL,name\na,Person,\"Doe, John\"\nb,Person,\"She said \"\"hi\"\"\"\n",
    );
    let mut importer = db.import();
    importer
        .neo4j_nodes_from_csv(&nodes, &Neo4jCsvOptions::new())
        .expect("import");
    assert_eq!(
        db.get_node(resolve(&importer, "", "a"))
            .unwrap()
            .properties
            .get("name"),
        Some(&PropertyValue::string("Doe, John"))
    );
    assert_eq!(
        db.get_node(resolve(&importer, "", "b"))
            .unwrap()
            .properties
            .get("name"),
        Some(&PropertyValue::string("She said \"hi\""))
    );
}

// ---- Relationships / resolution --------------------------------------------

// 24: dangling :END_ID -> unresolved endpoint (skip mode reports it).
#[test]
fn dangling_endpoint_reported() {
    let files = TempDir::new().unwrap();
    let (_tmp, db) = create_test_db().unwrap();
    let nodes = write_file(&files, "nodes.csv", ":ID,:LABEL\na,Person\n");
    let rels = write_file(
        &files,
        "rels.csv",
        ":START_ID,:END_ID,:TYPE\na,ghost,KNOWS\n",
    );

    let mut importer = db.import().failure_mode(FailureMode::SkipAndReport);
    importer
        .neo4j_nodes_from_csv(&nodes, &Neo4jCsvOptions::new())
        .expect("nodes");
    let report = importer
        .neo4j_edges_from_csv(&rels, &Neo4jCsvOptions::new())
        .expect("edges skip mode");
    assert_eq!(report.relationships_imported, 0);
    assert_eq!(report.unresolved_endpoints.len(), 1);
    assert_eq!(report.unresolved_endpoints[0].side, Endpoint::Target);
    assert!(!report.zero_loss);
}

// 25: empty :TYPE -> row error.
#[test]
fn empty_type_row_error() {
    let files = TempDir::new().unwrap();
    let (_tmp, db) = create_test_db().unwrap();
    let nodes = write_file(&files, "nodes.csv", ":ID,:LABEL\na,Person\nb,Person\n");
    let rels = write_file(&files, "rels.csv", ":START_ID,:END_ID,:TYPE\na,b,\n");
    let mut importer = db.import();
    importer
        .neo4j_nodes_from_csv(&nodes, &Neo4jCsvOptions::new())
        .expect("nodes");
    let err = importer
        .neo4j_edges_from_csv(&rels, &Neo4jCsvOptions::new())
        .expect_err("should error");
    assert!(err.to_string().contains(":TYPE"), "was: {err}");
}

// 26: self-loop edge imports fine.
#[test]
fn self_loop_edge() {
    let files = TempDir::new().unwrap();
    let (_tmp, db) = create_test_db().unwrap();
    let nodes = write_file(&files, "nodes.csv", ":ID,:LABEL\na,Person\n");
    let rels = write_file(&files, "rels.csv", ":START_ID,:END_ID,:TYPE\na,a,SELF\n");
    let mut importer = db.import();
    importer
        .neo4j_nodes_from_csv(&nodes, &Neo4jCsvOptions::new())
        .expect("nodes");
    let report = importer
        .neo4j_edges_from_csv(&rels, &Neo4jCsvOptions::new())
        .expect("edges");
    assert_eq!(report.relationships_imported, 1);
}

// ---- Multi-file & structural -----------------------------------------------

// 27: two node files + one rel file resolve across both.
#[test]
fn multi_file_import() {
    let files = TempDir::new().unwrap();
    let (_tmp, db) = create_test_db().unwrap();
    let n1 = write_file(&files, "n1.csv", ":ID,:LABEL,name\na,Person,Alice\n");
    let n2 = write_file(&files, "n2.csv", ":ID,:LABEL,name\nb,Person,Bob\n");
    let rels = write_file(&files, "rels.csv", ":START_ID,:END_ID,:TYPE\na,b,KNOWS\n");
    let mut importer = db.import();
    let report = importer
        .neo4j_import_csv(&[n1, n2], &[rels], &Neo4jCsvOptions::new())
        .expect("import");
    assert_eq!(report.nodes_imported, 2);
    assert_eq!(report.relationships_imported, 1);
    assert_eq!(report.rows_read, 3);
    assert!(report.zero_loss);
    assert!(report.type_mapping.iter().any(|t| t.neo4j_type == "KNOWS"));
}

// 31: malformed CSV (unterminated quote) surfaces (not a silent clean success).
#[test]
fn malformed_csv_row_error() {
    let files = TempDir::new().unwrap();
    let (_tmp, db) = create_test_db().unwrap();
    let nodes = write_file(
        &files,
        "nodes.csv",
        ":ID,:LABEL,name\na,Person,\"unterminated\nb,Person,ok\n",
    );
    let mut importer = db.import().failure_mode(FailureMode::SkipAndReport);
    let report = importer.neo4j_nodes_from_csv(&nodes, &Neo4jCsvOptions::new());
    if let Ok(r) = report {
        assert!(
            !r.zero_loss || r.nodes_imported < 2,
            "unexpected clean import: {r:?}"
        );
    }
}

// ---- Fidelity report -------------------------------------------------------

// 32/33: zero_loss true on clean import, false with unsupported column.
#[test]
fn zero_loss_flag() {
    let files = TempDir::new().unwrap();
    let (_tmp, db) = create_test_db().unwrap();
    let clean = write_file(&files, "clean.csv", ":ID,:LABEL,age:int\na,Person,1\n");
    let mut importer = db.import();
    let r1 = importer
        .neo4j_nodes_from_csv(&clean, &Neo4jCsvOptions::new())
        .expect("import");
    assert!(r1.zero_loss);
    assert_eq!(r1.properties_imported, 1);

    let dirty = write_file(
        &files,
        "dirty.csv",
        ":ID,:LABEL,span:duration\nb,Person,P1D\n",
    );
    let (_tmp2, db2) = create_test_db().unwrap();
    let mut importer2 = db2.import();
    let r2 = importer2
        .neo4j_nodes_from_csv(&dirty, &Neo4jCsvOptions::new())
        .expect("import");
    assert!(!r2.zero_loss);
}

// Report serializes to JSON (superset of ImportReport) with zero_loss field.
#[test]
fn report_serializes_to_json() {
    let files = TempDir::new().unwrap();
    let (_tmp, db) = create_test_db().unwrap();
    let nodes = write_file(&files, "nodes.csv", ":ID,:LABEL,name\na,Person,Alice\n");
    let mut importer = db.import();
    let report = importer
        .neo4j_nodes_from_csv(&nodes, &Neo4jCsvOptions::new())
        .expect("import");
    let json = serde_json::to_value(&report).expect("serialize");
    assert_eq!(json["zero_loss"], serde_json::Value::Bool(true));
    assert_eq!(json["nodes_imported"], serde_json::json!(1));
    assert!(json["label_mapping"].is_array());
}

// ---- Provenance (AC5) ------------------------------------------------------

// 34: every imported node/edge carries neo4j-import::<file> provenance.
#[test]
fn provenance_stamped() {
    let files = TempDir::new().unwrap();
    let (_tmp, db) = create_test_db().unwrap();
    let nodes = write_file(&files, "nodes.csv", ":ID,:LABEL\na,Person\nb,Person\n");
    let rels = write_file(&files, "rels.csv", ":START_ID,:END_ID,:TYPE\na,b,KNOWS\n");
    let mut importer = db.import();
    importer
        .neo4j_nodes_from_csv(&nodes, &Neo4jCsvOptions::new())
        .expect("nodes");
    importer
        .neo4j_edges_from_csv(&rels, &Neo4jCsvOptions::new())
        .expect("edges");

    let a = resolve(&importer, "", "a");
    let prov = db.get_node_provenance(a).unwrap().expect("node provenance");
    assert_eq!(prov.source(), Some("neo4j-import::nodes.csv"));

    let edge = db.get_outgoing_edges(a)[0];
    let eprov = db
        .get_edge_provenance(edge)
        .unwrap()
        .expect("edge provenance");
    assert_eq!(eprov.source(), Some("neo4j-import::rels.csv"));
}

// Existing importer behavior unchanged when provenance is None (regression).
#[test]
fn generic_import_has_no_provenance() {
    let files = TempDir::new().unwrap();
    let (_tmp, db) = create_test_db().unwrap();
    let nodes = write_file(&files, "nodes.csv", "id,name\na,Alice\n");
    let mapping = NodeMapping::new(LabelSource::fixed("Person"), "id").property(
        "name",
        "name",
        ColumnType::String,
    );
    let mut importer = db.import();
    importer.nodes_from_csv(&nodes, mapping).expect("import");
    let a = importer.resolve_key("a").unwrap();
    assert!(db.get_node_provenance(a).unwrap().is_none());
}

// ---- Valid-time (AC6) ------------------------------------------------------

// 35: --valid-from-property backdates the fact.
#[test]
fn valid_from_property_backdates() {
    let files = TempDir::new().unwrap();
    let (_tmp, db) = create_test_db().unwrap();
    let nodes = write_file(
        &files,
        "nodes.csv",
        ":ID,:LABEL,since:datetime\na,Person,2020-01-01T00:00:00Z\n",
    );
    let opts = Neo4jCsvOptions::new().valid_from_property("since");
    let mut importer = db.import();
    importer
        .neo4j_nodes_from_csv(&nodes, &opts)
        .expect("import");
    let a = resolve(&importer, "", "a");

    // Not stored as a regular property (consumed as valid time).
    let node = db.get_node(a).unwrap();
    assert_eq!(node.properties.get("since"), None);

    // Visible at 2021 (after valid_from), not at 2019 (before it).
    let ts_2021 = Timestamp::from(1_609_459_200_000_000i64);
    let ts_2019 = Timestamp::from(1_546_300_800_000_000i64);
    assert!(db.get_node_at_valid_time(a, ts_2021).is_ok());
    assert!(db.get_node_at_valid_time(a, ts_2019).is_err());
}

// ---- Conformance fixes (review) --------------------------------------------

// #1: boolean == Boolean.parseBoolean — `true` iff case-insensitively "true".
#[test]
fn boolean_parse_matches_neo4j() {
    let files = TempDir::new().unwrap();
    let (_tmp, db) = create_test_db().unwrap();
    let nodes = write_file(
        &files,
        "nodes.csv",
        ":ID,:LABEL,b:boolean\n\
         one,Person,1\n\
         yes,Person,yes\n\
         banana,Person,banana\n\
         upper,Person,TRUE\n\
         no,Person,false\n",
    );
    let mut importer = db.import();
    importer
        .neo4j_nodes_from_csv(&nodes, &Neo4jCsvOptions::new())
        .expect("import");
    let get = |id: &str| {
        db.get_node(resolve(&importer, "", id))
            .unwrap()
            .properties
            .get("b")
            .cloned()
    };
    assert_eq!(get("one"), Some(PropertyValue::Bool(false))); // "1" is not "true"
    assert_eq!(get("yes"), Some(PropertyValue::Bool(false))); // "yes" is not "true"
    assert_eq!(get("banana"), Some(PropertyValue::Bool(false)));
    assert_eq!(get("upper"), Some(PropertyValue::Bool(true))); // case-insensitive
    assert_eq!(get("no"), Some(PropertyValue::Bool(false)));
}

// #2: multi-character `char` is a clean row error; single char is fine.
#[test]
fn char_requires_exactly_one_character() {
    let files = TempDir::new().unwrap();
    let (_tmp, db) = create_test_db().unwrap();
    let bad = write_file(&files, "bad.csv", ":ID,:LABEL,c:char\na,Person,AB\n");
    let mut importer = db.import();
    let err = importer
        .neo4j_nodes_from_csv(&bad, &Neo4jCsvOptions::new())
        .expect_err("multi-char char should error");
    let msg = err.to_string();
    assert!(msg.contains("row 1"), "was: {msg}");
    assert!(
        msg.contains("char") && msg.contains("exactly 1"),
        "was: {msg}"
    );

    let ok = write_file(&files, "ok.csv", ":ID,:LABEL,c:char\na,Person,A\n");
    let (_tmp2, db2) = create_test_db().unwrap();
    let mut importer2 = db2.import();
    importer2
        .neo4j_nodes_from_csv(&ok, &Neo4jCsvOptions::new())
        .expect("single char imports");
    let node = db2
        .get_node(importer2.resolve_key(&nkey("", "a")).unwrap())
        .unwrap();
    assert_eq!(node.properties.get("c"), Some(&PropertyValue::string("A")));
}

// #3: out-of-range `short` (and `byte`) rejected; in-range become Int.
#[test]
fn short_out_of_range_rejected() {
    let files = TempDir::new().unwrap();
    let (_tmp, db) = create_test_db().unwrap();
    let nodes = write_file(
        &files,
        "nodes.csv",
        ":ID,:LABEL,tiny:short\na,Person,99999\n",
    );
    let mut importer = db.import();
    let err = importer
        .neo4j_nodes_from_csv(&nodes, &Neo4jCsvOptions::new())
        .expect_err("out-of-range short should error");
    let msg = err.to_string();
    assert!(msg.contains("row 1") && msg.contains("short"), "was: {msg}");
}

#[test]
fn byte_out_of_range_rejected() {
    let files = TempDir::new().unwrap();
    let (_tmp, db) = create_test_db().unwrap();
    let nodes = write_file(&files, "nodes.csv", ":ID,:LABEL,b:byte\na,Person,200\n");
    let mut importer = db.import();
    let err = importer
        .neo4j_nodes_from_csv(&nodes, &Neo4jCsvOptions::new())
        .expect_err("out-of-range byte should error");
    assert!(err.to_string().contains("byte"), "was: {err}");
}

// #4: quoted array element containing the array delimiter is not mis-split.
#[test]
fn quoted_array_element_with_delimiter() {
    let files = TempDir::new().unwrap();
    let (_tmp, db) = create_test_db().unwrap();
    // CSV cell after unquoting is: "a;b";c  ->  ["a;b", "c"]
    let nodes = write_file(
        &files,
        "nodes.csv",
        ":ID,:LABEL,tags:string[]\na,Person,\"\"\"a;b\"\";c\"\n",
    );
    let mut importer = db.import();
    importer
        .neo4j_nodes_from_csv(&nodes, &Neo4jCsvOptions::new())
        .expect("import");
    let node = db.get_node(resolve(&importer, "", "a")).unwrap();
    assert_eq!(
        node.properties.get("tags"),
        Some(&PropertyValue::array(vec![
            PropertyValue::string("a;b"),
            PropertyValue::string("c"),
        ]))
    );
}

// #5: datetime with a trailing bracketed IANA zone id parses.
#[test]
fn datetime_named_zone_id_stripped() {
    let files = TempDir::new().unwrap();
    let (_tmp, db) = create_test_db().unwrap();
    let nodes = write_file(
        &files,
        "nodes.csv",
        ":ID,:LABEL,born:datetime\n\
         zoned,Person,2015-06-24T12:50:35.556+01:00[Europe/London]\n\
         plain,Person,2015-06-24T12:50:35.556+01:00\n",
    );
    let mut importer = db.import();
    importer
        .neo4j_nodes_from_csv(&nodes, &Neo4jCsvOptions::new())
        .expect("import");
    let zoned = db.get_node(resolve(&importer, "", "zoned")).unwrap();
    let plain = db.get_node(resolve(&importer, "", "plain")).unwrap();
    let z = zoned.properties.get("born").cloned();
    assert!(matches!(z, Some(PropertyValue::Int(_))), "was: {z:?}");
    // The bracketed zone id names the same instant as the bare offset.
    assert_eq!(z, plain.properties.get("born").cloned());
}

// #6: non-finite float (overflow) is rejected, not stored as inf.
#[test]
fn non_finite_float_rejected() {
    let files = TempDir::new().unwrap();
    let (_tmp, db) = create_test_db().unwrap();
    let nodes = write_file(&files, "nodes.csv", ":ID,:LABEL,x:double\na,Person,1e400\n");
    let mut importer = db.import();
    let err = importer
        .neo4j_nodes_from_csv(&nodes, &Neo4jCsvOptions::new())
        .expect_err("non-finite float should error");
    let msg = err.to_string();
    assert!(
        msg.contains("row 1") && msg.contains("non-finite"),
        "was: {msg}"
    );
}

// #7: multi-file skipped/unresolved errors name the offending file.
#[test]
fn multi_file_errors_name_the_file() {
    let files = TempDir::new().unwrap();
    let (_tmp, db) = create_test_db().unwrap();
    let n1 = write_file(&files, "good.csv", ":ID,:LABEL\na,Person\n");
    // Second node file has a malformed row (empty :ID).
    let n2 = write_file(&files, "broken.csv", ":ID,:LABEL\n,Person\n");
    let rels = write_file(
        &files,
        "links.csv",
        ":START_ID,:END_ID,:TYPE\na,ghost,KNOWS\n",
    );

    let mut importer = db.import().failure_mode(FailureMode::SkipAndReport);
    let report = importer
        .neo4j_import_csv(&[n1, n2], &[rels], &Neo4jCsvOptions::new())
        .expect("import");

    let skipped = report
        .skipped
        .iter()
        .find(|e| e.file.as_deref() == Some("broken.csv"))
        .expect("skipped row attributed to broken.csv");
    assert!(skipped.to_string().contains("broken.csv"), "was: {skipped}");

    let unresolved = &report.unresolved_endpoints[0];
    assert_eq!(unresolved.file.as_deref(), Some("links.csv"));
    assert!(
        unresolved.to_string().contains("links.csv"),
        "was: {unresolved}"
    );
}

// #9: duplicate header columns are rejected (would silently collapse).
#[test]
fn duplicate_header_rejected() {
    let files = TempDir::new().unwrap();
    let (_tmp, db) = create_test_db().unwrap();
    let nodes = write_file(&files, "nodes.csv", ":ID,:ID,:LABEL\na,b,Person\n");
    let mut importer = db.import();
    let err = importer
        .neo4j_nodes_from_csv(&nodes, &Neo4jCsvOptions::new())
        .expect_err("duplicate header should error");
    let msg = err.to_string();
    assert!(
        msg.contains("duplicate header") && msg.contains(":ID"),
        "was: {msg}"
    );
}

// #10: skip mode — mapping counts reflect only imported rows, not skipped ones.
#[test]
fn skip_mode_type_mapping_matches_imported() {
    let files = TempDir::new().unwrap();
    let (_tmp, db) = create_test_db().unwrap();
    let nodes = write_file(&files, "nodes.csv", ":ID,:LABEL\na,Person\nb,Person\n");
    // One resolvable edge + one dangling edge, same :TYPE.
    let rels = write_file(
        &files,
        "rels.csv",
        ":START_ID,:END_ID,:TYPE\na,b,KNOWS\na,ghost,KNOWS\n",
    );
    let mut importer = db.import().failure_mode(FailureMode::SkipAndReport);
    importer
        .neo4j_nodes_from_csv(&nodes, &Neo4jCsvOptions::new())
        .expect("nodes");
    let report = importer
        .neo4j_edges_from_csv(&rels, &Neo4jCsvOptions::new())
        .expect("edges");
    assert_eq!(report.relationships_imported, 1);
    let knows = report
        .type_mapping
        .iter()
        .find(|t| t.neo4j_type == "KNOWS")
        .expect("KNOWS type mapping");
    // The dangling row must NOT be counted in the mapping.
    assert_eq!(knows.count, report.relationships_imported);
    assert_eq!(knows.count, 1);
}

// #11: an array column's coercion counts once per row, not per element.
#[test]
fn array_coercion_counts_per_row() {
    let files = TempDir::new().unwrap();
    let (_tmp, db) = create_test_db().unwrap();
    let nodes = write_file(
        &files,
        "nodes.csv",
        ":ID,:LABEL,grades:char[]\na,Person,A;B;C\n",
    );
    let mut importer = db.import();
    let report = importer
        .neo4j_nodes_from_csv(&nodes, &Neo4jCsvOptions::new())
        .expect("import");
    let note = report
        .coerced
        .iter()
        .find(|c| c.column == "grades:char[]" && c.kind == "char_to_string")
        .expect("char coercion recorded");
    // One row, three elements -> counted once (not three times).
    assert_eq!(note.count, 1);
}

// #12: unknown reserved header + malformed id-space report precise errors.
#[test]
fn unknown_reserved_header_error() {
    let files = TempDir::new().unwrap();
    let (_tmp, db) = create_test_db().unwrap();
    let nodes = write_file(&files, "nodes.csv", ":ID,:LABEL,:FOO\na,Person,x\n");
    let mut importer = db.import();
    let err = importer
        .neo4j_nodes_from_csv(&nodes, &Neo4jCsvOptions::new())
        .expect_err("unknown reserved header should error");
    assert!(
        err.to_string().contains("unknown reserved header"),
        "was: {err}"
    );
}

#[test]
fn malformed_id_space_error() {
    let files = TempDir::new().unwrap();
    let (_tmp, db) = create_test_db().unwrap();
    let nodes = write_file(&files, "nodes.csv", ":ID(,:LABEL\na,Person\n");
    let mut importer = db.import();
    let err = importer
        .neo4j_nodes_from_csv(&nodes, &Neo4jCsvOptions::new())
        .expect_err("malformed id-space should error");
    let msg = err.to_string();
    assert!(
        msg.contains("malformed id-space") && msg.contains("unclosed"),
        "was: {msg}"
    );
}

// ---- Scale (AC7) -----------------------------------------------------------

// 10K-node / ~30K-edge smoke: zero-loss report.
#[test]
fn scale_smoke_10k_nodes() {
    let files = TempDir::new().unwrap();
    let (_tmp, db) = create_test_db().unwrap();

    let mut nbuf = String::from(":ID,:LABEL,name,age:int\n");
    let n = 10_000usize;
    for i in 0..n {
        nbuf.push_str(&format!("n{i},Person,Name{i},{}\n", i % 100));
    }
    let nodes = write_file(&files, "nodes.csv", &nbuf);

    let mut ebuf = String::from(":START_ID,:END_ID,:TYPE\n");
    for i in 0..n {
        // ~3 edges per node -> ~30K edges, all resolvable.
        ebuf.push_str(&format!("n{i},n{},KNOWS\n", (i + 1) % n));
        ebuf.push_str(&format!("n{i},n{},KNOWS\n", (i + 2) % n));
        ebuf.push_str(&format!("n{i},n{},KNOWS\n", (i + 3) % n));
    }
    let rels = write_file(&files, "rels.csv", &ebuf);

    let mut importer = db.import();
    let report = importer
        .neo4j_import_csv(&[nodes], &[rels], &Neo4jCsvOptions::new())
        .expect("import");
    assert_eq!(report.nodes_imported, n);
    assert_eq!(report.relationships_imported, n * 3);
    assert!(report.zero_loss);
    assert_eq!(db.node_count(), n);
}
