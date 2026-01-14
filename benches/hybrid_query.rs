//! Benchmarks for hybrid query operations (Phase 4).
//!
//! Measures performance of:
//! - traverse_and_rank at different scales and topologies
//! - Temporal vector search (find_similar_as_of)
//! - Full hybrid query composition patterns
//! - Query optimization overhead (cache warmup, algorithmic optimizations)
//! - Comparison baselines (hybrid vs separate operations)

// Suppressing warnings for incremental development -
// functions and imports will be used in subsequent tasks (Task 3+)
#![allow(unused_imports)]
#![allow(dead_code)]

mod common;

use criterion::{BenchmarkId, Criterion, Throughput, black_box, criterion_group, criterion_main};
use gallifreydb::api::transaction::WriteOps;
use gallifreydb::core::id::NodeId;
use gallifreydb::core::property::PropertyMapBuilder;
use gallifreydb::core::vector::cosine_similarity;
use gallifreydb::db::GallifreyDB;
use gallifreydb::index::vector::temporal::{
    RetentionPolicy, SnapshotStrategy, TemporalVectorConfig,
};
use gallifreydb::index::vector::{DistanceMetric, HnswConfig};
use gallifreydb::query::hybrid::{find_similar_as_of, traverse_and_rank};
use std::cmp::Ordering;

// ============================================================================
// Data Generation Helpers
// ============================================================================

/// Generate deterministic vector for benchmarking.
///
/// Uses simple mathematical formula to create reproducible vectors
/// without allocation noise during benchmark execution.
fn gen_vector(dim: usize, seed: usize) -> Vec<f32> {
    (0..dim)
        .map(|i| ((i + seed) as f32 / dim as f32) * 2.0 - 1.0)
        .collect()
}

/// Generate clustered vectors (realistic embedding distribution).
///
/// Vectors in the same cluster have high cosine similarity.
/// This simulates real embedding distributions where related concepts
/// cluster together in embedding space.
fn gen_clustered_vector(dim: usize, cluster_id: usize, variance: f32) -> Vec<f32> {
    let base: Vec<f32> = gen_vector(dim, cluster_id * 1000);
    base.iter()
        .enumerate()
        .map(|(i, &v)| v + (i as f32 * variance).sin() * 0.1)
        .collect()
}

// ============================================================================
// Graph Topology Builders
// ============================================================================

/// Build uniform graph: each node has ~fan_out edges.
///
/// This creates a structured graph where all nodes have similar
/// connectivity, representing knowledge graphs or databases with
/// predictable relationship patterns.
fn build_uniform_graph(node_count: usize, fan_out: usize, dim: usize) -> GallifreyDB {
    let db = GallifreyDB::new();
    let config = HnswConfig::new(dim, DistanceMetric::Cosine);
    db.enable_vector_index("embedding", config).unwrap();

    // Create nodes with embeddings
    for i in 0..node_count {
        let vector = gen_vector(dim, i);
        let _ = db.create_node(
            "Person",
            PropertyMapBuilder::new()
                .insert("id", i as i64)
                .insert_vector("embedding", &vector)
                .build(),
        );
    }

    // Create edges (deterministic fan-out)
    for i in 0..node_count {
        let source = NodeId::new(i as u64).unwrap();
        for j in 0..fan_out {
            let target_idx = (i + j + 1) % node_count;
            let target = NodeId::new(target_idx as u64).unwrap();
            let _ = db.create_edge(source, target, "KNOWS", PropertyMapBuilder::new().build());
        }
    }

    db
}

