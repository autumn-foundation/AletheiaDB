//! Deterministic agent-scoped-namespace read workload for instruction-count
//! profiling (Issue #3349).
//!
//! Models the documented pattern: one shared knowledge-base namespace plus a
//! private scratch namespace per agent. Each agent reads through a
//! `[kb, agent]` union scope (the isolated-by-default read path) via the
//! public `*_scoped` API: point reads, label listing, property lookup and
//! bounded BFS traversal.
//!
//! Plain binary (not criterion) so a single fixed pass runs under valgrind.
//!
//! ```text
//! cargo build --profile bench --example bolt_namespace_workload
//! valgrind --tool=callgrind --callgrind-out-file=/tmp/callgrind.out \
//!     target/release/examples/bolt_namespace_workload
//! callgrind_annotate /tmp/callgrind.out | less
//! valgrind --tool=dhat --dhat-out-file=/tmp/dhat.out \
//!     target/release/examples/bolt_namespace_workload
//! ```
//!
//! Env vars: `BOLT_KB_NODES` (2000), `BOLT_AGENTS` (8), `BOLT_AGENT_NODES`
//! (250), `BOLT_ROUNDS` (20).

use aletheiadb::core::{Namespace, NamespaceScope, PropertyMapBuilder, PropertyValue};
use aletheiadb::{AletheiaDB, NodeId};

fn env_usize(key: &str, default: usize) -> usize {
    std::env::var(key)
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(default)
}

/// Create `n` nodes in `ns`, each linked to the previous and to node `i/2`
/// (a binary-tree-ish fan-in so BFS has real frontiers).
fn populate(db: &AletheiaDB, ns: &str, n: usize) -> Vec<NodeId> {
    let mut ids: Vec<NodeId> = Vec::with_capacity(n);
    for i in 0..n {
        let id = db
            .create_node_in_namespace(
                "Fact",
                PropertyMapBuilder::new()
                    .insert("bucket", (i % 50) as i64)
                    .insert("i", i as i64)
                    .build(),
                ns,
            )
            .expect("node");
        if i > 0 {
            db.create_edge_in_namespace(
                ids[i - 1],
                id,
                "NEXT",
                PropertyMapBuilder::new().build(),
                ns,
            )
            .expect("edge");
            db.create_edge_in_namespace(
                ids[i / 2],
                id,
                "NEXT",
                PropertyMapBuilder::new().build(),
                ns,
            )
            .expect("edge");
        }
        ids.push(id);
    }
    ids
}

fn main() {
    let kb_nodes = env_usize("BOLT_KB_NODES", 2000);
    let agents = env_usize("BOLT_AGENTS", 8);
    let agent_nodes = env_usize("BOLT_AGENT_NODES", 250);
    let rounds = env_usize("BOLT_ROUNDS", 20);

    let db = AletheiaDB::new().expect("db");
    let kb_ids = populate(&db, "kb", kb_nodes);
    let mut agent_ids: Vec<Vec<NodeId>> = Vec::new();
    for a in 0..agents {
        agent_ids.push(populate(&db, &format!("agent{a}"), agent_nodes));
    }

    let kb = Namespace::new("kb").expect("ns");
    let scopes: Vec<NamespaceScope> = (0..agents)
        .map(|a| {
            NamespaceScope::list(vec![
                kb.clone(),
                Namespace::new(format!("agent{a}")).expect("ns"),
            ])
            .expect("scope")
        })
        .collect();

    let mut sink: u64 = 0;
    for r in 0..rounds {
        for (a, scope) in scopes.iter().enumerate() {
            // Point reads.
            for k in 0..20 {
                let id = if k % 2 == 0 {
                    kb_ids[(r * 31 + k * 97) % kb_ids.len()]
                } else {
                    agent_ids[a][(r * 7 + k * 13) % agent_ids[a].len()]
                };
                sink = sink.wrapping_add(db.get_node_scoped(id, scope).is_ok() as u64);
            }
            // Label listing.
            let ids = db.list_nodes_scoped(Some("Fact"), scope).expect("list");
            sink = sink.wrapping_add(ids.len() as u64);
            // Property lookup.
            let found = db
                .find_nodes_by_property_scoped(
                    "Fact",
                    "bucket",
                    &PropertyValue::Int(((r + a) % 50) as i64),
                    scope,
                )
                .expect("find");
            sink = sink.wrapping_add(found.len() as u64);
            // Bounded BFS from the agent's own root.
            let reach = db
                .traverse_scoped(agent_ids[a][0], Some("NEXT"), 4, scope)
                .expect("traverse");
            sink = sink.wrapping_add(reach.len() as u64);
        }
    }
    println!("sink={sink} kb={kb_nodes} agents={agents} agent_nodes={agent_nodes} rounds={rounds}");
}
