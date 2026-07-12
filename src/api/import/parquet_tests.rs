//! Tests for the Parquet import path (Issue #3364).
//!
//! These re-assert the #3211 / PR #3495 invariants for Parquet input (abort vs
//! skip-and-report, precise `row N:` numbering, unresolved-endpoint side, valid-time
//! backfill, chunk boundaries, null-as-absent) and add Parquet-native coverage:
//! int/float/bool/string round-trip, `list<float32>` embedding round-trip with exact
//! bits, native `timestamp` valid-time at microsecond precision, multi-row-group
//! files, malformed/non-parquet input, and empty files.

use std::path::PathBuf;
use std::sync::Arc;

use arrow::array::{
    ArrayRef, BooleanArray, Float64Array, Int64Array, ListArray, RecordBatch, StringArray,
    TimestampMicrosecondArray,
};
use arrow::datatypes::Float32Type;
// `::parquet` (the crate) is disambiguated from the sibling `super::parquet` module.
use ::parquet::arrow::ArrowWriter;
use ::parquet::file::properties::WriterProperties;
use tempfile::TempDir;

use super::*;
use crate::core::property::PropertyValue;
use crate::core::temporal::time;
use crate::test_utils::create_test_db;

/// Write a single `RecordBatch` to a Parquet file, optionally forcing a small
/// max row-group size (to exercise multi-row-group reads).
fn write_parquet(
    dir: &TempDir,
    name: &str,
    batch: &RecordBatch,
    max_row_group_size: Option<usize>,
) -> PathBuf {
    let path = dir.path().join(name);
    let file = std::fs::File::create(&path).expect("create parquet file");
    let props = max_row_group_size.map(|n| {
        WriterProperties::builder()
            .set_max_row_group_row_count(Some(n))
            .build()
    });
    let mut writer = ArrowWriter::try_new(file, batch.schema(), props).expect("arrow writer");
    writer.write(batch).expect("write batch");
    writer.close().expect("close writer");
    path
}

fn str_col(values: Vec<Option<&str>>) -> ArrayRef {
    Arc::new(StringArray::from(values))
}

fn person_nodes(key_col: &str) -> NodeMapping {
    NodeMapping::new(LabelSource::fixed("Person"), key_col)
        .property("name", "name", ColumnType::String)
        .property("age", "age", ColumnType::Int)
}

// --- Happy path: nodes + edges, native typed coercion --------------------------

#[test]
fn happy_path_nodes_and_edges_parquet() {
    let files = TempDir::new().unwrap();
    let (_tmp, db) = create_test_db().unwrap();

    let node_batch = RecordBatch::try_from_iter(vec![
        ("id", str_col(vec![Some("alice"), Some("bob")])),
        ("name", str_col(vec![Some("Alice"), Some("Bob")])),
        ("age", Arc::new(Int64Array::from(vec![30, 25])) as ArrayRef),
    ])
    .unwrap();
    let nodes = write_parquet(&files, "nodes.parquet", &node_batch, None);

    let edge_batch = RecordBatch::try_from_iter(vec![
        ("src", str_col(vec![Some("alice")])),
        ("dst", str_col(vec![Some("bob")])),
        ("since", Arc::new(Int64Array::from(vec![2020])) as ArrayRef),
    ])
    .unwrap();
    let edges = write_parquet(&files, "edges.parquet", &edge_batch, None);

    let mut importer = db.import();
    let node_report = importer
        .nodes_from_parquet(&nodes, person_nodes("id"))
        .expect("nodes import");
    assert_eq!(node_report.rows_read, 2);
    assert_eq!(node_report.nodes_imported, 2);
    assert!(node_report.skipped.is_empty());

    let edge_mapping = EdgeMapping::new(LabelSource::fixed("KNOWS"), "src", "dst").property(
        "since",
        "since",
        ColumnType::Int,
    );
    let edge_report = importer
        .edges_from_parquet(&edges, edge_mapping)
        .expect("edges import");
    assert_eq!(edge_report.edges_imported, 1);
    assert!(edge_report.unresolved_endpoints.is_empty());

    assert_eq!(db.node_count(), 2);
    assert_eq!(db.edge_count(), 1);

    let alice = importer.resolve_key("alice").expect("alice resolved");
    let node = db.get_node(alice).unwrap();
    assert_eq!(
        node.properties.get("name"),
        Some(&PropertyValue::string("Alice"))
    );
    assert_eq!(node.properties.get("age"), Some(&PropertyValue::Int(30)));
}