/// Build power-law graph: hubs, medium, and sparse nodes.
///
/// Simulates realistic social networks with power-law degree distribution:
/// - 5% hubs (100 edges each)
/// - 20% medium nodes (30 edges each)
/// - 75% regular nodes (5 edges each)
///
/// Uses clustered embeddings to simulate semantic similarity within communities.
fn build_power_law_graph(node_count: usize, dim: usize) -> GallifreyDB {
    let db = GallifreyDB::new();
    let config = HnswConfig::new(dim, DistanceMetric::Cosine);
    db.enable_vector_index("embedding", config).unwrap();

    // Create nodes with clustered embeddings (10 clusters)
    let num_clusters = 10;
    for i in 0..node_count {
        let cluster_id = i % num_clusters;
        let vector = gen_clustered_vector(dim, cluster_id, 0.1);
        let _ = db.create_node(
            "Person",
            PropertyMapBuilder::new()
                .insert("id", i as i64)
                .insert_vector("embedding", &vector)
                .build(),
        );
    }

    // Create edges with power-law distribution
    for i in 0..node_count {
        let source = NodeId::new(i as u64).unwrap();

        // Determine degree based on power-law distribution
        let degree = if i < node_count / 20 {
            100 // 5% hubs
        } else if i < node_count / 4 {
            30 // 20% medium nodes
        } else {
            5 // 75% regular nodes
        };

        // Create edges to random targets
        for j in 0..degree {
            let target_idx = (i + j * 17 + 1) % node_count; // Pseudo-random but deterministic
            let target = NodeId::new(target_idx as u64).unwrap();
            let _ = db.create_edge(source, target, "KNOWS", PropertyMapBuilder::new().build());
        }
    }

    db
}

/// Build sparse graph: most nodes have 2-5 edges.
///
/// This represents minimalist graphs where connections are selective,
/// such as expert networks or curated knowledge bases.
fn build_sparse_graph(node_count: usize, dim: usize) -> GallifreyDB {
    let db = GallifreyDB::new();
    let config = HnswConfig::new(dim, DistanceMetric::Cosine);
    db.enable_vector_index("embedding", config).unwrap();

    // Create nodes
    for i in 0..node_count {
        let vector = gen_vector(dim, i);
        let _ = db.create_node(
            "Person",
            PropertyMapBuilder::new()
                .insert("id", i as i64)
                .insert_vector("embedding", &vector)
                .build(),
        );
    }

    // Create sparse edges (2-5 per node)
    for i in 0..node_count {
        let source = NodeId::new(i as u64).unwrap();
        let edge_count = 2 + (i % 4); // 2-5 edges

        for j in 0..edge_count {
            let target_idx = (i + j + 1) % node_count;
            let target = NodeId::new(target_idx as u64).unwrap();
            let _ = db.create_edge(source, target, "KNOWS", PropertyMapBuilder::new().build());
        }
    }

    db
}

/// Create graph with temporal snapshots.
///
/// Returns database instance and vector of timestamps for each snapshot.
/// Snapshots are created by updating node embeddings over time.
fn build_temporal_graph(
    node_count: usize,
    snapshot_count: usize,
    dim: usize,
) -> (GallifreyDB, Vec<i64>) {
    let db = GallifreyDB::new();
    let hnsw_config = HnswConfig::new(dim, DistanceMetric::Cosine);
    let temporal_config = TemporalVectorConfig {
        snapshot_strategy: SnapshotStrategy::TransactionInterval(1),
        retention_policy: RetentionPolicy::KeepN(snapshot_count * 2),
        max_snapshots: snapshot_count * 2,
        full_snapshot_interval: 10,
        hnsw_config,
    };
    db.enable_temporal_vector_index("embedding", temporal_config)
        .unwrap();

    let mut timestamps = Vec::new();

    // Create snapshots
    for snapshot_idx in 0..snapshot_count {
        let timestamp = snapshot_idx as i64 * 1000;
        timestamps.push(timestamp);

        // Create/update nodes with evolving embeddings
        for i in 0..node_count {
            let vector = gen_vector(dim, snapshot_idx * node_count + i);

            if snapshot_idx == 0 {
                // Create node
                let _ = db.create_node(
                    "Document",
                    PropertyMapBuilder::new()
                        .insert("id", i as i64)
                        .insert_vector("embedding", &vector)
                        .build(),
                );
            } else {
                // Update node (evolving embedding)
                let node_id = NodeId::new(i as u64).unwrap();
                db.write(|tx| {
                    tx.update_node(
                        node_id,
                        PropertyMapBuilder::new()
                            .insert_vector("embedding", &vector)
                            .build(),
                    )
                })
                .unwrap();
            }
        }
    }

    (db, timestamps)
}

