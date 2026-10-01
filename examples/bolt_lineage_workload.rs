//! Deterministic derivation-lineage workload for instruction-count profiling
//! (Issue #3371).
//!
//! Exercises `AletheiaDB::create_node_with_lineage`/`upstream_lineage`/
//! `downstream_lineage` -- the fact-to-fact derivation closure API
//! (`src/core/lineage.rs`'s `LineageStore`, `src/db/lineage.rs`'s
//! write/resolve glue) -- through the public API only. No existing
//! `examples/bolt_*.rs` harness touches this module (`benches/derivation_lineage.rs`
//! is criterion-only, and criterion wall-clock timing is inadmissible under
//! this repo's Bolt evidence rules on its own).
//!
//! Builds a citation-style DAG in `BOLT_LEVELS` waves of `BOLT_PER_LEVEL`
//! facts each (matching `benches/derivation_lineage.rs`'s own "5 waves x
//! 2000 = 10k facts, depth <= 5" blast-radius sizing): wave 0 derives from a
//! single root, wave `k` derives round-robin from wave `k-1`, so the root's
//! downstream closure spans the whole graph. Then runs the two realistic
//! access patterns the module's docs describe:
//!
//! - **Blast radius**: repeated `downstream_lineage` calls from the root and
//!   from a mid-graph node -- "an input was found wrong / retracted, what
//!   does it affect?" -- each walking a large fraction of the 10K-fact graph,
//!   so this dominates the profile's lineage-specific cost.
//! - **Citation lookup**: many single-hop `upstream_lineage` calls from leaf
//!   facts -- "what was this claim computed from?" -- the cheap, frequent
//!   case CLAUDE.md's `lineage_upstream` MCP tool serves.
//!
//! This is a plain binary, not a criterion benchmark: criterion's
//! iteration/warmup loop is prohibitively slow stacked under valgrind's own
//! 20-50x slowdown. Running one fixed, deterministic pass keeps instruction
//! counts reproducible run to run.
//!
//! ```text
//! cargo build --profile bench --example bolt_lineage_workload
//! valgrind --tool=callgrind --callgrind-out-file=/tmp/callgrind.out \
//!     target/release/examples/bolt_lineage_workload
//! callgrind_annotate /tmp/callgrind.out | less
//!
//! valgrind --tool=dhat --dhat-out-file=/tmp/dhat.out \
//!     target/release/examples/bolt_lineage_workload
//! ```
//!
//! Env vars: `BOLT_LEVELS` (default 5), `BOLT_PER_LEVEL` (default 2000),
//! `BOLT_DOWNSTREAM_QUERIES` (default 10), `BOLT_UPSTREAM_QUERIES` (default
//! 2000).

use aletheiadb::AletheiaDB;
use aletheiadb::core::PropertyMapBuilder;
use aletheiadb::core::lineage::{LineageQueryOptions, LineageRef};

fn env_usize(key: &str, default: usize) -> usize {
    std::env::var(key)
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(default)
}

/// Build a `levels * per_level`-fact citation DAG. Wave 0 facts each derive
/// from the single root; wave `k` facts each derive from one wave `k-1` fact
/// (round-robin), so the root's downstream closure spans exactly `levels`
/// hops and the whole graph. Returns `(db, root_ref, waves)` where
/// `waves[k]` holds every lineage ref created in wave `k`.
fn build_graph(levels: usize, per_level: usize) -> (AletheiaDB, LineageRef, Vec<Vec<LineageRef>>) {
    let db = AletheiaDB::new().expect("create ephemeral db");
    let root = db
        .create_node("Root", PropertyMapBuilder::new().build())
        .expect("root");
    let root_ref = db.node_lineage_ref(root).expect("root ref");

    let mut waves: Vec<Vec<LineageRef>> = Vec::with_capacity(levels);
    let mut prev: Vec<LineageRef> = vec![root_ref];
    for level in 0..levels {
        let mut this: Vec<LineageRef> = Vec::with_capacity(per_level);
        for i in 0..per_level {
            let parent = prev[i % prev.len()];
            let id = db
                .create_node_with_lineage(
                    "Fact",
                    PropertyMapBuilder::new()
                        .insert("lvl", level as i64)
                        .insert("i", i as i64)
                        .build(),
                    &[parent],
                )
                .expect("derived node");
            this.push(db.node_lineage_ref(id).expect("ref"));
        }
        waves.push(this.clone());
        prev = this;
    }
    (db, root_ref, waves)
}

fn main() {
    let levels = env_usize("BOLT_LEVELS", 5);
    let per_level = env_usize("BOLT_PER_LEVEL", 2000);
    let downstream_queries = env_usize("BOLT_DOWNSTREAM_QUERIES", 10);
    let upstream_queries = env_usize("BOLT_UPSTREAM_QUERIES", 2000);

    let (db, root_ref, waves) = build_graph(levels, per_level);
    let total_facts = levels * per_level;

    let mut sink: u64 = 0;

    // Blast radius from the root: spans the whole graph every call.
    let full_opts = LineageQueryOptions::new()
        .with_max_depth(levels)
        .with_limit(total_facts + 10);
    for _ in 0..downstream_queries {
        let view = db.downstream_lineage(root_ref, full_opts);
        sink = sink.wrapping_add(view.count() as u64);
    }

    // Blast radius from a mid-graph node (first fact of the middle wave):
    // a smaller, but still multi-level, downstream closure.
    let mid_level = levels / 2;
    let mid_ref = waves[mid_level][0];
    for _ in 0..downstream_queries {
        let view = db.downstream_lineage(mid_ref, full_opts);
        sink = sink.wrapping_add(view.count() as u64);
    }

    // Citation lookups: many single-hop upstream queries from leaf facts.
    let one_hop = LineageQueryOptions::new().with_max_depth(1).with_limit(64);
    let leaves = &waves[levels - 1];
    for i in 0..upstream_queries {
        let leaf = leaves[i % leaves.len()];
        let view = db.upstream_lineage(leaf, one_hop);
        sink = sink.wrapping_add(view.count() as u64);
    }

    println!(
        "sink={sink} levels={levels} per_level={per_level} total_facts={total_facts} downstream_queries={downstream_queries} upstream_queries={upstream_queries}"
    );
}
