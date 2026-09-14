//! Deterministic vector-search workload for instruction-count profiling.
//!
//! Exercises the public `AletheiaDB` vector-index API the way the MCP
//! `find_similar` tool / SUPERRAG vector search is actually used: create a
//! batch of nodes each carrying a dense embedding (which the write path
//! automatically indexes via the enabled HNSW index — see
//! `AletheiaDB::vector_index`/`enable_vector_index` in
//! `src/db/vector.rs`), then issue repeated k-NN `find_similar` queries
//! against it, matching `docs/guides/vector-search-integration.md`'s
//! documented quick-start shape.
//!
//! No existing benchmark profiles this path through the public entry point
//! with instruction-level attribution: `benches/hnsw_index.rs` is a
//! criterion suite that calls `HnswIndex` directly (bypassing
//! `AletheiaDB`'s write-path indexing hook), and criterion's
//! iteration/warmup loop is prohibitively slow stacked under valgrind's own
//! 20-50x slowdown anyway. This is a plain binary, run once, deterministic.
//!
//! Vector generation uses a fixed-seed `SmallRng` (not `thread_rng`) so the
//! embeddings — and therefore the HNSW graph shape and every query's
//! candidate set — are bit-identical run to run, keeping instruction counts
//! reproducible.
//!
//! ```text
//! cargo build --profile bench --example bolt_vector_workload
//! valgrind --tool=callgrind --callgrind-out-file=/tmp/callgrind.out \
//!     target/release/examples/bolt_vector_workload
//! callgrind_annotate /tmp/callgrind.out | less
//!
//! valgrind --tool=dhat --dhat-out-file=/tmp/dhat.out \
//!     target/release/examples/bolt_vector_workload
//! ```
//!
//! Env vars: `BOLT_NODES` (default 2000), `BOLT_DIMENSIONS` (default 128),
//! `BOLT_K` (default 10), `BOLT_QUERY_ITERS` (default 500).

use aletheiadb::SimilarityQuery;
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
        .map(|_| rng.gen_range(-1.0f32..1.0))
        .collect()
}

fn main() {
    let node_count = env_usize("BOLT_NODES", 2_000);
    let dimensions = env_usize("BOLT_DIMENSIONS", 128);
    let k = env_usize("BOLT_K", 10);
    let query_iters = env_usize("BOLT_QUERY_ITERS", 500);

    let db = AletheiaDB::new().expect("create ephemeral db");

    // A realistically-configured deployment enables the vector index before
    // writing, so every `create_node` below indexes its embedding through
    // the normal write-path hook rather than a bulk `rebuild_vector_index`
    // pass at the end.
    db.vector_index("embedding")
        .hnsw(HnswConfig::new(dimensions, DistanceMetric::Cosine))
        .enable()
        .expect("enable vector index");

    let mut rng = SmallRng::seed_from_u64(0xB017_5EED);

    // --- Write phase: a document-corpus-shaped dataset, one embedding per
    // node, indexed automatically as each node is created. ---
    let mut node_ids = Vec::with_capacity(node_count);
    for i in 0..node_count {
        let embedding = random_vector(&mut rng, dimensions);
        let props = PropertyMapBuilder::new()
            .insert("title", format!("Document{i}"))
            .insert("category", format!("cat_{}", i % 10))
            .insert_vector("embedding", &embedding)
            .build();
        node_ids.push(db.create_node("Document", props).expect("create_node"));
    }

    // --- Read phase: repeated k-NN similarity search, the pattern the MCP
    // `find_similar` tool issues on every call. ---
    let mut sink: u64 = 0;
    for i in 0..query_iters {
        let query_node = node_ids[i % node_count];
        if let Ok(results) = db.similarity_search(SimilarityQuery::from_node(query_node).k(k)) {
            sink = sink.wrapping_add(results.len() as u64);
            for (_id, score) in &results {
                sink = sink.wrapping_add(score.to_bits() as u64);
            }
        }
    }

    println!(
        "sink={sink} nodes={node_count} dimensions={dimensions} k={k} query_iters={query_iters}"
    );
}