/// Create graph with temporal snapshots AND edges.
///
/// Extended version of build_temporal_graph that also creates edges
/// for testing chained hybrid operations (traverse -> rank -> temporal lookup).
fn build_temporal_graph_with_edges(
    node_count: usize,
    snapshot_count: usize,
    fan_out: usize,
    dim: usize,
) -> (GallifreyDB, Vec<i64>) {
    let db = GallifreyDB::new();
    let hnsw_config = HnswConfig::new(dim, DistanceMetric::Cosine);
    let temporal_config = TemporalVectorConfig {
        snapshot_strategy: SnapshotStrategy::TransactionInterval(1),
        retention_policy: RetentionPolicy::KeepN(snapshot_count * 2),
        max_snapshots: snapshot_count * 2,
        full_snapshot_interval: 10,
        hnsw_config,
    };
    db.enable_temporal_vector_index("embedding", temporal_config)
        .unwrap();

    let mut timestamps = Vec::new();

    // Create snapshots
    for snapshot_idx in 0..snapshot_count {
        let timestamp = snapshot_idx as i64 * 1000;
        timestamps.push(timestamp);

        // Create/update nodes with evolving embeddings
        for i in 0..node_count {
            let vector = gen_vector(dim, snapshot_idx * node_count + i);

            if snapshot_idx == 0 {
                // Create node
                let _ = db.create_node(
                    "Document",
                    PropertyMapBuilder::new()
                        .insert("id", i as i64)
                        .insert_vector("embedding", &vector)
                        .build(),
                );
            } else {
                // Update node (evolving embedding)
                let node_id = NodeId::new(i as u64).unwrap();
                db.write(|tx| {
                    tx.update_node(
                        node_id,
                        PropertyMapBuilder::new()
                            .insert_vector("embedding", &vector)
                            .build(),
                    )
                })
                .unwrap();
            }
        }
    }

    // Create edges (deterministic fan-out) after all nodes exist
    for i in 0..node_count {
        let source = NodeId::new(i as u64).unwrap();
        for j in 0..fan_out {
            let target_idx = (i + j + 1) % node_count;
            let target = NodeId::new(target_idx as u64).unwrap();
            let _ = db.create_edge(
                source,
                target,
                "RELATED_TO",
                PropertyMapBuilder::new().build(),
            );
        }
    }

    (db, timestamps)
}

// ============================================================================
// Benchmark: traverse_and_rank Basic (Scale × Topology)
// ============================================================================

/// Type alias for graph builder functions to reduce complexity.
type GraphBuilder = Box<dyn Fn(usize, usize) -> GallifreyDB>;

/// Benchmark traverse_and_rank across different scales and topologies.
///
/// Tests the core hybrid query operation (graph traversal + vector ranking)
/// with 9 combinations: 3 scales (100, 1K, 10K) × 3 topologies (uniform, power-law, sparse).
///
/// This measures the baseline performance of hybrid queries across realistic
/// graph structures and sizes.
fn bench_traverse_and_rank_basic(c: &mut Criterion) {
    let mut group = c.benchmark_group("traverse_and_rank/basic");

    // Test matrix: 3 scales × 3 topologies = 9 combinations
    let scales = vec![("100", 100), ("1K", 1000), ("10K", 10000)];

    let topologies: Vec<(&str, GraphBuilder)> = vec![
        ("uniform", Box::new(|n, d| build_uniform_graph(n, 20, d))),
        ("power_law", Box::new(build_power_law_graph)),
        ("sparse", Box::new(build_sparse_graph)),
    ];

    for (scale_name, node_count) in scales {
        for (topo_name, builder) in &topologies {
            let db = builder(node_count, 384);
            let source = NodeId::new(0).unwrap();
            let query_vec = gen_vector(384, 42);

            group.throughput(Throughput::Elements(node_count as u64));
            group.bench_function(
                BenchmarkId::new(format!("{}/{}", scale_name, topo_name), node_count),
                |b| {
                    b.iter(|| {
                        let results = traverse_and_rank(
                            black_box(&db),
                            black_box(source),
                            black_box("KNOWS"),
                            black_box(&query_vec),
                            black_box(10), // k
                        );
                        black_box(results)
                    });
                },
            );
        }
    }

    group.finish();
}

// ============================================================================
// Benchmark: traverse_and_rank K-Value Variations
// ============================================================================

