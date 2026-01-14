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
use gallifreydb::core::id::NodeId;
use gallifreydb::core::property::PropertyMapBuilder;
use gallifreydb::core::vector::cosine_similarity;
use gallifreydb::db::GallifreyDB;
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

// Placeholder benchmark - will be replaced with actual benchmarks in subsequent tasks
fn bench_placeholder(_c: &mut Criterion) {
    // This is a placeholder to make the benchmark file compile.
    // Actual benchmarks will be added in subsequent tasks.
}

criterion_group!(
    name = benches;
    config = common::configure_criterion();
    targets = bench_placeholder
);

criterion_main!(benches);