// --- Native scalar round-trip: int / float / bool / string ---------------------

#[test]
fn scalar_types_round_trip_natively() {
    let files = TempDir::new().unwrap();
    let (_tmp, db) = create_test_db().unwrap();

    let batch = RecordBatch::try_from_iter(vec![
        ("id", str_col(vec![Some("n1")])),
        ("name", str_col(vec![Some("Widget")])),
        ("age", Arc::new(Int64Array::from(vec![42])) as ArrayRef),
        ("score", Arc::new(Float64Array::from(vec![3.5])) as ArrayRef),
        (
            "active",
            Arc::new(BooleanArray::from(vec![true])) as ArrayRef,
        ),
    ])
    .unwrap();
    let path = write_parquet(&files, "scalars.parquet", &batch, None);

    let mapping = NodeMapping::new(LabelSource::fixed("Thing"), "id")
        .property("name", "name", ColumnType::String)
        .property("age", "age", ColumnType::Int)
        .property("score", "score", ColumnType::Float)
        .property("active", "active", ColumnType::Bool);

    let mut importer = db.import();
    importer.nodes_from_parquet(&path, mapping).unwrap();
    let id = importer.resolve_key("n1").unwrap();
    let node = db.get_node(id).unwrap();

    assert_eq!(
        node.properties.get("name"),
        Some(&PropertyValue::string("Widget"))
    );
    assert_eq!(node.properties.get("age"), Some(&PropertyValue::Int(42)));
    assert_eq!(
        node.properties.get("score"),
        Some(&PropertyValue::Float(3.5))
    );
    assert_eq!(
        node.properties.get("active"),
        Some(&PropertyValue::Bool(true))
    );
}

// --- Embedding round-trip with exact f32 bits ----------------------------------

#[test]
fn embedding_list_float32_round_trips_exact_bits() {
    let files = TempDir::new().unwrap();
    let (_tmp, db) = create_test_db().unwrap();

    let expected: Vec<f32> = vec![1.5, -2.25, 3.0, 0.1];
    let list = ListArray::from_iter_primitive::<Float32Type, _, _>(vec![Some(
        expected.iter().copied().map(Some).collect::<Vec<_>>(),
    )]);

    let batch = RecordBatch::try_from_iter(vec![
        ("id", str_col(vec![Some("doc1")])),
        ("embedding", Arc::new(list) as ArrayRef),
    ])
    .unwrap();
    let path = write_parquet(&files, "emb.parquet", &batch, None);

    let mapping = NodeMapping::new(LabelSource::fixed("Document"), "id").property(
        "embedding",
        "embedding",
        ColumnType::Embedding,
    );
    let mut importer = db.import();
    importer.nodes_from_parquet(&path, mapping).unwrap();
    let id = importer.resolve_key("doc1").unwrap();
    let node = db.get_node(id).unwrap();

    let stored = node
        .properties
        .get("embedding")
        .and_then(|v| v.as_vector())
        .expect("embedding stored as vector");
    // Exact bit equality — no lossy string round-trip.
    assert_eq!(stored, expected.as_slice());
    assert_eq!(
        stored.iter().map(|f| f.to_bits()).collect::<Vec<_>>(),
        expected.iter().map(|f| f.to_bits()).collect::<Vec<_>>()
    );
}

// --- Native timestamp valid-time backfill at microsecond precision -------------

