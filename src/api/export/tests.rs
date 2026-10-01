//! Tests for the Parquet export path (Issue #3364).
//!
//! Coverage: current-state round-trip (export → import → identical graph, including
//! int/float/bool/string/embedding exact bits and valid-time AS OF), absent property →
//! null → absent, the `properties_json` overflow column for bytes/array/sparse and
//! type-conflicting keys (with a decode round-trip), history export shape (one row per
//! version, open `valid_to` = null not sentinel, tombstone, provenance columns),
//! streaming a large export, and an empty database producing a valid empty file.

use std::fs::File;
use std::path::Path;
use std::sync::Arc;

use arrow::array::{
    Array, BooleanArray, Float64Array, RecordBatch, StringArray, TimestampMicrosecondArray,
};
use arrow::datatypes::Schema;
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use tempfile::TempDir;

use super::parquet::overflow_json_to_value;
use crate::api::import::{ColumnType, EdgeMapping, LabelSource, NodeMapping};
use crate::api::transaction::WriteOps;
use crate::core::property::{PropertyMap, PropertyMapBuilder, PropertyValue};
use crate::core::temporal::{TIMESTAMP_MAX, time};
use crate::core::vector::SparseVec;
use crate::test_utils::create_test_db;

/// Read a Parquet file back into its schema and all record batches.
fn read_parquet(path: &Path) -> (Arc<Schema>, Vec<RecordBatch>) {
    let file = File::open(path).expect("open parquet");
    let builder = ParquetRecordBatchReaderBuilder::try_new(file).expect("parquet reader builder");
    let schema = builder.schema().clone();
    let reader = builder.build().expect("parquet reader");
    let batches = reader
        .map(|b| b.expect("read batch"))
        .collect::<Vec<RecordBatch>>();
    (schema, batches)
}

/// Total row count across all batches.
fn total_rows(batches: &[RecordBatch]) -> usize {
    batches.iter().map(RecordBatch::num_rows).sum()
}

/// Names of the columns in schema order.
fn field_names(schema: &Schema) -> Vec<String> {
    schema.fields().iter().map(|f| f.name().clone()).collect()
}

// ---------------------------------------------------------------------------
// Current-state round-trip: export → import → identical graph
// ---------------------------------------------------------------------------

#[test]
fn current_state_round_trip_all_scalar_types_and_embedding() {
    let files = TempDir::new().unwrap();
    let (_tmp, db) = create_test_db().unwrap();

    let embedding: Vec<f32> = vec![1.5, -2.25, 3.0, 0.125];
    let alice = db
        .create_node(
            "Person",
            PropertyMapBuilder::new()
                .insert("name", "Alice")
                .insert("age", 30i64)
                .insert("score", 3.5f64)
                .insert("active", true)
                .insert_vector("embedding", &embedding)
                .build(),
        )
        .unwrap();
    let bob = db
        .create_node(
            "Person",
            PropertyMapBuilder::new()
                .insert("name", "Bob")
                .insert("age", 25i64)
                .build(),
        )
        .unwrap();
    db.create_edge(
        alice,
        bob,
        "KNOWS",
        PropertyMapBuilder::new().insert("since", 2020i64).build(),
    )
    .unwrap();

    let nodes_path = files.path().join("nodes.parquet");
    let edges_path = files.path().join("edges.parquet");
    let node_report = db.export().nodes_to_parquet(&nodes_path).unwrap();
    let edge_report = db.export().edges_to_parquet(&edges_path).unwrap();
    assert_eq!(node_report.nodes_exported, 2);
    assert_eq!(edge_report.edges_exported, 1);

    // Re-import into a fresh database and assert equivalence.
    let (_tmp2, db2) = create_test_db().unwrap();
    let mut importer = db2.import();
    let node_mapping = NodeMapping::new(LabelSource::column("label"), "id")
        .property_same("name", ColumnType::String)
        .property_same("age", ColumnType::Int)
        .property_same("score", ColumnType::Float)
        .property_same("active", ColumnType::Bool)
        .property_same("embedding", ColumnType::Embedding)
        .valid_time_column("valid_time");
    importer
        .nodes_from_parquet(&nodes_path, node_mapping)
        .unwrap();

    let edge_mapping =
        EdgeMapping::new(LabelSource::column("edge_type"), "source_key", "target_key")
            .property_same("since", ColumnType::Int)
            .valid_time_column("valid_time");
    importer
        .edges_from_parquet(&edges_path, edge_mapping)
        .unwrap();

    assert_eq!(db2.node_count(), 2);
    assert_eq!(db2.edge_count(), 1);

    // Business keys are the original node ids (stringified).
    let alice2 = importer.resolve_key(&alice.as_u64().to_string()).unwrap();
    let node = db2.get_node(alice2).unwrap();
    assert_eq!(
        node.properties.get("name"),
        Some(&PropertyValue::string("Alice"))
    );
    assert_eq!(node.properties.get("age"), Some(&PropertyValue::Int(30)));
    assert_eq!(
        node.properties.get("score"),
        Some(&PropertyValue::Float(3.5))
    );
    assert_eq!(
        node.properties.get("active"),
        Some(&PropertyValue::Bool(true))
    );
    let stored = node
        .properties
        .get("embedding")
        .and_then(|v| v.as_vector())
        .expect("embedding present");
    assert_eq!(
        stored.iter().map(|f| f.to_bits()).collect::<Vec<_>>(),
        embedding.iter().map(|f| f.to_bits()).collect::<Vec<_>>(),
        "embedding must round-trip with exact bits"
    );
}

