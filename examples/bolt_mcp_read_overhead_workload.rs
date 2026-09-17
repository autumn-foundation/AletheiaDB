//! Deterministic workload quantifying the MCP per-query resource-limits
//! dispatch wrapper's overhead on a cheap read (Issue #3368 residue).
//!
//! `CLAUDE.md` documents that `dispatch_read_tool_limited` — the wrapper the
//! 13 tools in `RESOURCE_LIMITED_READ_TOOLS` (`traverse`, `hybrid_query`,
//! `find_similar`, `get_node_at_time`, `get_edge_at_time`,
//! `find_nodes_at_time`, plus the six #2907 semantic-search analysis tools)
//! are dispatched through — takes a `std::thread::spawn` + `mpsc` timeout-race
//! worker path under the *default* config (30s timeout, not the `disabled()`
//! zero-timeout inline fast path), and says "the per-call worker-spawn cost on
//! hot-path reads (including cheap `get_node_at_time`/`get_edge_at_time`) has
//! a quantifying micro-benchmark deferred to Lane-2" (`src/mcp/server.rs`,
//! `dispatch_read_tool_limited`'s doc comment). No existing benchmark measures
//! this: `benches/query_resource_limits.rs` profiles the engine-lane
//! `ResourceGuardIterator` (a different, later-landed guard, explicitly *not*
//! on this MCP dispatch path per that module's own doc comment), and no
//! `examples/bolt_*.rs` workload calls `dispatch_tool_json` at all.
//!
//! This workload drives the exact public entry point the autumn-web HTTP
//! surface uses (`AletheiaMcpServer::dispatch_tool_json`, `src/mcp/server.rs`
//! line ~840, "exposed for autumn-web migration (Issue #3524)") to call
//! `get_node_at_time` — a single O(1)-ish index lookup + anchor+delta
//! reconstruction, i.e. as cheap as a "read tool" gets — repeatedly against a
//! small dataset with real version history. `BOLT_MODE` selects between the
//! two configurations the doc comment above contrasts:
//!
//! - `wrapped` (default): `AletheiaMcpServer::new` — default
//!   `QueryLimitsConfig` (30_000ms effective timeout), so every call takes the
//!   `race_deadline` thread-spawn path.
//! - `inline`: `.with_query_limits(QueryLimitsConfig::disabled())` — effective
//!   timeout 0, so `dispatch_read_tool_limited` takes its documented
//!   zero-overhead inline branch and calls `dispatch_read_tool` directly, no
//!   thread spawned.
//!
//! Both modes call the identical handler (`handle_get_node_at_time`) with
//! identical arguments against an identical dataset; the only difference is
//! whether that call is wrapped in a spawned OS thread + `mpsc` channel. This
//! isolates the wrapper's own cost from the handler's cost, which is exactly
//! what the deferred quantifying benchmark needs to answer.
//!
//! This is a plain binary, not a criterion benchmark: criterion's
//! iteration/warmup loop is prohibitively slow stacked under valgrind's own
//! 20-50x slowdown, and thread creation adds enough real wall-clock cost that
//! wall-clock timing would be dominated by scheduler noise anyway. Running one
//! fixed, deterministic pass keeps instruction counts and syscall counts
//! reproducible run to run.
//!
//! ```text
//! cargo build --profile bench --example bolt_mcp_read_overhead_workload --features mcp-server
//!
//! BOLT_MODE=wrapped valgrind --tool=callgrind --callgrind-out-file=/tmp/cg_wrapped.out -- \
//!     target/release/examples/bolt_mcp_read_overhead_workload
//! BOLT_MODE=inline valgrind --tool=callgrind --callgrind-out-file=/tmp/cg_inline.out -- \
//!     target/release/examples/bolt_mcp_read_overhead_workload
//! callgrind_annotate /tmp/cg_wrapped.out | head -5
//! callgrind_annotate /tmp/cg_inline.out | head -5
//!
//! BOLT_MODE=wrapped strace -f -c -o /tmp/strace_wrapped.txt \
//!     target/release/examples/bolt_mcp_read_overhead_workload
//! BOLT_MODE=inline strace -f -c -o /tmp/strace_inline.txt \
//!     target/release/examples/bolt_mcp_read_overhead_workload
//! ```
//!
//! Env vars: `BOLT_NODES` (default 800), `BOLT_VERSIONS_PER_NODE` (default
//! 4), `BOLT_READ_ITERS` (default 20000), `BOLT_MODE` (`wrapped` | `inline`,
//! default `wrapped`).

use aletheiadb::AletheiaDB;
use aletheiadb::api::transaction::WriteRequestOptions;
use aletheiadb::mcp::{AletheiaMcpServer, QueryLimitsConfig};
use aletheiadb::prelude::*;
use serde_json::json;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

fn env_usize(key: &str, default: usize) -> usize {
    std::env::var(key)
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(default)
}

/// Build `node_count` nodes, each updated `versions_per_node` extra times
/// (real bi-temporal history, not a single-version trivial case), and return
/// their ids plus a "now" valid-time string (microseconds since epoch, the
/// format `parse_timestamp` accepts verbatim) taken after the last write so
/// every `get_node_at_time` call below reconstructs a real head version.
fn build_dataset(
    db: &AletheiaDB,
    node_count: usize,
    versions_per_node: usize,
) -> (Vec<u64>, String) {
    let mut node_ids = Vec::with_capacity(node_count);
    for i in 0..node_count {
        let props = PropertyMapBuilder::new()
            .insert("name", format!("Person{i}"))
            .insert("age", (i % 80) as i64)
            .insert("category", format!("cat_{}", i % 10))
            .build();
        let id = db.create_node("Person", props).expect("create_node");
        node_ids.push(id);
    }

    for &id in &node_ids {
        for v in 0..versions_per_node {
            let props = PropertyMapBuilder::new()
                .insert("age", v as i64)
                .insert("visits", (v * 3) as i64)
                .build();
            db.update_node_with_options(id, props, WriteRequestOptions::default())
                .expect("update_node_with_options");
        }
    }

    let now_micros = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock before epoch")
        .as_micros() as i64;

    (
        node_ids.iter().map(|id| id.as_u64()).collect(),
        now_micros.to_string(),
    )
}

fn main() {
    let node_count = env_usize("BOLT_NODES", 800);
    let versions_per_node = env_usize("BOLT_VERSIONS_PER_NODE", 4);
    let read_iters = env_usize("BOLT_READ_ITERS", 20_000);
    let mode = std::env::var("BOLT_MODE").unwrap_or_else(|_| "wrapped".to_string());

    let db = Arc::new(AletheiaDB::new().expect("create ephemeral db"));
    let (node_ids, valid_time) = build_dataset(&db, node_count, versions_per_node);

    let server = match mode.as_str() {
        "inline" => AletheiaMcpServer::new(db).with_query_limits(QueryLimitsConfig::disabled()),
        "wrapped" => AletheiaMcpServer::new(db),
        other => panic!("unknown BOLT_MODE '{other}', expected 'wrapped' or 'inline'"),
    };

    let mut sink: u64 = 0;
    for i in 0..read_iters {
        let node_id = node_ids[i % node_ids.len()];
        let args = json!({
            "node_id": node_id,
            "valid_time": valid_time,
        });
        let response = server.dispatch_tool_json("get_node_at_time", args);
        sink = sink.wrapping_add(response.len() as u64);
    }

    println!(
        "sink={sink} mode={mode} nodes={node_count} versions_per_node={versions_per_node} read_iters={read_iters}"
    );
}
