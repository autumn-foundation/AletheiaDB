//! Parquet import/export throughput benchmark (Issue #3364).
//!
//! Two axes are measured:
//!
//! 1. **Import**: the batched Parquet importer against the batched CSV importer over the
//!    *same* row data, so the ratio answers "how much faster is native columnar Parquet
//!    decode than string-parsing CSV?".
//! 2. **Export**: current-state node/edge export and full bi-temporal history export,
//!    exercising the two-pass schema-discovery + streaming row-group writer.
//!
//! Success metric (from the issue): a 1M-node export completes well under 1 GB of peak
//! memory and streams without buffering the whole graph. This bench measures directional
//! throughput on a smaller, CI-friendly row count; scale `ROWS` (or `BENCH_PARQUET_ROWS`)
//! up to reproduce the production target (do not run 1M casually — it is slow and heavy).

#![cfg(feature = "parquet")]

mod common;

use std::hint::black_box;
use std::io::Write;
use std::path::PathBuf;

use arrow::array::{ArrayRef, Int64Array, RecordBatch, StringArray};
use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use parquet::arrow::ArrowWriter;
use std::sync::Arc;
use tempfile::TempDir;

use aletheiadb::AletheiaDB;
use aletheiadb::PropertyMapBuilder;
use aletheiadb::api::import::{ColumnType, LabelSource, NodeMapping};

/// Default row count for the benchmark (override with `BENCH_PARQUET_ROWS`).
const ROWS: usize = 10_000;

fn rows() -> usize {
    std::env::var("BENCH_PARQUET_ROWS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(ROWS)
}

fn node_mapping() -> NodeMapping {
    NodeMapping::new(LabelSource::fixed("Person"), "id")
        .property("name", "name", ColumnType::String)
        .property("age", "age", ColumnType::Int)
}

/// Build a node CSV with `n` rows and return `(dir, path)`.
fn make_node_csv(n: usize) -> (TempDir, PathBuf) {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("nodes.csv");
    let mut file = std::fs::File::create(&path).expect("create csv");
    writeln!(file, "id,name,age").unwrap();
    for i in 0..n {
        writeln!(file, "n{i},Name{i},{}", i % 100).unwrap();
    }
    file.flush().unwrap();
    (dir, path)
}

/// Build a node Parquet file with the *same* `n` rows as [`make_node_csv`] and return
/// `(dir, path)`. `id`/`name` are string columns, `age` is a native int64 column.
fn make_node_parquet(n: usize) -> (TempDir, PathBuf) {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("nodes.parquet");

    let ids: Vec<String> = (0..n).map(|i| format!("n{i}")).collect();
    let names: Vec<String> = (0..n).map(|i| format!("Name{i}")).collect();
    let ages: Vec<i64> = (0..n).map(|i| (i % 100) as i64).collect();

    let batch = RecordBatch::try_from_iter(vec![
        ("id", Arc::new(StringArray::from(ids)) as ArrayRef),
        ("name", Arc::new(StringArray::from(names)) as ArrayRef),
        ("age", Arc::new(Int64Array::from(ages)) as ArrayRef),
    ])
    .expect("record batch");

    let file = std::fs::File::create(&path).expect("create parquet");
    let mut writer = ArrowWriter::try_new(file, batch.schema(), None).expect("arrow writer");
    writer.write(&batch).expect("write batch");
    writer.close().expect("close writer");
    (dir, path)
}

/// Import throughput: native Parquet decode vs CSV string-parsing over identical rows.
fn bench_parquet_import(c: &mut Criterion) {
    let n = rows();
    let (_csv_dir, csv_path) = make_node_csv(n);
    let (_pq_dir, pq_path) = make_node_parquet(n);

    let mut group = c.benchmark_group("parquet_import_nodes");
    group.throughput(Throughput::Elements(n as u64));

    group.bench_function("parquet", |b| {
        b.iter(|| {
            let db = AletheiaDB::new().expect("db");
            let mut importer = db.import();
            let report = importer
                .nodes_from_parquet(&pq_path, node_mapping())
                .expect("parquet import");
            black_box(report.nodes_imported);
        });
    });

    group.bench_function("csv", |b| {
        b.iter(|| {
            let db = AletheiaDB::new().expect("db");
            let mut importer = db.import();
            let report = importer
                .nodes_from_csv(&csv_path, node_mapping())
                .expect("csv import");
            black_box(report.nodes_imported);
        });
    });

    group.finish();
}

/// Build a database of `n` two-version nodes plus a sparse set of edges, so both
/// current-state and history export have real work to do. Built once and reused across
/// iterations (export is read-only).
fn build_export_db(n: usize) -> AletheiaDB {
    let db = AletheiaDB::new().expect("db");
    let mut ids = Vec::with_capacity(n);
    for i in 0..n {
        let props = PropertyMapBuilder::new()
            .insert("name", format!("Name{i}"))
            .insert("age", (i % 100) as i64)
            .build();
        ids.push(db.create_node("Person", props).expect("create_node"));
    }
    // A second version of every node so history export streams two versions per node.
    for (i, &id) in ids.iter().enumerate() {
        let props = PropertyMapBuilder::new()
            .insert("name", format!("Name{i}"))
            .insert("age", ((i % 100) + 1) as i64)
            .build();
        db.update_node_with_valid_time(id, props, None)
            .expect("update_node");
    }
    // A sparse spray of edges (one per 100 nodes) for the edge-export path.
    let mut i = 0;
    while i + 1 < ids.len() {
        let props = PropertyMapBuilder::new().insert("since", 2020i64).build();
        db.create_edge(ids[i], ids[i + 1], "KNOWS", props)
            .expect("create_edge");
        i += 100;
    }
    db
}

/// Export throughput: current-state node/edge export and full history export.
fn bench_parquet_export(c: &mut Criterion) {
    let n = rows();
    let db = build_export_db(n);

    let mut group = c.benchmark_group("parquet_export");
    group.throughput(Throughput::Elements(n as u64));

    group.bench_function("current_nodes", |b| {
        b.iter(|| {
            let dir = tempfile::tempdir().expect("tempdir");
            let path = dir.path().join("nodes.parquet");
            let report = db.export().nodes_to_parquet(&path).expect("export nodes");
            black_box(report.nodes_exported);
        });
    });

    group.bench_function("current_edges", |b| {
        b.iter(|| {
            let dir = tempfile::tempdir().expect("tempdir");
            let path = dir.path().join("edges.parquet");
            let report = db.export().edges_to_parquet(&path).expect("export edges");
            black_box(report.edges_exported);
        });
    });

    group.bench_function("history_nodes", |b| {
        b.iter(|| {
            let dir = tempfile::tempdir().expect("tempdir");
            let path = dir.path().join("node_history.parquet");
            let report = db
                .export()
                .node_history_to_parquet(&path)
                .expect("export node history");
            black_box(report.node_versions_exported);
        });
    });

    group.finish();
}

criterion_group!(
    name = benches;
    config = common::configure_criterion();
    targets = bench_parquet_import, bench_parquet_export
);
criterion_main!(benches);