#[test]
fn current_state_export_stamps_file_metadata() {
    let files = TempDir::new().unwrap();
    let (_tmp, db) = create_test_db().unwrap();
    db.create_node("Person", PropertyMap::new()).unwrap();

    let path = files.path().join("nodes.parquet");
    db.export().nodes_to_parquet(&path).unwrap();

    let (schema, _batches) = read_parquet(&path);
    let md = schema.metadata();
    assert_eq!(
        md.get("aletheiadb.schema_version").map(String::as_str),
        Some("1")
    );
    assert_eq!(
        md.get("aletheiadb.mode").map(String::as_str),
        Some("current")
    );
    assert_eq!(
        md.get("aletheiadb.entity").map(String::as_str),
        Some("node")
    );
}

#[test]
fn valid_time_column_reimports_and_answers_as_of() {
    let files = TempDir::new().unwrap();
    let (_tmp, db) = create_test_db().unwrap();
    let id = db
        .create_node(
            "Person",
            PropertyMapBuilder::new().insert("name", "Alice").build(),
        )
        .unwrap();
    let created_valid_from = db
        .get_node_version_read_metadata(db.get_node(id).unwrap().current_version)
        .unwrap()
        .map(|(_, interval)| interval.valid_time().start())
        .unwrap();

    let path = files.path().join("nodes.parquet");
    db.export().nodes_to_parquet(&path).unwrap();

    let (_tmp2, db2) = create_test_db().unwrap();
    let mut importer = db2.import();
    let mapping = NodeMapping::new(LabelSource::column("label"), "id")
        .property_same("name", ColumnType::String)
        .valid_time_column("valid_time");
    importer.nodes_from_parquet(&path, mapping).unwrap();
    let reimported = importer.resolve_key(&id.as_u64().to_string()).unwrap();

    // The reimported node is valid at/after its original valid_from.
    let now = time::now();
    assert!(
        db2.get_node_at_time(reimported, created_valid_from, now)
            .is_ok()
    );
}

// ---------------------------------------------------------------------------
// Absent property → Parquet null → absent on re-import
// ---------------------------------------------------------------------------

