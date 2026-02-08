//! Benchmarks for query optimization
//!
//! Compares optimized vs unoptimized query plans to validate that:
//! - Operation reordering reduces execution time
//! - Filter pushdown improves performance
//! - Cost-based optimization makes correct decisions
//!
//! Performance Expectations:
//! - Optimized plans should be 2-10x faster for complex queries
//! - Simple queries should have minimal overhead

mod common;

use aletheiadb::PropertyMapBuilder;
use aletheiadb::core::NodeId;
use aletheiadb::index::vector::{DistanceMetric, hnsw::HnswConfig};
use aletheiadb::query::builder::QueryBuilder;
use aletheiadb::query::planner::{QueryPlanner, Statistics};
use aletheiadb::storage::CurrentStorage;
use criterion::{Criterion, criterion_group, criterion_main};
use std::hint::black_box;
use std::sync::Arc;

/// Create a test graph for query benchmarks
fn create_test_graph(node_count: usize) -> Arc<CurrentStorage> {
    let storage = Arc::new(CurrentStorage::new());

    // Enable vector index
    let config = HnswConfig::new(4, DistanceMetric::Cosine);
    storage.enable_vector_index("embedding", config).unwrap();

    // Create nodes with properties and embeddings
    for i in 0..node_count {
        let embedding = vec![(i as f32) / (node_count as f32), 0.5, 0.3, 0.2];

        storage
            .create_node(
                if i % 3 == 0 { "Person" } else { "Document" },
                PropertyMapBuilder::new()
                    .insert("id", i as i64)
                    .insert("score", (i * 10) as i64)
                    .insert("active", i % 2 == 0)
                    .insert_vector("embedding", &embedding)
                    .build(),
            )
            .unwrap();
    }

    // Create edges
    let node_ids: Vec<NodeId> = (0..node_count)
        .map(|i| NodeId::new((i + 1) as u64).unwrap())
        .collect();

    for i in 0..node_count.saturating_sub(1) {
        storage
            .create_edge(
                node_ids[i],
                node_ids[i + 1],
                if i % 2 == 0 { "KNOWS" } else { "RELATED" },
                PropertyMapBuilder::new().build(),
            )
            .unwrap();
    }

    storage
}

/// Benchmark planning overhead (should be minimal)
fn bench_planning_overhead(c: &mut Criterion) {
    let storage = create_test_graph(100);
    let stats = Arc::new(Statistics::default());
    let planner = QueryPlanner::new(Arc::clone(&stats), Arc::clone(&storage));

    let mut group = c.benchmark_group("planning_overhead");

    // Simple node lookup - no optimization benefit
    group.bench_function("simple_lookup", |b| {
        b.iter(|| {
            let query = QueryBuilder::new().start(NodeId::new(1).unwrap()).build();
            let _plan = planner.plan(query).unwrap();
        });
    });

    // Complex query with filters
    group.bench_function("complex_with_filters", |b| {
        b.iter(|| {
            let query = QueryBuilder::new()
                .scan_label("Person")
                .filter(aletheiadb::query::ir::Predicate::eq("active", true))
                .filter(aletheiadb::query::ir::Predicate::gt("score", 50i64))
                .limit(10)
                .build();
            let _plan = planner.plan(query).unwrap();
        });
    });

    group.finish();
}

/// Benchmark explain() performance
fn bench_explain(c: &mut Criterion) {
    let storage = create_test_graph(100);
    let stats = Arc::new(Statistics::default());
    let planner = QueryPlanner::new(Arc::clone(&stats), Arc::clone(&storage));

    // Create a complex query plan
    let query = QueryBuilder::new()
        .scan_label("Person")
        .filter(aletheiadb::query::ir::Predicate::eq("active", true))
        .traverse("KNOWS")
        .limit(10)
        .build();

    let plan = planner.plan(query).unwrap();

    c.bench_function("explain_complex_plan", |b| {
        b.iter(|| {
            let _explanation = black_box(plan.explain());
        });
    });
}

/// Benchmark cost estimation
fn bench_cost_estimation(c: &mut Criterion) {
    use aletheiadb::query::planner::{CostModel, physical::PhysicalOp};

    let _storage = create_test_graph(100);
    let stats = Arc::new(Statistics::default());
    let cost_model = CostModel::default();

    let mut group = c.benchmark_group("cost_estimation");

    // Simple operator
    let simple_op = PhysicalOp::NodeLookup {
        node_ids: vec![NodeId::new(1).unwrap()],
    };

    group.bench_function("simple_operator", |b| {
        b.iter(|| {
            let _cost = cost_model.estimate(black_box(&simple_op), &stats);
        });
    });

    // Complex nested operator
    let complex_op = PhysicalOp::Filter {
        input: Box::new(PhysicalOp::IndexedTraversal {
            input: Box::new(PhysicalOp::NodeLookup {
                node_ids: vec![NodeId::new(1).unwrap()],
            }),
            direction: aletheiadb::query::ir::Direction::Outgoing,
            label: Some("KNOWS".to_string()),
            depth: 2,
            temporal_context: None,
        }),
        predicate: aletheiadb::query::ir::Predicate::eq("active", true),
    };

    group.bench_function("complex_nested", |b| {
        b.iter(|| {
            let _cost = cost_model.estimate(black_box(&complex_op), &stats);
        });
    });

    group.finish();
}

/// Benchmark statistics collection
fn bench_statistics(c: &mut Criterion) {
    let stats = Statistics::default();

    // Simulate statistics refresh
    stats.refresh(1000, 5000, 100, vec![], 5.0);

    let mut group = c.benchmark_group("statistics");

    group.bench_function("cardinality_lookup", |b| {
        b.iter(|| {
            let _count = stats.node_count();
            let _edges = stats.edge_count();
            let _degree = stats.average_out_degree();
        });
    });

    group.bench_function("selectivity_estimation", |b| {
        b.iter(|| {
            let _sel = stats.estimate_selectivity("name", "Alice");
        });
    });

    group.finish();
}

criterion_group!(
    benches,
    bench_planning_overhead,
    bench_explain,
    bench_cost_estimation,
    bench_statistics,
);

criterion_main!(benches);
