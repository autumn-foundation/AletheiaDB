//! Bulk import throughput benchmark (Issue #3211).
//!
//! Compares the batched bulk importer against a naive per-row `create_node` loop.
//!
//! Success metric (from the issue): loading a 1M-node / 5M-edge CSV pair completes in
//! < 60s in GroupCommit mode on commodity hardware, with >= 20% higher throughput than a
//! naive per-row loop. This bench measures that relative win on a smaller, CI-friendly
//! row count; scale `ROWS` (or `BENCH_IMPORT_ROWS`) up to reproduce the production target.

mod common;

use std::io::Write;

use aletheiadb::AletheiaDB;
use aletheiadb::api::import::{ColumnType, LabelSource, NodeMapping};
use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use std::hint::black_box;

/// Default row count for the benchmark (override with `BENCH_IMPORT_ROWS`).
const ROWS: usize = 10_000;

fn rows() -> usize {
    std::env::var("BENCH_IMPORT_ROWS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(ROWS)
}

/// Build a node CSV with `n` rows in a temp dir and return (dir, path).
fn make_node_csv(n: usize) -> (tempfile::TempDir, std::path::PathBuf) {
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

fn node_mapping() -> NodeMapping {
    NodeMapping::new(LabelSource::fixed("Person"), "id")
        .property("name", "name", ColumnType::String)
        .property("age", "age", ColumnType::Int)
}

fn bench_import(c: &mut Criterion) {
    let n = rows();
    let (_dir, csv_path) = make_node_csv(n);

    let mut group = c.benchmark_group("bulk_import_nodes");
    group.throughput(Throughput::Elements(n as u64));

    // Batched importer: one commit per batch through the WAL append_batch path.
    group.bench_function("importer_batched", |b| {
        b.iter(|| {
            let db = AletheiaDB::new().expect("db");
            let mut importer = db.import();
            let report = importer
                .nodes_from_csv(&csv_path, node_mapping())
                .expect("import");
            black_box(report.nodes_imported);
        });
    });

    // Naive baseline: one write transaction (one commit) per row.
    group.bench_function("naive_per_row", |b| {
        use aletheiadb::PropertyMapBuilder;
        b.iter(|| {
            let db = AletheiaDB::new().expect("db");
            for i in 0..n {
                let props = PropertyMapBuilder::new()
                    .insert("name", format!("Name{i}"))
                    .insert("age", (i % 100) as i64)
                    .build();
                db.create_node("Person", props).expect("create_node");
            }
            black_box(db.node_count());
        });
    });

    group.finish();
}

criterion_group!(
    name = benches;
    config = common::configure_criterion();
    targets = bench_import
);
criterion_main!(benches);