#[test]
fn absent_property_is_null_and_reimports_absent() {
    let files = TempDir::new().unwrap();
    let (_tmp, db) = create_test_db().unwrap();
    // A has age; B does not.
    let a = db
        .create_node(
            "Person",
            PropertyMapBuilder::new()
                .insert("name", "A")
                .insert("age", 1i64)
                .build(),
        )
        .unwrap();
    let b = db
        .create_node(
            "Person",
            PropertyMapBuilder::new().insert("name", "B").build(),
        )
        .unwrap();

    let path = files.path().join("nodes.parquet");
    db.export().nodes_to_parquet(&path).unwrap();

    // The age column exists and is null for exactly one row.
    let (schema, batches) = read_parquet(&path);
    assert!(field_names(&schema).contains(&"age".to_string()));
    let age_idx = schema.index_of("age").unwrap();
    let null_count: usize = batches
        .iter()
        .map(|batch| {
            let col = batch.column(age_idx);
            (0..col.len()).filter(|&i| col.is_null(i)).count()
        })
        .sum();
    assert_eq!(null_count, 1, "exactly B's age must be null");

    // Re-import: B has no age property (absent, not a stored null).
    let (_tmp2, db2) = create_test_db().unwrap();
    let mut importer = db2.import();
    let mapping = NodeMapping::new(LabelSource::column("label"), "id")
        .property_same("name", ColumnType::String)
        .property_same("age", ColumnType::Int);
    importer.nodes_from_parquet(&path, mapping).unwrap();

    let b2 = importer.resolve_key(&b.as_u64().to_string()).unwrap();
    assert_eq!(db2.get_node(b2).unwrap().properties.get("age"), None);
    let a2 = importer.resolve_key(&a.as_u64().to_string()).unwrap();
    assert_eq!(
        db2.get_node(a2).unwrap().properties.get("age"),
        Some(&PropertyValue::Int(1))
    );
}

// ---------------------------------------------------------------------------
// Overflow column: bytes / array / sparse vector round-trip through JSON
// ---------------------------------------------------------------------------

#[test]
fn overflow_column_round_trips_bytes_array_and_sparse() {
    let files = TempDir::new().unwrap();
    let (_tmp, db) = create_test_db().unwrap();

    let bytes = PropertyValue::bytes(vec![1u8, 2, 3, 255]);
    let array = PropertyValue::array(vec![
        PropertyValue::Int(7),
        PropertyValue::string("mixed"),
        PropertyValue::Bool(false),
    ]);
    let sparse = PropertyValue::sparse_vector(
        SparseVec::new(vec![1u32, 5], vec![0.5f32, -0.25], 16).unwrap(),
    );

    db.create_node(
        "Doc",
        PropertyMapBuilder::new()
            .insert("blob", bytes.clone())
            .insert("tags", array.clone())
            .insert("sparse", sparse.clone())
            .insert("title", "keep-native")
            .build(),
    )
    .unwrap();

    let path = files.path().join("nodes.parquet");
    db.export().nodes_to_parquet(&path).unwrap();

    let (schema, batches) = read_parquet(&path);
    let names = field_names(&schema);
    // The exotic keys are NOT native columns; the native scalar still is.
    assert!(names.contains(&"properties_json".to_string()));
    assert!(names.contains(&"title".to_string()));
    assert!(!names.contains(&"blob".to_string()));
    assert!(!names.contains(&"sparse".to_string()));

    // Decode properties_json and confirm each value round-trips exactly.
    let json_idx = schema.index_of("properties_json").unwrap();
    let col = batches[0]
        .column(json_idx)
        .as_any()
        .downcast_ref::<StringArray>()
        .unwrap();
    let obj: serde_json::Value = serde_json::from_str(col.value(0)).unwrap();
    let map = obj.as_object().unwrap();

    assert_eq!(overflow_json_to_value(&map["blob"]).unwrap(), bytes);
    assert_eq!(overflow_json_to_value(&map["tags"]).unwrap(), array);
    assert_eq!(overflow_json_to_value(&map["sparse"]).unwrap(), sparse);
}

