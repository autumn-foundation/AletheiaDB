//! Deterministic multi-hop graph-traversal workload for instruction-count
//! profiling of the `AletheiaDB::query().start(..).traverse(label)` path
//! (`src/query/builder.rs`) — the API `docs/ARCHITECTURE.md`/`CLAUDE.md`
//! document for bi-temporal graph traversal (`db.query().as_of(..).start(id)
//! .traverse("KNOWS").execute(&db)`), and the same physical operator the MCP
//! `traverse` tool drives.
//!
//! `TraversalIterator::get_neighbors` (`src/query/executor/iterators.rs`)
//! pulls each frontier node's edges through the zero-allocation
//! `get_outgoing_edges_with_label_iter`/`get_outgoing_edges_iter` streaming
//! iterators (`src/storage/current/iterators.rs`) rather than the
//! Vec-returning `get_outgoing_edges` — so this harness exercises the
//! streaming-iterator `next()` path per Issue #187/#190, not the Vec path
//! `examples/bolt_workload.rs`'s 3-hop read phase already covers.
//!
//! This is a plain binary, not a criterion benchmark: criterion's
//! iteration/warmup loop is prohibitively slow stacked under valgrind's own
//! 20-50x slowdown. Running one fixed, deterministic pass keeps instruction
//! counts reproducible run to run.
//!
//! ```text
//! cargo build --profile bench --example bolt_adjacency_iterator_workload
//! valgrind --tool=callgrind --callgrind-out-file=/tmp/callgrind.out \
//!     target/release/examples/bolt_adjacency_iterator_workload
//! callgrind_annotate /tmp/callgrind.out src/storage/current/iterators.rs | less
//!
//! valgrind --tool=dhat --dhat-out-file=/tmp/dhat.out \
//!     target/release/examples/bolt_adjacency_iterator_workload
//! ```
//!
//! Env vars: `BOLT_NODES` (default 3000), `BOLT_OUT_DEGREE` (default 8),
//! `BOLT_START_ITERS` (default 3000, number of distinct 2-hop traversals
//! issued).
//!
//! The write phase (graph construction through the durable, fsync'd WAL of
//! `AletheiaDB::new()`) costs real instructions of its own and has nothing to
//! do with the traversal path this harness targets, so isolate the read
//! phase's contribution to a profile by diffing two runs -- `BOLT_START_ITERS=0`
//! (write phase only) against the default (write + read) -- and subtracting
//! their total `Ir` counts, rather than reading the read phase's share off
//! the combined run directly (mirrors the isolation
//! `examples/bolt_workload.rs`-adjacent Issue #3794 findings used).

use aletheiadb::prelude::*;
use aletheiadb::query::executor::EntityResult;

fn env_usize(key: &str, default: usize) -> usize {
    std::env::var(key)
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(default)
}

fn main() {
    let node_count = env_usize("BOLT_NODES", 3_000);
    let out_degree = env_usize("BOLT_OUT_DEGREE", 8);
    let start_iters = env_usize("BOLT_START_ITERS", 3_000);

    let db = AletheiaDB::new().expect("create ephemeral db");

    // --- Write phase: every edge is "KNOWS", so `traverse("KNOWS")` never
    // filters an edge out -- the read phase below is dominated by adjacency
    // iteration, not label-mismatch skips. ---
    let mut node_ids = Vec::with_capacity(node_count);
    for i in 0..node_count {
        let props = PropertyMapBuilder::new()
            .insert("name", format!("Person{i}"))
            .insert("age", (i % 80) as i64)
            .build();
        node_ids.push(db.create_node("Person", props).expect("create_node"));
    }
    for i in 0..node_count {
        for j in 0..out_degree {
            let target = node_ids[(i + j + 1) % node_count];
            let props = PropertyMapBuilder::new()
                .insert("weight", (i + j) as i64)
                .build();
            db.create_edge(node_ids[i], target, "KNOWS", props)
                .expect("create_edge");
        }
    }

    // --- Read phase: the documented `db.query().start(id).traverse(label)`
    // pattern, 2 hops deep, over many distinct start nodes -- the same
    // "who did Alice know, and who did they know" pattern an LLM agent
    // issues via the MCP `traverse` tool. ---
    let mut sink: u64 = 0;
    for i in 0..start_iters {
        let start = node_ids[i % node_count];
        let results = db
            .query()
            .start(start)
            .traverse("KNOWS")
            .traverse("KNOWS")
            .execute(&db)
            .expect("traverse query");
        for row in results.flatten() {
            let id = match row.entity {
                EntityResult::Node(n) => n.id.as_u64(),
                EntityResult::NodeId(id) => id.as_u64(),
                EntityResult::Edge(e) => e.id.as_u64(),
                EntityResult::EdgeId(id) => id.as_u64(),
                EntityResult::Null => 0,
                _ => 0,
            };
            sink = sink.wrapping_add(id);
        }
    }

    println!("sink={sink} nodes={node_count} out_degree={out_degree} start_iters={start_iters}");
}
