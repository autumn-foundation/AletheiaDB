//! Deterministic workload for the MCP token-budget response shaper's rung-4
//! array-truncation search (Issue #3353), for instruction-count /
//! allocation-count profiling.
//!
//! `list_nodes` is one of the twenty `BUDGETABLE_READ_TOOLS`
//! (`CLAUDE.md`, "Token-budget-aware responses (Issue #3353)"): a caller may
//! pass `max_response_tokens`/`max_response_bytes` and the response is
//! guaranteed to fit, degrading along a fixed ladder (full -> elide bulky
//! property values -> per-entity summaries -> counts-plus-handles) when it
//! doesn't fit as-is. This is a realistic shape for a context-bounded LLM
//! agent: "list nodes, but spend at most N tokens on the answer" rather than
//! guessing a row `limit` up front.
//!
//! This workload drives that exact scenario through the public
//! `AletheiaMcpServer::dispatch_tool_json` entry point: a large `list_nodes`
//! page (hundreds to thousands of entities) against a `max_response_tokens`
//! small enough that even the fully-summarized page overflows, forcing the
//! rung-4 (`counts_and_handles`) binary search in
//! `src/mcp/budget.rs::truncate_arrays_to_fit` to run. No existing benchmark
//! exercises this: `benches/` has no `budget`/token-shaping entry, and no
//! other `examples/bolt_*.rs` workload passes `max_response_tokens`.
//!
//! This is a plain binary, not a criterion benchmark: criterion's
//! iteration/warmup loop is prohibitively slow stacked under valgrind's own
//! 20-50x slowdown. Running one fixed, deterministic pass keeps instruction
//! and allocation counts reproducible run to run.
//!
//! ```text
//! cargo build --profile bench --example bolt_budget_truncation_workload --features mcp-server
//! valgrind --tool=callgrind --callgrind-out-file=/tmp/callgrind.out \
//!     target/release/examples/bolt_budget_truncation_workload
//! callgrind_annotate /tmp/callgrind.out | less
//!
//! valgrind --tool=dhat --dhat-out-file=/tmp/dhat.out \
//!     target/release/examples/bolt_budget_truncation_workload
//! ```
//!
//! Env vars: `BOLT_NODES` (default 3000), `BOLT_MAX_TOKENS` (default 400,
//! i.e. an ~1600 byte cap -- small enough to force the counts-and-handles
//! rung on a 3000-entity page), `BOLT_ITERS` (default 30).

use aletheiadb::AletheiaDB;
use aletheiadb::mcp::AletheiaMcpServer;
use aletheiadb::prelude::*;
use serde_json::json;
use std::sync::Arc;

fn env_usize(key: &str, default: usize) -> usize {
    std::env::var(key)
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(default)
}

fn build_dataset(db: &AletheiaDB, node_count: usize) {
    for i in 0..node_count {
        let props = PropertyMapBuilder::new()
            .insert("name", format!("Person-{i}"))
            .insert("age", (i % 80) as i64)
            .insert("category", format!("cat_{}", i % 10))
            .insert("active", i % 2 == 0)
            .build();
        db.create_node("Person", props).expect("create_node");
    }
}

fn main() {
    let node_count = env_usize("BOLT_NODES", 3_000);
    let max_tokens = env_usize("BOLT_MAX_TOKENS", 400);
    let iters = env_usize("BOLT_ITERS", 30);

    let db = Arc::new(AletheiaDB::new().expect("create ephemeral db"));
    build_dataset(&db, node_count);
    let server = AletheiaMcpServer::new(db);

    let mut sink: u64 = 0;
    let mut last_response = String::new();
    for _ in 0..iters {
        let args = json!({
            "label": "Person",
            "limit": node_count,
            "max_response_tokens": max_tokens,
        });
        let response = server.dispatch_tool_json("list_nodes", args);
        sink = sink.wrapping_add(response.len() as u64);
        last_response = response;
    }

    // Sanity check: confirm the workload actually reaches the rung this
    // harness exists to profile, so a future regression that stops reaching
    // it (e.g. a change to entity size or the elide/summarize thresholds)
    // fails loudly here rather than silently profiling a cheaper path.
    let parsed: serde_json::Value =
        serde_json::from_str(&last_response).expect("response is valid JSON");
    let rung = parsed
        .get("budget")
        .and_then(|b| b.get("rung"))
        .and_then(|r| r.as_str())
        .expect("response carries a budget.rung field");
    assert_eq!(
        rung, "counts_and_handles",
        "workload must reach the counts_and_handles rung to exercise the \
         truncate_arrays_to_fit binary search this harness profiles; got rung={rung} \
         (tune BOLT_MAX_TOKENS/BOLT_NODES)"
    );

    println!(
        "sink={sink} nodes={node_count} max_tokens={max_tokens} iters={iters} last_rung={rung}"
    );
}