/// Benchmark traverse_and_rank with different k values.
///
/// Tests how the number of results requested (k) affects performance.
/// Uses a fixed 10K node power-law graph (realistic social network topology)
/// with dimension 384 (standard embedding size).
///
/// K values tested: 1, 5, 10, 25, 50, 100
/// This helps identify:
/// - Sorting overhead at different result sizes
/// - Memory allocation patterns for result vectors
/// - HNSW search efficiency at various k values
fn bench_traverse_and_rank_k_values(c: &mut Criterion) {
    let mut group = c.benchmark_group("traverse_and_rank/k_values");

    // Fixed setup: 10K nodes, power-law topology
    let db = build_power_law_graph(10000, 384);
    let source = NodeId::new(0).unwrap();
    let query_vec = gen_vector(384, 42);

    // Test different k values
    let k_values = vec![1, 5, 10, 25, 50, 100];

    for k in k_values {
        group.bench_function(BenchmarkId::new("k", k), |b| {
            b.iter(|| {
                let results = traverse_and_rank(
                    black_box(&db),
                    black_box(source),
                    black_box("KNOWS"),
                    black_box(&query_vec),
                    black_box(k),
                );
                black_box(results)
            });
        });
    }

    group.finish();
}

// ============================================================================
// Benchmark: traverse_and_rank Dimension Variations
// ============================================================================

/// Benchmark traverse_and_rank with different vector dimensions.
///
/// Tests dim=[384, 768, 1536] to show similarity computation overhead.
/// Expected: Linear scaling with dimension size.
fn bench_traverse_and_rank_dimensions(c: &mut Criterion) {
    let mut group = c.benchmark_group("traverse_and_rank/dimensions");
    group.throughput(Throughput::Elements(1));

    for dim in [384, 768, 1536] {
        let db = build_uniform_graph(1000, 20, dim);
        let start = NodeId::new(500).unwrap();
        let query = gen_vector(dim, 0);

        group.bench_with_input(BenchmarkId::new("dim", dim), &dim, |b, _| {
            b.iter(|| {
                traverse_and_rank(
                    black_box(&db),
                    black_box(start),
                    black_box("KNOWS"),
                    black_box(&query),
                    black_box(10),
                )
            });
        });
    }

    group.finish();
}

// ============================================================================
// Section 2: Temporal Vector Search
// ============================================================================

/// Benchmark find_similar_as_of with multi-depth history.
///
/// Tests queries at different temporal distances (recent, middle, deep)
/// to measure anchor+delta reconstruction overhead.
fn bench_find_similar_as_of(c: &mut Criterion) {
    let mut group = c.benchmark_group("find_similar_as_of");

    for snapshot_count in [10, 50, 100] {
        for (query_depth_name, query_depth_pct) in [
            ("recent", 0.1), // Last 10% of snapshots
            ("middle", 0.5), // 50% back
            ("deep", 0.9),   // 90% back (tests reconstruction depth)
        ] {
            group.bench_with_input(
                BenchmarkId::new(
                    format!("{}snapshots_{}", snapshot_count, query_depth_name),
                    snapshot_count,
                ),
                &(snapshot_count, query_depth_pct),
                |b, &(snap_count, depth_pct)| {
                    let (db, timestamps) = build_temporal_graph(1000, snap_count, 384);
                    let query_timestamp = timestamps[(snap_count as f32 * depth_pct) as usize];
                    let query = gen_vector(384, 0);

                    b.iter(|| {
                        find_similar_as_of(
                            black_box(&db),
                            black_box(&query),
                            black_box(10),
                            black_box(query_timestamp),
                        )
                    });
                },
            );
        }
    }

    group.finish();
}

// ============================================================================
// Benchmark: Temporal vs Current Query Comparison
// ============================================================================