#[test]
fn type_conflicting_key_routes_to_overflow() {
    let files = TempDir::new().unwrap();
    let (_tmp, db) = create_test_db().unwrap();
    // Same key "x" is Int on one node, String on another => conflict => overflow.
    db.create_node("T", PropertyMapBuilder::new().insert("x", 42i64).build())
        .unwrap();
    db.create_node("T", PropertyMapBuilder::new().insert("x", "hello").build())
        .unwrap();

    let path = files.path().join("nodes.parquet");
    db.export().nodes_to_parquet(&path).unwrap();

    let (schema, batches) = read_parquet(&path);
    let names = field_names(&schema);
    assert!(
        !names.contains(&"x".to_string()),
        "conflicting key must not be a native column"
    );
    assert!(names.contains(&"properties_json".to_string()));

    // Both rows carry a decodable tagged value under "x".
    let json_idx = schema.index_of("properties_json").unwrap();
    let mut decoded = Vec::new();
    for batch in &batches {
        let col = batch
            .column(json_idx)
            .as_any()
            .downcast_ref::<StringArray>()
            .unwrap();
        for i in 0..col.len() {
            let v: serde_json::Value = serde_json::from_str(col.value(i)).unwrap();
            decoded.push(overflow_json_to_value(&v.as_object().unwrap()["x"]).unwrap());
        }
    }
    assert!(decoded.contains(&PropertyValue::Int(42)));
    assert!(decoded.contains(&PropertyValue::string("hello")));
}

// ---------------------------------------------------------------------------
// Overflow column auto-expands on re-import (end-to-end, no manual key mapping)
// ---------------------------------------------------------------------------

#[test]
fn overflow_column_auto_expands_on_reimport_end_to_end() {
    let files = TempDir::new().unwrap();
    let (_tmp, db) = create_test_db().unwrap();

    let bytes = PropertyValue::bytes(vec![1u8, 2, 3, 255, 0, 128]);
    let array = PropertyValue::array(vec![
        PropertyValue::Int(7),
        PropertyValue::string("mixed"),
        PropertyValue::Bool(false),
        PropertyValue::Float(2.5),
    ]);
    let sparse = PropertyValue::sparse_vector(
        SparseVec::new(vec![1u32, 5, 9], vec![0.5f32, -0.25, 1.75], 16).unwrap(),
    );

    let doc = db
        .create_node(
            "Doc",
            PropertyMapBuilder::new()
                .insert("title", "keep-native")
                .insert("blob", bytes.clone())
                .insert("tags", array.clone())
                .insert("sparse", sparse.clone())
                .build(),
        )
        .unwrap();

    let path = files.path().join("nodes.parquet");
    db.export().nodes_to_parquet(&path).unwrap();

    // Re-import into a fresh database. The mapping declares ONLY the native `title`
    // column; the exotic overflow keys are auto-expanded from `properties_json`.
    let (_tmp2, db2) = create_test_db().unwrap();
    let mut importer = db2.import();
    let mapping = NodeMapping::new(LabelSource::column("label"), "id")
        .property_same("title", ColumnType::String);
    importer.nodes_from_parquet(&path, mapping).unwrap();

    let id = importer.resolve_key(&doc.as_u64().to_string()).unwrap();
    let node = db2.get_node(id).unwrap();
    assert_eq!(
        node.properties.get("title"),
        Some(&PropertyValue::string("keep-native"))
    );
    // Byte / element-identical round trip of the overflow values.
    assert_eq!(node.properties.get("blob"), Some(&bytes));
    assert_eq!(node.properties.get("tags"), Some(&array));
    assert_eq!(node.properties.get("sparse"), Some(&sparse));
}

