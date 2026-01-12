//! Benchmark suite for hybrid query patterns (VS-070).
//!
//! Measures performance of queries that combine:
//! - Temporal queries (time-travel)
//! - Graph traversal (BFS/DFS)
//! - Vector similarity search (HNSW k-NN)

use criterion::{BenchmarkId, Criterion, Throughput, black_box, criterion_group, criterion_main};
use gallifreydb::{
    DistanceMetric, GallifreyDB, HnswConfig, NodeId, PropertyMapBuilder,
    query::{QueryBuilder, traverse_and_rank},
};

/// Create a test database with vector indexing enabled.
fn create_test_db() -> GallifreyDB {
    let db = GallifreyDB::new();
    let config = HnswConfig::new(384, DistanceMetric::Cosine);
    db.enable_vector_index("embedding", config)
        .expect("Failed to enable vector index");
    db
}

/// Create a social network graph with specified size.
/// Returns (node_ids, hub_id)
fn create_social_network(db: &GallifreyDB, num_nodes: usize) -> (Vec<NodeId>, NodeId) {
    let mut nodes = Vec::with_capacity(num_nodes);

    // Create a hub node
    let hub_embedding: Vec<f32> = (0..384).map(|i| (i as f32 / 384.0).sin()).collect();
    let hub = db
        .create_node(
            "Person",
            PropertyMapBuilder::new()
                .insert("name", "Hub")
                .insert("is_hub", true)
                .insert_vector("embedding", &hub_embedding)
                .build(),
        )
        .expect("Failed to create hub");

    // Create regular nodes with varying similarity to hub
    for i in 0..num_nodes {
        let similarity_factor = (i as f32 / num_nodes as f32) * 0.5 + 0.5; // 0.5 to 1.0
        let embedding: Vec<f32> = hub_embedding
            .iter()
            .map(|&v| v * similarity_factor)
            .collect();

        let node = db
            .create_node(
                "Person",
                PropertyMapBuilder::new()
                    .insert("name", format!("Person_{}", i))
                    .insert("id", i as i64)
                    .insert_vector("embedding", &embedding)
                    .build(),
            )
            .expect("Failed to create node");

        // Connect to hub
        db.create_edge(hub, node, "KNOWS", PropertyMapBuilder::new().build())
            .expect("Failed to create edge");

        // Create some inter-node connections (social graph structure)
        if i > 0 && i % 3 == 0 {
            db.create_edge(
                nodes[i - 1],
                node,
                "KNOWS",
                PropertyMapBuilder::new().build(),
            )
            .expect("Failed to create edge");
        }

        nodes.push(node);
    }

    (nodes, hub)
}

/// Benchmark traverse_and_rank at different scales.
fn bench_traverse_and_rank(c: &mut Criterion) {
    let mut group = c.benchmark_group("traverse_and_rank");

    // Test at different scales
    for size in [10, 50, 100, 500].iter() {
        group.throughput(Throughput::Elements(*size as u64));

        let db = create_test_db();
        let (_nodes, hub) = create_social_network(&db, *size);
        let query_embedding: Vec<f32> = (0..384).map(|i| (i as f32 / 384.0).cos()).collect();

        group.bench_with_input(BenchmarkId::new("small_k", size), size, |b, _| {
            b.iter(|| {
                let results = traverse_and_rank(
                    &db,
                    black_box(hub),
                    black_box("KNOWS"),
                    black_box(&query_embedding),
                    black_box(10), // k=10
                )
                .expect("traverse_and_rank failed");
                black_box(results);
            });
        });

        group.bench_with_input(BenchmarkId::new("large_k", size), size, |b, _| {
            b.iter(|| {
                let results = traverse_and_rank(
                    &db,
                    black_box(hub),
                    black_box("KNOWS"),
                    black_box(&query_embedding),
                    black_box(*size), // k = graph size
                )
                .expect("traverse_and_rank failed");
                black_box(results);
            });
        });
    }

    group.finish();
}

/// Benchmark pure vector search (baseline comparison).
fn bench_vector_search(c: &mut Criterion) {
    let mut group = c.benchmark_group("vector_search");

    for size in [10, 50, 100, 500].iter() {
        group.throughput(Throughput::Elements(*size as u64));

        let db = create_test_db();
        let (_nodes, _hub) = create_social_network(&db, *size);
        let query_embedding: Vec<f32> = (0..384).map(|i| (i as f32 / 384.0).cos()).collect();

        group.bench_with_input(BenchmarkId::new("k=10", size), size, |b, _| {
            b.iter(|| {
                let query = QueryBuilder::new()
                    .find_similar(black_box(&query_embedding), black_box(10))
                    .build();
                let results = db.execute_query(black_box(query)).expect("Query failed");
                black_box(results);
            });
        });
    }

    group.finish();
}

