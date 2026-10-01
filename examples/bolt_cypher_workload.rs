//! Deterministic Cypher query workload for instruction-count profiling.
//!
//! No benchmark in this repository exercises `AletheiaDB::execute_cypher`'s
//! full string-in path (`benches/` has no `cypher` entry). `execute_cypher`
//! re-parses its input on every call (`src/db/query.rs` calls
//! `crate::cypher::plan_cypher(query_string)` with no prepared-statement
//! cache) -- exactly the call shape the MCP `query` tool and any AQL/Cypher
//! caller uses: an LLM agent re-issuing the same or similar statement text
//! repeatedly, never holding a parsed handle across calls.
//!
//! This is a plain binary, not a criterion benchmark: criterion's
//! iteration/warmup loop is prohibitively slow stacked under valgrind's own
//! 20-50x slowdown. Running one fixed, deterministic pass keeps instruction
//! counts reproducible run to run and keeps the profiling session tractable
//! (see `bolt_workload.rs`, `bolt_temporal_workload.rs` for the same pattern).
//!
//! ```text
//! cargo build --profile bench --features cypher --example bolt_cypher_workload
//! valgrind --tool=callgrind --callgrind-out-file=/tmp/callgrind.out \
//!     target/release/examples/bolt_cypher_workload
//! callgrind_annotate /tmp/callgrind.out | less
//!
//! valgrind --tool=dhat --dhat-out-file=/tmp/dhat.out \
//!     target/release/examples/bolt_cypher_workload
//! ```
//!
//! Env vars: `BOLT_NODES` (default 2000), `BOLT_OUT_DEGREE` (default 6),
//! `BOLT_QUERY_ITERS` (default 4000).

use aletheiadb::AletheiaDB;
use aletheiadb::core::property::PropertyMapBuilder;

fn env_usize(key: &str, default: usize) -> usize {
    std::env::var(key)
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(default)
}

fn main() {
    let node_count = env_usize("BOLT_NODES", 2_000);
    let out_degree = env_usize("BOLT_OUT_DEGREE", 6);
    let query_iters = env_usize("BOLT_QUERY_ITERS", 4_000);

    let db = AletheiaDB::new().expect("create ephemeral db");

    // --- Write phase: a social-graph-shaped dataset (mirrors bolt_workload.rs) ---
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
            let props = PropertyMapBuilder::new()
                .insert("weight", (i + j) as i64)
                .build();
            db.create_edge(node_ids[i], target, "KNOWS", props)
                .expect("create_edge");
        }
    }

    // --- Query phase: the same statement *text* re-issued repeatedly with a
    // varying literal, as an LLM caller re-typing a query template would --
    // never a held/prepared statement. Mixes a simple property-filtered
    // single-hop MATCH with a 2-hop pattern + WHERE + ORDER BY + LIMIT, the
    // shapes documented in CLAUDE.md's `query` tool guide. ---
    let mut sink: u64 = 0;
    for i in 0..query_iters {
        let age = (i % 80) as i64;
        let query = format!(
            "MATCH (n:Person)-[:KNOWS]->(f:Person) WHERE n.age = {age} RETURN f.name, f.age \
             ORDER BY f.age DESC LIMIT 10"
        );
        let results = db
            .execute_cypher(&query)
            .and_then(|r| r.collect_all())
            .expect("execute_cypher");
        sink = sink.wrapping_add(results.len() as u64);
    }

    println!(
        "sink={sink} nodes={node_count} edges={} query_iters={query_iters}",
        node_count * out_degree
    );
}
