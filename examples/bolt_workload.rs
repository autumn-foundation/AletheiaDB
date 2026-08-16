//! Deterministic mixed read/write workload for instruction-count profiling.
//!
//! Exercises the public `AletheiaDB` API the way an LLM agent session
//! actually does through the MCP surface: build a small social-graph-shaped
//! dataset, then issue the read patterns `get_node`/`traverse`/
//! `find_nodes_by_property`-style MCP tools take (single-hop, 3-hop, and
//! property lookups), matching the shapes already exercised in
//! `benches/current_state.rs`.
//!
//! This is a plain binary, not a criterion benchmark: criterion's
//! iteration/warmup loop is prohibitively slow stacked under valgrind's own
//! 20-50x slowdown. Running one fixed, deterministic pass keeps instruction
//! counts reproducible run to run and keeps the profiling session tractable.
//!
//! ```text
//! cargo build --profile bench --example bolt_workload
//! valgrind --tool=callgrind --callgrind-out-file=/tmp/callgrind.out \
//!     target/bench/examples/bolt_workload
//! callgrind_annotate /tmp/callgrind.out | less
//!
//! valgrind --tool=dhat --dhat-out-file=/tmp/dhat.out \
//!     target/bench/examples/bolt_workload
//! ```
//!
//! Env vars: `BOLT_NODES` (default 5000), `BOLT_OUT_DEGREE` (default 8),
//! `BOLT_READ_ITERS` (default 4000).

use aletheiadb::core::property::PropertyValue;
use aletheiadb::prelude::*;
use std::sync::Arc;

fn env_usize(key: &str, default: usize) -> usize {
    std::env::var(key)
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(default)
}

fn main() {
    let node_count = env_usize("BOLT_NODES", 5_000);
    let out_degree = env_usize("BOLT_OUT_DEGREE", 8);
    let read_iters = env_usize("BOLT_READ_ITERS", 4_000);

    let db = AletheiaDB::new().expect("create ephemeral db");

    // --- Write phase: a social-graph-shaped dataset ---
    let mut node_ids = Vec::with_capacity(node_count);
    for i in 0..node_count {
        let category = format!("cat_{}", i % 10);
        let props = PropertyMapBuilder::new()
            .insert("name", format!("Person{i}"))
            .insert("age", (i % 80) as i64)
            .insert("category", category.as_str())
            .build();
        node_ids.push(db.create_node("Person", props).expect("create_node"));
    }

    for i in 0..node_count {
        for j in 0..out_degree {
            let target = node_ids[(i + j + 1) % node_count];
            let label = if j % 2 == 0 { "KNOWS" } else { "FOLLOWS" };
            let props = PropertyMapBuilder::new()
                .insert("weight", (i + j) as i64)
                .build();
            db.create_edge(node_ids[i], target, label, props)
                .expect("create_edge");
        }
    }

    // --- Read phase: get_node + single-hop + 3-hop traversal + property
    // lookup, the mixed pattern a caller actually issues repeatedly. ---
    let mut sink: u64 = 0;
    for i in 0..read_iters {
        let node = node_ids[i % node_count];

        if let Ok(n) = db.get_node(node) {
            sink = sink.wrapping_add(n.properties.len() as u64);
        }

        let hop1 = db.get_outgoing_edges(node);
        sink = sink.wrapping_add(hop1.len() as u64);

        for edge_id in &hop1 {
            if let Ok(edge) = db.get_edge(*edge_id) {
                let hop2 = db.get_outgoing_edges(edge.target);
                for edge_id2 in &hop2 {
                    if let Ok(edge2) = db.get_edge(*edge_id2) {
                        let hop3 = db.get_outgoing_edges(edge2.target);
                        sink = sink.wrapping_add(hop3.len() as u64);
                    }
                }
            }
        }

        let category = format!("cat_{}", i % 10);
        let target = PropertyValue::String(Arc::from(category.as_str()));
        let matches = db.find_nodes_by_property("Person", "category", &target);
        sink = sink.wrapping_add(matches.len() as u64);
    }

    println!(
        "sink={sink} nodes={node_count} edges={} read_iters={read_iters}",
        node_count * out_degree
    );
}