/// Benchmark pure graph traversal (baseline comparison).
fn bench_graph_traversal(c: &mut Criterion) {
    let mut group = c.benchmark_group("graph_traversal");

    for size in [10, 50, 100, 500].iter() {
        group.throughput(Throughput::Elements(*size as u64));

        let db = create_test_db();
        let (_nodes, hub) = create_social_network(&db, *size);

        group.bench_with_input(BenchmarkId::new("1-hop", size), size, |b, _| {
            b.iter(|| {
                let query = QueryBuilder::new()
                    .start(black_box(hub))
                    .traverse(black_box("KNOWS"))
                    .build();
                let results = db.execute_query(black_box(query)).expect("Query failed");
                black_box(results);
            });
        });
    }

    group.finish();
}

/// Benchmark query optimization overhead.
fn bench_query_optimization(c: &mut Criterion) {
    let mut group = c.benchmark_group("query_optimization");

    let db = create_test_db();
    let (_nodes, hub) = create_social_network(&db, 100);
    let query_embedding: Vec<f32> = (0..384).map(|i| (i as f32 / 384.0).cos()).collect();

    group.bench_function("build_hybrid_query", |b| {
        b.iter(|| {
            let query = QueryBuilder::new()
                .start(black_box(hub))
                .traverse(black_box("KNOWS"))
                .rank_by_similarity(black_box(&query_embedding), black_box(10))
                .build();

            // Query building includes validation
            black_box(query);
        });
    });

    group.finish();
}

/// Benchmark multi-hop traversal queries.
fn bench_multi_hop_traversal(c: &mut Criterion) {
    let mut group = c.benchmark_group("multi_hop_traversal");

    let db = create_test_db();
    let (nodes, _hub) = create_social_network(&db, 100);

    // Create a chain for multi-hop testing
    for i in 0..99 {
        db.create_edge(
            nodes[i],
            nodes[i + 1],
            "CHAIN",
            PropertyMapBuilder::new().build(),
        )
        .expect("Failed to create chain edge");
    }

    for hops in [2, 3, 5].iter() {
        group.bench_with_input(BenchmarkId::new("hops", hops), hops, |b, &depth| {
            b.iter(|| {
                let query = QueryBuilder::new()
                    .start(black_box(nodes[0]))
                    .traverse_n(black_box("CHAIN"), black_box(depth))
                    .build();
                let results = db.execute_query(black_box(query)).expect("Query failed");
                black_box(results);
            });
        });
    }

    group.finish();
}

/// Benchmark hybrid vs separate operations.
fn bench_hybrid_vs_separate(c: &mut Criterion) {
    let mut group = c.benchmark_group("hybrid_vs_separate");

    let db = create_test_db();
    let (_nodes, hub) = create_social_network(&db, 100);
    let query_embedding: Vec<f32> = (0..384).map(|i| (i as f32 / 384.0).cos()).collect();

    // Hybrid query (optimized with traverse_and_rank)
    group.bench_function("hybrid_traverse_and_rank", |b| {
        b.iter(|| {
            let results = traverse_and_rank(
                &db,
                black_box(hub),
                black_box("KNOWS"),
                black_box(&query_embedding),
                black_box(10),
            )
            .expect("traverse_and_rank failed");
            black_box(results);
        });
    });

    // Query builder approach
    group.bench_function("query_builder_hybrid", |b| {
        b.iter(|| {
            let query = QueryBuilder::new()
                .start(black_box(hub))
                .traverse(black_box("KNOWS"))
                .rank_by_similarity(black_box(&query_embedding), black_box(10))
                .build();
            let results = db.execute_query(black_box(query)).expect("Query failed");
            black_box(results);
        });
    });

    group.finish();
}

/// Benchmark full hybrid query execution (end-to-end).
fn bench_full_hybrid_query(c: &mut Criterion) {
    let mut group = c.benchmark_group("full_hybrid_query");

    let db = create_test_db();
    let (_nodes, hub) = create_social_network(&db, 100);
    let query_embedding: Vec<f32> = (0..384).map(|i| (i as f32 / 384.0).cos()).collect();

    group.bench_function("traverse_and_rank_k10", |b| {
        b.iter(|| {
            let results = traverse_and_rank(
                &db,
                black_box(hub),
                black_box("KNOWS"),
                black_box(&query_embedding),
                black_box(10),
            )
            .expect("traverse_and_rank failed");
            black_box(results);
        });
    });

    group.bench_function("traverse_and_rank_k50", |b| {
        b.iter(|| {
            let results = traverse_and_rank(
                &db,
                black_box(hub),
                black_box("KNOWS"),
                black_box(&query_embedding),
                black_box(50),
            )
            .expect("traverse_and_rank failed");
            black_box(results);
        });
    });

    group.finish();
}

criterion_group!(
    benches,
    bench_traverse_and_rank,
    bench_vector_search,
    bench_graph_traversal,
    bench_query_optimization,
    bench_multi_hop_traversal,
    bench_hybrid_vs_separate,
    bench_full_hybrid_query,
);
criterion_main!(benches);