#[test]
fn native_timestamp_valid_time_round_trips_with_as_of() {
    let files = TempDir::new().unwrap();
    let (_tmp, db) = create_test_db().unwrap();

    // 2021-01-01T00:00:00Z in microseconds since the epoch.
    let micros_2021 = 1_609_459_200_i64 * 1_000_000;
    let ts = TimestampMicrosecondArray::from(vec![micros_2021]);
    let batch = RecordBatch::try_from_iter(vec![
        ("id", str_col(vec![Some("alice")])),
        ("name", str_col(vec![Some("Alice")])),
        ("age", Arc::new(Int64Array::from(vec![30])) as ArrayRef),
        ("known_since", Arc::new(ts) as ArrayRef),
    ])
    .unwrap();
    let path = write_parquet(&files, "ts.parquet", &batch, None);

    let mapping = person_nodes("id").valid_time_column("known_since");
    let mut importer = db.import();
    importer.nodes_from_parquet(&path, mapping).unwrap();
    let alice = importer.resolve_key("alice").unwrap();

    let now = time::now();
    let after = time::from_secs(1_622_505_600); // 2021-06-01
    assert!(
        db.get_node_at_time(alice, after, now).is_ok(),
        "node should be valid after its valid_from"
    );
    let before = time::from_secs(1_590_969_600); // 2020-06-01
    assert!(
        db.get_node_at_time(alice, before, now).is_err(),
        "node should not be valid before its valid_from"
    );
}

// --- A null cell becomes an ABSENT property, not a stored null -----------------

#[test]
fn null_cell_becomes_absent_property_not_null() {
    let files = TempDir::new().unwrap();
    let (_tmp, db) = create_test_db().unwrap();

    let batch = RecordBatch::try_from_iter(vec![
        ("id", str_col(vec![Some("n1")])),
        ("name", str_col(vec![Some("HasName")])),
        // age is null for this row.
        (
            "age",
            Arc::new(Int64Array::from(vec![None::<i64>])) as ArrayRef,
        ),
    ])
    .unwrap();
    let path = write_parquet(&files, "nullage.parquet", &batch, None);

    let mut importer = db.import();
    importer
        .nodes_from_parquet(&path, person_nodes("id"))
        .unwrap();
    let id = importer.resolve_key("n1").unwrap();
    let node = db.get_node(id).unwrap();

    assert_eq!(
        node.properties.get("name"),
        Some(&PropertyValue::string("HasName"))
    );
    assert_eq!(node.properties.get("age"), None, "null age must be absent");
}

// --- Malformed / non-parquet input is a clean error, no partial writes ---------

#[test]
fn non_parquet_file_is_clean_error_no_writes() {
    let files = TempDir::new().unwrap();
    let (_tmp, db) = create_test_db().unwrap();

    let path = files.path().join("garbage.parquet");
    std::fs::write(&path, b"this is definitely not a parquet file").unwrap();

    // Fatal even in skip mode (open/metadata failure is always fatal).
    let mut importer = db.import().failure_mode(FailureMode::SkipAndReport);
    let result = importer.nodes_from_parquet(&path, person_nodes("id"));
    assert!(result.is_err(), "non-parquet file must error");
    assert_eq!(db.node_count(), 0, "no partial writes on open failure");
}

// --- Missing required label column errors --------------------------------------

#[test]
fn missing_label_column_errors_with_row_number() {
    let files = TempDir::new().unwrap();
    let (_tmp, db) = create_test_db().unwrap();

    // No "kind" column present in the file.
    let batch = RecordBatch::try_from_iter(vec![
        ("id", str_col(vec![Some("n1")])),
        ("name", str_col(vec![Some("A")])),
    ])
    .unwrap();
    let path = write_parquet(&files, "nolabel.parquet", &batch, None);

    let mapping = NodeMapping::new(LabelSource::column("kind"), "id");
    let mut importer = db.import();
    let err = importer
        .nodes_from_parquet(&path, mapping)
        .expect_err("missing label column must error");
    assert!(err.to_string().contains("row 1"), "got: {err}");
    assert!(err.to_string().contains("kind"), "got: {err}");
    assert_eq!(db.node_count(), 0);
}

