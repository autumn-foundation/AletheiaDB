//! Deterministic mixed read/write workload driven through the MCP tool
//! **dispatch** layer (`AletheiaMcpServer::dispatch_tool_json`), for
//! instruction-count profiling.
//!
//! Prior Bolt profiling passes (`examples/bolt_workload.rs`,
//! `examples/bolt_profile_workload.rs`) drove the raw `AletheiaDB` Rust API
//! directly. That is a legitimate workload but it skips the layer every real
//! LLM agent session actually pays for: JSON-RPC `tools/call` argument
//! deserialization into a typed request struct, per-tool dispatch, and
//! response serialization back to a JSON string
//! (`AletheiaMcpServer::dispatch_tool_json`, `src/mcp/server.rs`). This
//! harness exercises that dispatch path in-process (no stdio/subprocess, so
//! no pipe/process-spawn noise), matching the shape `benches/mcp_round_trip.rs`
//! drives over a real child process, but structured as a single fixed pass
//! for callgrind (see `bolt_workload.rs` for why: criterion's iteration loop
//! is unusable stacked under valgrind's 20-50x slowdown).
//!
//! ```text
//! cargo build --profile bench --example bolt_mcp_workload
//! valgrind --tool=callgrind --callgrind-out-file=/tmp/callgrind_mcp.out \
//!     target/release/examples/bolt_mcp_workload
//! callgrind_annotate --threshold=95 /tmp/callgrind_mcp.out | less
//!
//! valgrind --tool=dhat --dhat-out-file=/tmp/dhat_mcp.out \
//!     target/release/examples/bolt_mcp_workload
//! ```
//!
//! Env vars: `BOLT_MCP_NODES` (default 1500), `BOLT_MCP_OUT_DEGREE` (default
//! 6), `BOLT_MCP_READ_ITERS` (default 1500).

use aletheiadb::AletheiaDB;
use aletheiadb::mcp::AletheiaMcpServer;
use serde_json::{Value, json};
use std::sync::Arc;

fn env_usize(key: &str, default: usize) -> usize {
    std::env::var(key)
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(default)
}

fn field_u64(text: &str, field: &str) -> Option<u64> {
    serde_json::from_str::<Value>(text)
        .ok()?
        .get(field)?
        .as_u64()
}

fn main() {
    let node_count = env_usize("BOLT_MCP_NODES", 1_500);
    let out_degree = env_usize("BOLT_MCP_OUT_DEGREE", 6);
    let read_iters = env_usize("BOLT_MCP_READ_ITERS", 1_500);

    let db = Arc::new(AletheiaDB::new().expect("create ephemeral db"));

    // Mirrors bolt_workload.rs: a realistically-configured deployment enables
    // the equality index for the repeated property lookups the read loop
    // issues, rather than forcing every `list_nodes` property filter through
    // the unindexed fallback scan.
    db.property_index("Person", "category")
        .enable()
        .expect("enable_property_index");

    let server = AletheiaMcpServer::new(db);

    // --- Write phase: a social-graph-shaped dataset, via tools/call ---
    let mut node_ids: Vec<u64> = Vec::with_capacity(node_count);
    for i in 0..node_count {
        let category = format!("cat_{}", i % 10);
        let args = json!({
            "label": "Person",
            "properties": {
                "name": format!("Person{i}"),
                "age": (i % 80) as i64,
                "category": category,
            }
        });
        let resp = server.dispatch_tool_json("create_node", args);
        let id = field_u64(&resp, "id").expect("create_node returned an id");
        node_ids.push(id);
    }

    for i in 0..node_count {
        for j in 0..out_degree {
            let target = node_ids[(i + j + 1) % node_count];
            let label = if j % 2 == 0 { "KNOWS" } else { "FOLLOWS" };
            let args = json!({
                "source_id": node_ids[i],
                "target_id": target,
                "label": label,
                "properties": { "weight": (i + j) as i64 },
            });
            let resp = server.dispatch_tool_json("create_edge", args);
            // Keep the response alive on the stack past dispatch so the
            // serialize step isn't optimized away; error would panic below.
            if field_u64(&resp, "id").is_none() {
                panic!("create_edge failed: {resp}");
            }
        }
    }

    // --- Read phase: the mixed get_node / get_outgoing_edges / traverse /
    // list_nodes(property filter) pattern an LLM agent session issues
    // repeatedly through tools/call, matching the MCP tool names directly
    // (not the internal Rust method names). ---
    let mut sink: u64 = 0;
    for i in 0..read_iters {
        let node = node_ids[i % node_count];

        let get_resp = server.dispatch_tool_json("get_node", json!({ "node_id": node }));
        sink = sink.wrapping_add(get_resp.len() as u64);

        let edges_resp =
            server.dispatch_tool_json("get_outgoing_edges", json!({ "node_id": node }));
        sink = sink.wrapping_add(edges_resp.len() as u64);

        let traverse_resp = server.dispatch_tool_json(
            "traverse",
            json!({
                "start_node_id": node,
                "edge_label": "KNOWS",
                "direction": "outgoing",
                "depth": 3,
                "limit": 100,
            }),
        );
        sink = sink.wrapping_add(traverse_resp.len() as u64);

        let category = format!("cat_{}", i % 10);
        let list_resp = server.dispatch_tool_json(
            "list_nodes",
            json!({
                "label": "Person",
                "property_key": "category",
                "property_value": category,
                "limit": 50,
            }),
        );
        sink = sink.wrapping_add(list_resp.len() as u64);
    }

    println!(
        "sink={sink} nodes={node_count} edges={} read_iters={read_iters}",
        node_count * out_degree
    );
}