#[test]
fn overflow_non_finite_floats_round_trip() {
    let files = TempDir::new().unwrap();
    let (_tmp, db) = create_test_db().unwrap();

    // A heterogeneous array (overflow-routed) carrying non-finite floats, which
    // serde_json would otherwise map to `null`.
    let array = PropertyValue::array(vec![
        PropertyValue::Float(f64::NAN),
        PropertyValue::Float(f64::INFINITY),
        PropertyValue::Float(f64::NEG_INFINITY),
        PropertyValue::Float(1.5),
    ]);
    let doc = db
        .create_node(
            "Doc",
            PropertyMapBuilder::new().insert("arr", array).build(),
        )
        .unwrap();

    let path = files.path().join("nodes.parquet");
    db.export().nodes_to_parquet(&path).unwrap();

    let (_tmp2, db2) = create_test_db().unwrap();
    let mut importer = db2.import();
    importer
        .nodes_from_parquet(&path, NodeMapping::new(LabelSource::column("label"), "id"))
        .unwrap();
    let id = importer.resolve_key(&doc.as_u64().to_string()).unwrap();
    let node = db2.get_node(id).unwrap();

    let items = node
        .properties
        .get("arr")
        .and_then(PropertyValue::as_array)
        .expect("array present");
    assert_eq!(items.len(), 4);
    assert!(matches!(items[0], PropertyValue::Float(f) if f.is_nan()));
    assert!(matches!(items[1], PropertyValue::Float(f) if f == f64::INFINITY));
    assert!(matches!(items[2], PropertyValue::Float(f) if f == f64::NEG_INFINITY));
    assert!(matches!(items[3], PropertyValue::Float(f) if f == 1.5));
}

// ---------------------------------------------------------------------------
// History export: one row per version, open valid_to null, tombstone, provenance
// ---------------------------------------------------------------------------

#[test]
fn node_history_export_one_row_per_version_with_open_valid_to_null() {
    let files = TempDir::new().unwrap();
    let (_tmp, db) = create_test_db().unwrap();
    let id = db
        .create_node(
            "Person",
            PropertyMapBuilder::new()
                .insert("title", "Engineer")
                .build(),
        )
        .unwrap();
    // Second version.
    db.write(|tx| {
        tx.update_node(
            id,
            PropertyMapBuilder::new().insert("title", "Staff").build(),
        )
    })
    .unwrap();

    let versions = db.get_node_history(id).unwrap().versions.len();
    assert!(versions >= 2, "expected at least two versions");

    let path = files.path().join("node_history.parquet");
    let report = db.export().node_history_to_parquet(&path).unwrap();
    assert_eq!(report.node_versions_exported, versions);

    let (schema, batches) = read_parquet(&path);
    assert_eq!(total_rows(&batches), versions, "one row per version");

    let names = field_names(&schema);
    for expected in [
        "id",
        "label",
        "version",
        "valid_from",
        "valid_to",
        "transaction_time",
        "tombstone",
        "provenance_source",
        "provenance_confidence",
        "provenance_note",
        "provenance_correlation_id",
        "provenance_principal",
    ] {
        assert!(
            names.contains(&expected.to_string()),
            "missing column {expected}"
        );
    }

    // The still-open current version has a null valid_to (never the sentinel).
    let valid_to_idx = schema.index_of("valid_to").unwrap();
    let batch = &batches[0];
    let valid_to = batch
        .column(valid_to_idx)
        .as_any()
        .downcast_ref::<TimestampMicrosecondArray>()
        .unwrap();
    let open_rows = (0..valid_to.len()).filter(|&i| valid_to.is_null(i)).count();
    assert!(
        open_rows >= 1,
        "the open current version must have null valid_to"
    );
    // No row encodes the open-interval sentinel as a real timestamp.
    for i in 0..valid_to.len() {
        if !valid_to.is_null(i) {
            assert_ne!(valid_to.value(i), TIMESTAMP_MAX.wallclock());
            assert_ne!(valid_to.value(i), i64::MAX);
        }
    }

    // tombstone is false for the still-valid versions.
    let tomb_idx = schema.index_of("tombstone").unwrap();
    let tomb = batch
        .column(tomb_idx)
        .as_any()
        .downcast_ref::<BooleanArray>()
        .unwrap();
    assert!(
        (0..tomb.len()).any(|i| !tomb.value(i)),
        "at least one non-tombstone version"
    );
}

