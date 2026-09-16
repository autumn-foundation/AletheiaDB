//! Deterministic workload for the MCP `apply_batch` write path (Issue #3231),
//! for instruction-count profiling.
//!
//! `apply_batch` is the documented shape an LLM agent uses to build a
//! subgraph in one call instead of N single-op calls (CLAUDE.md: "an LLM
//! builds an entity-with-relationships subgraph in ONE call instead of N
//! calls with N-1 possible partially-committed states"). No existing
//! benchmark exercises it: `benches/` has no `apply_batch`/`mcp` entry, and
//! `bolt_workload.rs` drives `create_node`/`create_edge` directly, one op per
//! transaction, never through the MCP batch surface.
//!
//! Each simulated batch is a small star-shaped subgraph: a set of
//! `create_node` ops (each carrying a handful of properties and a `ref`
//! alias) followed by `create_edge` ops linking them purely by alias (no
//! committed ids), so batches never collide with each other and no
//! cross-batch state is needed. This is the realistic shape `apply_batch`'s
//! own module docs describe as the motivating case. Requests are built as
//! raw `serde_json::Value` bodies (via `ApplyBatchRequest`'s untyped
//! `operations` field) and round-tripped through `AletheiaMcpServer::apply_batch`
//! exactly as a JSON-RPC caller's payload would be.
//!
//! This is a plain binary, not a criterion benchmark: criterion's
//! iteration/warmup loop is prohibitively slow stacked under valgrind's own
//! 20-50x slowdown. Running one fixed, deterministic pass keeps instruction
//! counts reproducible run to run.
//!
//! ```text
//! cargo build --profile bench --example bolt_batch_workload --features mcp-server
//! valgrind --tool=callgrind --callgrind-out-file=/tmp/callgrind.out \
//!     target/release/examples/bolt_batch_workload
//! callgrind_annotate /tmp/callgrind.out | less
//! ```
//!
//! Env vars: `BOLT_BATCHES` (default 200), `BOLT_NODES_PER_BATCH` (default
//! 40), `BOLT_EDGES_PER_NODE` (default 2).

use aletheiadb::mcp::{AletheiaMcpServer, ApplyBatchRequest};
use aletheiadb::AletheiaDB;
use serde_json::json;
use std::sync::Arc;

fn env_usize(key: &str, default: usize) -> usize {
    std::env::var(key)
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(default)
}

/// Build one self-contained batch: `nodes_per_batch` `create_node` ops (each
/// with 5 properties, aliased "n0".."n{k}") plus `edges_per_node` `create_edge`
/// ops per node linking it to later nodes purely by alias.
fn build_batch(batch_index: usize, nodes_per_batch: usize, edges_per_node: usize) -> Vec<serde_json::Value> {
    let mut ops = Vec::with_capacity(nodes_per_batch + nodes_per_batch * edges_per_node);

    for i in 0..nodes_per_batch {
        ops.push(json!({
            "op": "create_node",
            "label": "Person",
            "properties": {
                "name": format!("Person-{batch_index}-{i}"),
                "age": (i % 80) as i64,
                "category": format!("cat_{}", i % 10),
                "active": i % 2 == 0,
                "score": (i as f64) * 0.5,
            },
            "ref": format!("n{i}"),
        }));
    }

    for i in 0..nodes_per_batch {
        for j in 0..edges_per_node {
            let target = (i + j + 1) % nodes_per_batch;
            ops.push(json!({
                "op": "create_edge",
                "source_id": format!("$n{i}"),
                "target_id": format!("$n{target}"),
                "label": if j % 2 == 0 { "KNOWS" } else { "FOLLOWS" },
                "properties": {
                    "weight": (i + j) as i64,
                },
            }));
        }
    }

    ops
}

fn main() {
    let batches = env_usize("BOLT_BATCHES", 200);
    let nodes_per_batch = env_usize("BOLT_NODES_PER_BATCH", 40);
    let edges_per_node = env_usize("BOLT_EDGES_PER_NODE", 2);

    let db = Arc::new(AletheiaDB::new().expect("create ephemeral db"));
    let server = AletheiaMcpServer::new(db);

    let mut sink: u64 = 0;
    for batch_index in 0..batches {
        let operations = build_batch(batch_index, nodes_per_batch, edges_per_node);
        let req = ApplyBatchRequest { operations };
        let response = server.apply_batch(req);
        sink = sink.wrapping_add(response.len() as u64);
    }

    println!(
        "sink={sink} batches={batches} nodes_per_batch={nodes_per_batch} edges_per_node={edges_per_node}"
    );
}