// --- Wrong column type vs mapping: abort reports row, skip collects it ----------

#[test]
fn wrong_column_type_abort_reports_row_number() {
    let files = TempDir::new().unwrap();
    let (_tmp, db) = create_test_db().unwrap();

    // age column is a String that is not an integer; mapping asks for Int.
    let batch = RecordBatch::try_from_iter(vec![
        ("id", str_col(vec![Some("alice"), Some("bob")])),
        ("name", str_col(vec![Some("Alice"), Some("Bob")])),
        ("age", str_col(vec![Some("30"), Some("notanumber")])),
    ])
    .unwrap();
    let path = write_parquet(&files, "badage.parquet", &batch, None);

    // batch_size(1) so row 1 commits before the abort at row 2 (mirrors the CSV test).
    let mut importer = db.import().batch_size(1);
    let err = importer
        .nodes_from_parquet(&path, person_nodes("id"))
        .expect_err("bad Int coercion must abort");
    let msg = err.to_string();
    assert!(msg.contains("row 2"), "got: {msg}");
    assert!(msg.contains("Int"), "got: {msg}");
    // Row 1 committed before the abort at row 2.
    assert_eq!(db.node_count(), 1);
}

#[test]
fn wrong_column_type_skip_and_report() {
    let files = TempDir::new().unwrap();
    let (_tmp, db) = create_test_db().unwrap();

    let batch = RecordBatch::try_from_iter(vec![
        ("id", str_col(vec![Some("alice"), Some("bob")])),
        ("name", str_col(vec![Some("Alice"), Some("Bob")])),
        ("age", str_col(vec![Some("30"), Some("notanumber")])),
    ])
    .unwrap();
    let path = write_parquet(&files, "badage2.parquet", &batch, None);

    let mut importer = db.import().failure_mode(FailureMode::SkipAndReport);
    let report = importer
        .nodes_from_parquet(&path, person_nodes("id"))
        .unwrap();
    assert_eq!(report.rows_read, 2);
    assert_eq!(report.nodes_imported, 1);
    assert_eq!(report.skipped.len(), 1);
    assert_eq!(report.skipped[0].row, 2);
    assert_eq!(db.node_count(), 1);
}

// --- Unresolved edge endpoint reports the correct side -------------------------

#[test]
fn unresolved_target_endpoint_reported() {
    let files = TempDir::new().unwrap();
    let (_tmp, db) = create_test_db().unwrap();

    let node_batch = RecordBatch::try_from_iter(vec![
        ("id", str_col(vec![Some("alice")])),
        ("name", str_col(vec![Some("Alice")])),
        ("age", Arc::new(Int64Array::from(vec![30])) as ArrayRef),
    ])
    .unwrap();
    let nodes = write_parquet(&files, "n.parquet", &node_batch, None);

    // Edge references "bob" as target, which was never imported.
    let edge_batch = RecordBatch::try_from_iter(vec![
        ("src", str_col(vec![Some("alice")])),
        ("dst", str_col(vec![Some("bob")])),
    ])
    .unwrap();
    let edges = write_parquet(&files, "e.parquet", &edge_batch, None);

    let mut importer = db.import().failure_mode(FailureMode::SkipAndReport);
    importer
        .nodes_from_parquet(&nodes, person_nodes("id"))
        .unwrap();
    let report = importer
        .edges_from_parquet(
            &edges,
            EdgeMapping::new(LabelSource::fixed("KNOWS"), "src", "dst"),
        )
        .unwrap();
    assert_eq!(report.edges_imported, 0);
    assert_eq!(report.unresolved_endpoints.len(), 1);
    assert_eq!(report.unresolved_endpoints[0].side, Endpoint::Target);
    assert_eq!(report.unresolved_endpoints[0].key, "bob");
    assert_eq!(report.unresolved_endpoints[0].row, 1);
}

