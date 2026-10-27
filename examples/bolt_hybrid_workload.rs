//! Deterministic hybrid (graph traversal + vector rank) workload for
//! instruction-count profiling.
//!
//! Drives `AletheiaDB::traverse_and_rank` (the documented hybrid entry point)
//! over a hub-and-spoke document graph with HNSW-indexed embeddings.
//!
//! ```text
//! cargo build --profile bench --example bolt_hybrid_workload
//! valgrind --tool=callgrind --callgrind-out-file=/tmp/cg.out \
//!     target/release/examples/bolt_hybrid_workload
//! ```
//!
//! Env vars: `BOLT_HUBS` (50), `BOLT_FANOUT` (100), `BOLT_DIMENSIONS` (64),
//! `BOLT_K` (10), `BOLT_QUERY_ITERS` (200).

use aletheiadb::index::vector::{DistanceMetric, HnswConfig};
use aletheiadb::prelude::*;
use rand::rngs::SmallRng;
use rand::{Rng, SeedableRng};

fn env_usize(key: &str, default: usize) -> usize {
    std::env::var(key)
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(default)
}

fn random_vector(rng: &mut SmallRng, dimensions: usize) -> Vec<f32> {
    (0..dimensions)
        .map(|_| rng.random_range(-1.0f32..1.0))
        .collect()
}

fn main() {
    let hubs = env_usize("BOLT_HUBS", 50);
    let fanout = env_usize("BOLT_FANOUT", 100);
    let dimensions = env_usize("BOLT_DIMENSIONS", 64);
    let k = env_usize("BOLT_K", 10);
    let query_iters = env_usize("BOLT_QUERY_ITERS", 200);

    let db = AletheiaDB::new().expect("create ephemeral db");
    db.vector_index("embedding")
        .hnsw(HnswConfig::new(dimensions, DistanceMetric::Cosine))
        .enable()
        .expect("enable vector index");

    let mut rng = SmallRng::seed_from_u64(0xB017_5EED);
    let mut hub_ids = Vec::with_capacity(hubs);
    for h in 0..hubs {
        let hub = db
            .create_node(
                "Hub",
                PropertyMapBuilder::new()
                    .insert("name", format!("hub{h}"))
                    .build(),
            )
            .expect("hub");
        for i in 0..fanout {
            let emb = random_vector(&mut rng, dimensions);
            let doc = db
                .create_node(
                    "Document",
                    PropertyMapBuilder::new()
                        .insert("title", format!("doc{h}_{i}"))
                        .insert_vector("embedding", &emb)
                        .build(),
                )
                .expect("doc");
            db.create_edge(hub, doc, "CONTAINS", PropertyMapBuilder::new().build())
                .expect("edge");
        }
        hub_ids.push(hub);
    }

    let mut sink: u64 = 0;
    for i in 0..query_iters {
        let q = random_vector(&mut rng, dimensions);
        if let Ok(res) = db.traverse_and_rank(hub_ids[i % hubs], "CONTAINS", &q, k) {
            for row in res {
                if let Ok(r) = row {
                    sink = sink
                        .wrapping_add(1)
                        .wrapping_add(r.score.unwrap_or(0.0).to_bits() as u64);
                }
            }
        }
    }
    println!("sink={sink} hubs={hubs} fanout={fanout} dims={dimensions} k={k} iters={query_iters}");
}