#[test]
fn node_history_export_marks_tombstone_and_closes_valid_to_on_retract() {
    let files = TempDir::new().unwrap();
    let (_tmp, db) = create_test_db().unwrap();
    let id = db
        .create_node(
            "Person",
            PropertyMapBuilder::new().insert("name", "Alice").build(),
        )
        .unwrap();
    // Close the valid interval (retraction / tombstone).
    // A tiny sleep so the retraction's valid_to is strictly after the creation.
    std::thread::sleep(std::time::Duration::from_millis(3));
    let cutoff = time::now();
    db.retract_node(id, cutoff).unwrap();

    let path = files.path().join("node_history.parquet");
    db.export().node_history_to_parquet(&path).unwrap();

    let (schema, batches) = read_parquet(&path);
    let tomb_idx = schema.index_of("tombstone").unwrap();
    let valid_to_idx = schema.index_of("valid_to").unwrap();

    let mut any_tombstone = false;
    for batch in &batches {
        let tomb = batch
            .column(tomb_idx)
            .as_any()
            .downcast_ref::<BooleanArray>()
            .unwrap();
        let valid_to = batch
            .column(valid_to_idx)
            .as_any()
            .downcast_ref::<TimestampMicrosecondArray>()
            .unwrap();
        for i in 0..tomb.len() {
            if tomb.value(i) {
                any_tombstone = true;
                assert!(
                    !valid_to.is_null(i),
                    "a tombstone row must have a closed (non-null) valid_to"
                );
            }
        }
    }
    assert!(any_tombstone, "retraction must produce a tombstone version");
}

#[test]
fn node_history_export_carries_provenance_columns() {
    use crate::api::transaction::WriteRequestOptions;
    use crate::core::provenance::Provenance;

    let files = TempDir::new().unwrap();
    let (_tmp, db) = create_test_db().unwrap();
    let prov = Provenance::from_parts(
        Some("ingest".to_string()),
        Some(0.9),
        Some("seed".to_string()),
        None,
        None,
    )
    .unwrap();
    db.write(|tx| {
        tx.create_node_with_options(
            "Person",
            PropertyMapBuilder::new().insert("name", "Alice").build(),
            WriteRequestOptions::new().with_provenance(prov.clone()),
        )
    })
    .unwrap();

    let path = files.path().join("node_history.parquet");
    db.export().node_history_to_parquet(&path).unwrap();

    let (schema, batches) = read_parquet(&path);
    let src_idx = schema.index_of("provenance_source").unwrap();
    let conf_idx = schema.index_of("provenance_confidence").unwrap();
    let src = batches[0]
        .column(src_idx)
        .as_any()
        .downcast_ref::<StringArray>()
        .unwrap();
    let conf = batches[0]
        .column(conf_idx)
        .as_any()
        .downcast_ref::<Float64Array>()
        .unwrap();
    assert_eq!(src.value(0), "ingest");
    assert!((conf.value(0) - 0.9).abs() < 1e-9);
}

// ---------------------------------------------------------------------------
// Deleted-edge history recovers its endpoints (not null)
// ---------------------------------------------------------------------------

#[test]
fn deleted_edge_history_recovers_source_and_target_keys() {
    use crate::api::transaction::WriteOps;

    let files = TempDir::new().unwrap();
    let (_tmp, db) = create_test_db().unwrap();

    let alice = db.create_node("Person", PropertyMap::new()).unwrap();
    let bob = db.create_node("Person", PropertyMap::new()).unwrap();
    let edge = db
        .create_edge(alice, bob, "KNOWS", PropertyMap::new())
        .unwrap();
    // Delete the edge: it leaves current state but keeps its history.
    db.write(|tx| tx.delete_edge(edge)).unwrap();

    let path = files.path().join("edge_history.parquet");
    let report = db.export().edge_history_to_parquet(&path).unwrap();
    assert!(report.edge_versions_exported >= 1);

    let (schema, batches) = read_parquet(&path);
    let src_idx = schema.index_of("source_key").unwrap();
    let tgt_idx = schema.index_of("target_key").unwrap();

    let mut rows = 0usize;
    for batch in &batches {
        let src = batch
            .column(src_idx)
            .as_any()
            .downcast_ref::<arrow::array::Int64Array>()
            .unwrap();
        let tgt = batch
            .column(tgt_idx)
            .as_any()
            .downcast_ref::<arrow::array::Int64Array>()
            .unwrap();
        for i in 0..src.len() {
            rows += 1;
            assert!(!src.is_null(i), "deleted edge source_key must be recovered");
            assert!(!tgt.is_null(i), "deleted edge target_key must be recovered");
            assert_eq!(src.value(i), alice.as_u64() as i64);
            assert_eq!(tgt.value(i), bob.as_u64() as i64);
        }
    }
    assert!(rows >= 1, "expected at least one historical edge version");
}