/// Benchmark temporal query vs current-state query overhead.
///
/// Compares find_similar_as_of (temporal) vs find_similar_by_embedding (current).
/// Shows cost of temporal reconstruction.
///
/// This benchmark helps quantify the overhead of temporal queries when querying
/// at the "current" timestamp. The difference reveals:
/// - Snapshot lookup overhead
/// - Temporal index navigation cost
/// - Memory overhead from temporal data structures
fn bench_temporal_vs_current(c: &mut Criterion) {
    let mut group = c.benchmark_group("temporal_vs_current");
    group.throughput(Throughput::Elements(1));

    let (temporal_db, timestamps) = build_temporal_graph(1000, 50, 384);
    let query = gen_vector(384, 0);
    let current_timestamp = *timestamps.last().unwrap();

    // Also build a non-temporal graph for true current-state comparison
    let current_db = build_uniform_graph(1000, 20, 384);

    // Temporal query (with reconstruction overhead)
    group.bench_function("temporal_query", |b| {
        b.iter(|| {
            find_similar_as_of(
                black_box(&temporal_db),
                black_box(&query),
                black_box(10),
                black_box(current_timestamp),
            )
        });
    });

    // Current-state query on temporal DB (uses current storage path)
    group.bench_function("current_on_temporal_db", |b| {
        b.iter(|| temporal_db.find_similar_by_embedding(black_box(&query), black_box(10)));
    });

    // Current-state query on non-temporal DB (baseline)
    group.bench_function("current_query", |b| {
        b.iter(|| current_db.find_similar_by_embedding(black_box(&query), black_box(10)));
    });

    group.finish();
}

// ============================================================================
// Section 3: Full Hybrid Queries (Composition)
// ============================================================================

/// Benchmark multi-step hybrid query compositions.
///
/// Tests realistic query patterns that combine multiple operations:
/// - Traverse -> Rank -> Filter -> Temporal lookup
/// - Multi-hop ranked traversal
fn bench_chained_hybrid_operations(c: &mut Criterion) {
    let mut group = c.benchmark_group("chained_operations");

    // Pattern: traverse -> rank -> filter -> temporal lookup
    group.bench_function("traverse_rank_filter_temporal", |b| {
        let (db, _timestamps) = build_temporal_graph_with_edges(1000, 20, 20, 384);
        let start = NodeId::new(100).unwrap();
        let query = gen_vector(384, 0);

        b.iter(|| {
            // 1. traverse_and_rank to get top-k neighbors
            let ranked = traverse_and_rank(&db, start, "RELATED_TO", &query, 20).unwrap();

            // 2. Filter by similarity threshold
            let filtered: Vec<_> = ranked.into_iter().filter(|(_, sim)| *sim > 0.8).collect();

            // 3. For each result, query historical state
            for (_node_id, _) in filtered {
                let _ = find_similar_as_of(&db, &query, 5, 1000);
            }
        });
    });

    // Pattern: multi-hop traversal with ranking at each step
    group.bench_function("multi_hop_ranked_traversal", |b| {
        let db = build_uniform_graph(1000, 20, 384);
        let start = NodeId::new(100).unwrap();
        let query = gen_vector(384, 0);

        b.iter(|| {
            // Hop 1: Start from node A
            let hop1 = traverse_and_rank(&db, start, "KNOWS", &query, 10).unwrap();

            // Hop 2: From best match B, traverse again
            if let Some((best_id, _)) = hop1.first() {
                let _ = traverse_and_rank(&db, *best_id, "KNOWS", &query, 10);
            }
        });
    });

    group.finish();
}

// ============================================================================
// Section 4: Query Optimization Overhead
// ============================================================================

/// Benchmark cold vs warm cache performance.
///
/// Measures the impact of cache hits on query performance.
/// Cold: fresh DB each iteration. Warm: repeated queries on same data.
fn bench_cache_warmup_effects(c: &mut Criterion) {
    let mut group = c.benchmark_group("cache_warmup");

    // Cold cache: first query after DB creation
    group.bench_function("cold_cache_traverse_rank", |b| {
        b.iter_batched(
            || build_uniform_graph(1000, 20, 384), // Fresh DB each iteration
            |db| {
                let start = NodeId::new(100).unwrap();
                let query = gen_vector(384, 0);
                traverse_and_rank(&db, start, "KNOWS", &query, 10)
            },
            criterion::BatchSize::SmallInput,
        );
    });

    // Warm cache: repeated queries on same data
    group.bench_function("warm_cache_traverse_rank", |b| {
        let db = build_uniform_graph(1000, 20, 384);
        let start = NodeId::new(100).unwrap();
        let query = gen_vector(384, 0);

        b.iter(|| traverse_and_rank(&db, start, "KNOWS", &query, 10));
    });

    group.finish();
}

criterion_group!(
    name = benches;
    config = common::configure_criterion();
    targets = bench_traverse_and_rank_basic, bench_traverse_and_rank_k_values, bench_traverse_and_rank_dimensions
);