// --- Multi-row-group file: all rows imported -----------------------------------

#[test]
fn multi_row_group_imports_all_rows() {
    let files = TempDir::new().unwrap();
    let (_tmp, db) = create_test_db().unwrap();

    let ids: Vec<String> = (0..25).map(|i| format!("n{i}")).collect();
    let id_col = str_col(ids.iter().map(|s| Some(s.as_str())).collect());
    let name_col = str_col((0..25).map(|_| Some("X")).collect());
    let age_col = Arc::new(Int64Array::from((0..25).collect::<Vec<i64>>())) as ArrayRef;
    let batch =
        RecordBatch::try_from_iter(vec![("id", id_col), ("name", name_col), ("age", age_col)])
            .unwrap();
    // Force a row-group boundary every 10 rows => 3 row groups.
    let path = write_parquet(&files, "multi.parquet", &batch, Some(10));

    let mut importer = db.import().batch_size(10);
    let report = importer
        .nodes_from_parquet(&path, person_nodes("id"))
        .unwrap();
    assert_eq!(report.rows_read, 25);
    assert_eq!(report.nodes_imported, 25);
    assert_eq!(db.node_count(), 25);
}

// --- Reader spans multiple internal read batches (> PARQUET_BATCH_SIZE) ---------

#[test]
fn reader_spans_multiple_read_batches() {
    let files = TempDir::new().unwrap();
    let (_tmp, db) = create_test_db().unwrap();

    // 9000 rows > the 8192-row internal batch size, exercising batch advancement.
    let n = 9000;
    let ids: Vec<String> = (0..n).map(|i| format!("n{i}")).collect();
    let id_col = str_col(ids.iter().map(|s| Some(s.as_str())).collect());
    let name_col = str_col((0..n).map(|_| Some("X")).collect());
    let batch = RecordBatch::try_from_iter(vec![("id", id_col), ("name", name_col)]).unwrap();
    let path = write_parquet(&files, "big.parquet", &batch, None);

    let mapping = NodeMapping::new(LabelSource::fixed("Person"), "id").property(
        "name",
        "name",
        ColumnType::String,
    );
    let mut importer = db.import();
    let report = importer.nodes_from_parquet(&path, mapping).unwrap();
    assert_eq!(report.rows_read, n);
    assert_eq!(report.nodes_imported, n);
    assert_eq!(db.node_count(), n);
}

// --- Empty file (schema, zero rows) imports nothing, no error ------------------

#[test]
fn empty_file_imports_nothing_without_error() {
    let files = TempDir::new().unwrap();
    let (_tmp, db) = create_test_db().unwrap();

    let batch =
        RecordBatch::try_from_iter(vec![("id", str_col(vec![])), ("name", str_col(vec![]))])
            .unwrap();
    let path = write_parquet(&files, "empty.parquet", &batch, None);

    let mut importer = db.import();
    let report = importer
        .nodes_from_parquet(&path, person_nodes("id"))
        .unwrap();
    assert_eq!(report.rows_read, 0);
    assert_eq!(report.nodes_imported, 0);
    assert_eq!(db.node_count(), 0);
}

// --- Empty key column errors with a row number ---------------------------------

#[test]
fn empty_key_column_errors_with_row_number() {
    let files = TempDir::new().unwrap();
    let (_tmp, db) = create_test_db().unwrap();

    // id is null on the (only) row => empty business key.
    let batch = RecordBatch::try_from_iter(vec![
        ("id", str_col(vec![None])),
        ("name", str_col(vec![Some("A")])),
        ("age", Arc::new(Int64Array::from(vec![1])) as ArrayRef),
    ])
    .unwrap();
    let path = write_parquet(&files, "nokey.parquet", &batch, None);

    let mut importer = db.import();
    let err = importer
        .nodes_from_parquet(&path, person_nodes("id"))
        .expect_err("empty key must error");
    assert!(err.to_string().contains("row 1"), "got: {err}");
    assert_eq!(db.node_count(), 0);
}