// ---------------------------------------------------------------------------
// History export carries the transaction-interval END (transaction_to)
// ---------------------------------------------------------------------------

#[test]
fn node_history_export_has_transaction_to_column_null_for_open() {
    let files = TempDir::new().unwrap();
    let (_tmp, db) = create_test_db().unwrap();
    db.create_node(
        "Person",
        PropertyMapBuilder::new().insert("name", "Alice").build(),
    )
    .unwrap();

    let path = files.path().join("node_history.parquet");
    db.export().node_history_to_parquet(&path).unwrap();

    let (schema, batches) = read_parquet(&path);
    let names = field_names(&schema);
    assert!(
        names.contains(&"transaction_to".to_string()),
        "history schema must expose transaction_to"
    );

    // The current, still-recorded version has an OPEN transaction interval => null.
    let tx_to_idx = schema.index_of("transaction_to").unwrap();
    let open_rows: usize = batches
        .iter()
        .map(|batch| {
            let col = batch
                .column(tx_to_idx)
                .as_any()
                .downcast_ref::<TimestampMicrosecondArray>()
                .unwrap();
            (0..col.len()).filter(|&i| col.is_null(i)).count()
        })
        .sum();
    assert!(
        open_rows >= 1,
        "the current version must have a null (open) transaction_to"
    );
}

// ---------------------------------------------------------------------------
// Tombstone pin: tombstone == "this version's valid interval is closed" (v1)
// ---------------------------------------------------------------------------