criterion_group!(
    name = temporal_operations;
    config = common::configure_criterion();
    targets = bench_find_similar_as_of,
        bench_temporal_vs_current,
);

criterion_group!(
    name = composition;
    config = common::configure_criterion();
    targets = bench_chained_hybrid_operations,
);

criterion_group!(
    name = optimization_overhead;
    config = common::configure_criterion();
    targets = bench_cache_warmup_effects,
);

// ============================================================================
// Section 5: Comparison Baselines (Hybrid vs Separate)
// ============================================================================

/// Compare hybrid API vs running operations sequentially.
///
/// Measures whether the integrated hybrid API provides performance
/// benefits over manual composition of separate operations.
fn bench_hybrid_vs_sequential(c: &mut Criterion) {
    let mut group = c.benchmark_group("hybrid_vs_sequential");

    let db = build_uniform_graph(1000, 20, 384);
    let start = NodeId::new(100).unwrap();
    let query = gen_vector(384, 0);

    // Hybrid approach (integrated)
    group.bench_function("hybrid_traverse_and_rank", |b| {
        b.iter(|| {
            traverse_and_rank(
                black_box(&db),
                black_box(start),
                black_box("KNOWS"),
                black_box(&query),
                black_box(10),
            )
        });
    });

    // Sequential approach (separate operations)
    group.bench_function("sequential_traverse_then_rank", |b| {
        b.iter(|| {
            // Step 1: Get all neighbors via graph traversal
            let edge_ids = db.get_outgoing_edges_with_label(start, "KNOWS");
            let neighbors: Vec<NodeId> = edge_ids
                .iter()
                .filter_map(|&eid| db.get_edge(eid).ok().map(|e| e.target))
                .collect();

            // Step 2: Load embeddings and compute similarities
            let mut scored: Vec<(NodeId, f32)> = neighbors
                .iter()
                .filter_map(|&nid| {
                    let node = db.get_node(nid).ok()?;
                    let emb = node.get_property("embedding")?.as_vector()?;
                    let sim = cosine_similarity(&query, emb).ok()?;
                    Some((nid, sim))
                })
                .collect();

            // Step 3: Sort and take top-k
            scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(Ordering::Equal));
            scored.truncate(10);
            scored
        });
    });

    group.finish();
}

/// Compare hybrid vs naive "load everything into memory" approach.
///
/// Hybrid uses streaming with min-heap (O(N log k)).
/// Naive loads all neighbors, computes all similarities, sorts all (O(N log N)).
fn bench_hybrid_vs_naive_composition(c: &mut Criterion) {
    let mut group = c.benchmark_group("hybrid_vs_naive");

    let db = build_uniform_graph(1000, 20, 384);
    let start = NodeId::new(100).unwrap();
    let query = gen_vector(384, 0);

    // Hybrid: streaming approach with min-heap
    group.bench_function("hybrid_streaming", |b| {
        b.iter(|| traverse_and_rank(&db, start, "KNOWS", &query, 10));
    });

    // Naive: load all neighbors + all embeddings, then rank
    group.bench_function("naive_load_all_rank", |b| {
        b.iter(|| {
            let edge_ids = db.get_outgoing_edges_with_label(start, "KNOWS");

            // Load ALL neighbors into Vec
            let mut all_scored: Vec<(NodeId, f32)> = Vec::new();

            for &eid in &edge_ids {
                let edge = db.get_edge(eid).unwrap();
                if let Ok(node) = db.get_node(edge.target)
                    && let Some(emb) = node.get_property("embedding").and_then(|p| p.as_vector())
                    && let Ok(sim) = cosine_similarity(&query, emb)
                {
                    all_scored.push((edge.target, sim));
                }
            }

            // Sort entire result set (no heap optimization)
            all_scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(Ordering::Equal));
            all_scored.truncate(10);
            all_scored
        });
    });

    group.finish();
}

criterion_group!(
    name = comparison_baselines;
    config = common::configure_criterion();
    targets = bench_hybrid_vs_sequential,
        bench_hybrid_vs_naive_composition,
);

criterion_main!(
    benches,
    temporal_operations,
    composition,
    optimization_overhead,
    comparison_baselines
);