#[test]
fn node_history_tombstone_equals_closed_valid_interval() {
    let files = TempDir::new().unwrap();
    let (_tmp, db) = create_test_db().unwrap();
    let id = db
        .create_node(
            "Person",
            PropertyMapBuilder::new()
                .insert("title", "Engineer")
                .build(),
        )
        .unwrap();
    // A valid-time supersession (ordinary update, NOT a delete/retract).
    db.write(|tx| {
        tx.update_node(
            id,
            PropertyMapBuilder::new().insert("title", "Staff").build(),
        )
    })
    .unwrap();

    let path = files.path().join("node_history.parquet");
    db.export().node_history_to_parquet(&path).unwrap();

    let (schema, batches) = read_parquet(&path);
    let tomb_idx = schema.index_of("tombstone").unwrap();
    let valid_to_idx = schema.index_of("valid_to").unwrap();

    // Pin the documented v1 semantics exactly: a row is a tombstone iff its valid
    // interval is closed (non-null valid_to) — which also flags valid-time-superseded
    // versions, not only deletes/retractions.
    for batch in &batches {
        let tomb = batch
            .column(tomb_idx)
            .as_any()
            .downcast_ref::<BooleanArray>()
            .unwrap();
        let valid_to = batch
            .column(valid_to_idx)
            .as_any()
            .downcast_ref::<TimestampMicrosecondArray>()
            .unwrap();
        for i in 0..tomb.len() {
            assert_eq!(
                tomb.value(i),
                !valid_to.is_null(i),
                "tombstone must equal (valid interval closed)"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Streaming / large export and empty database
// ---------------------------------------------------------------------------

#[test]
fn large_export_streams_all_rows_across_row_groups() {
    let files = TempDir::new().unwrap();
    let (_tmp, db) = create_test_db().unwrap();

    // Create 50_000 nodes in a single transaction (fast), then stream-export them
    // with a small batch so many row groups are produced with bounded memory.
    const N: usize = 50_000;
    db.write(|tx| {
        for i in 0..N {
            tx.create_node("N", PropertyMapBuilder::new().insert("i", i as i64).build())?;
        }
        Ok::<(), crate::core::error::Error>(())
    })
    .unwrap();

    let path = files.path().join("big.parquet");
    let report = db
        .export()
        .batch_size(1000)
        .nodes_to_parquet(&path)
        .unwrap();
    assert_eq!(report.nodes_exported, N);

    let (_schema, batches) = read_parquet(&path);
    assert_eq!(total_rows(&batches), N);
    // Small batch => many row groups (each written batch is one row group).
    assert!(batches.len() >= 2, "expected multiple row groups");
}

#[test]
fn empty_database_produces_valid_empty_file() {
    let files = TempDir::new().unwrap();
    let (_tmp, db) = create_test_db().unwrap();

    let path = files.path().join("empty.parquet");
    let report = db.export().nodes_to_parquet(&path).unwrap();
    assert_eq!(report.nodes_exported, 0);

    let (schema, batches) = read_parquet(&path);
    assert_eq!(total_rows(&batches), 0);
    // The fixed schema is still present and readable.
    let names = field_names(&schema);
    assert!(names.contains(&"id".to_string()));
    assert!(names.contains(&"label".to_string()));
    assert!(names.contains(&"valid_time".to_string()));
}

#[test]
fn empty_edge_history_produces_valid_empty_file() {
    let files = TempDir::new().unwrap();
    let (_tmp, db) = create_test_db().unwrap();
    // Nodes but no edges.
    db.create_node("Person", PropertyMap::new()).unwrap();

    let path = files.path().join("edge_history.parquet");
    let report = db.export().edge_history_to_parquet(&path).unwrap();
    assert_eq!(report.edge_versions_exported, 0);

    let (schema, batches) = read_parquet(&path);
    assert_eq!(total_rows(&batches), 0);
    let names = field_names(&schema);
    assert!(names.contains(&"edge_type".to_string()));
    assert!(names.contains(&"source_key".to_string()));
    assert!(names.contains(&"valid_to".to_string()));
}

/// Assert no exported Parquet field name nor any Utf8 cell carries the
/// engine-reserved namespace ride-along key (Issue #3349).
fn assert_no_reserved_in_parquet(schema: &Schema, batches: &[RecordBatch]) {
    for name in field_names(schema) {
        assert!(
            !name.contains("__aletheia_"),
            "reserved ride-along key leaked into a Parquet column name: {name}"
        );
    }
    for batch in batches {
        for col in batch.columns() {
            if let Some(arr) = col.as_any().downcast_ref::<StringArray>() {
                for i in 0..arr.len() {
                    if arr.is_valid(i) {
                        assert!(
                            !arr.value(i).contains("__aletheia_"),
                            "reserved ride-along key leaked into a Parquet Utf8 cell: {}",
                            arr.value(i)
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn namespaced_export_elides_reserved_ride_along_key() {
    let files = TempDir::new().unwrap();
    let (_tmp, db) = create_test_db().unwrap();

    let a = db
        .create_node_in_namespace(
            "Person",
            PropertyMapBuilder::new().insert("name", "Alice").build(),
            "agent:planner",
        )
        .unwrap();
    let b = db
        .create_node_in_namespace(
            "Person",
            PropertyMapBuilder::new().insert("name", "Bob").build(),
            "agent:planner",
        )
        .unwrap();
    db.create_edge_in_namespace(
        a,
        b,
        "KNOWS",
        PropertyMapBuilder::new().insert("since", 2020i64).build(),
        "agent:planner",
    )
    .unwrap();

    let nodes_path = files.path().join("nodes.parquet");
    let edges_path = files.path().join("edges.parquet");
    let node_hist_path = files.path().join("node_hist.parquet");
    db.export().nodes_to_parquet(&nodes_path).unwrap();
    db.export().edges_to_parquet(&edges_path).unwrap();
    db.export()
        .node_history_to_parquet(&node_hist_path)
        .unwrap();

    for path in [&nodes_path, &edges_path, &node_hist_path] {
        let (schema, batches) = read_parquet(path);
        assert_no_reserved_in_parquet(&schema, &batches);
    }
}
