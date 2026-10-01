//! Deterministic write-heavy workload profiling the uniqueness-constraint
//! reservation step on the commit path
//! (`src/api/transaction/write/constraint.rs::check_constraints` +
//! `src/core/constraint::ConstraintRegistry::reserve_for_transaction`) when
//! **no unique constraint has ever been declared** -- the common/default
//! case: `enable_unique_constraint` is opt-in (CLAUDE.md), so most databases
//! never call it.
//!
//! `check_constraints` runs unconditionally on every `WriteTransaction`
//! commit whenever a `constraint_registry` is attached (true for every
//! `AletheiaDB` created via the public constructors). For `CreateNode`/
//! `UpdateNode` it walks every buffered property calling
//! `ConstraintRegistry::is_constrained` (a `DashMap` lookup) even when the
//! registry's `declarations` map is empty; for `UpdateNode`/`DeleteNode` it
//! additionally calls `tx.current.get_node(id)` (an index lookup + `Node`
//! clone) purely to gather the old property values that a constraint
//! removal step would need to free -- work that is provably wasted whenever
//! no unique constraint is declared for any label.
//!
//! This workload exercises exactly the three buffered-op kinds
//! `check_constraints` walks (`CreateNode`/`UpdateNode`/`DeleteNode`)
//! through the public `AletheiaDB` API -- the same shapes the MCP
//! `create_node`/`update_node`/`delete_node` tools issue -- with zero
//! unique or schema constraints declared, isolating the reservation step's
//! per-write overhead in the common case.
//!
//! ```text
//! cargo build --profile bench --example bolt_constraint_overhead_workload
//! valgrind --tool=callgrind --callgrind-out-file=/tmp/cg.out \
//!     target/release/examples/bolt_constraint_overhead_workload
//! callgrind_annotate /tmp/cg.out | grep -i constraint
//! ```
//!
//! Env vars: `BOLT_NODES` (default 6000), `BOLT_UPDATE_ITERS` (default
//! 6000), `BOLT_DELETE_NODES` (default 2000, must be <= `BOLT_NODES`).

use aletheiadb::api::transaction::WriteOps;
use aletheiadb::prelude::*;

fn env_usize(key: &str, default: usize) -> usize {
    std::env::var(key)
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(default)
}

fn main() {
    let node_count = env_usize("BOLT_NODES", 6_000);
    let update_iters = env_usize("BOLT_UPDATE_ITERS", 6_000);
    let delete_nodes = env_usize("BOLT_DELETE_NODES", 2_000).min(node_count);

    let db = AletheiaDB::new().expect("create ephemeral db");

    // --- Create phase: a fact-store-shaped dataset, 4 properties/node. ---
    let mut node_ids = Vec::with_capacity(node_count);
    for i in 0..node_count {
        let props = PropertyMapBuilder::new()
            .insert("name", format!("Entity{i}"))
            .insert("age", (i % 100) as i64)
            .insert("category", format!("cat_{}", i % 10))
            .insert("active", i % 2 == 0)
            .build();
        node_ids.push(db.create_node("Person", props).expect("create_node"));
    }

    // --- Update phase: scattered full-property revisions on existing nodes
    // (an LLM agent correcting/refreshing previously-recorded facts). ---
    let mut sink: u64 = 0;
    for i in 0..update_iters {
        let node_id = node_ids[i % node_count];
        let props = PropertyMapBuilder::new()
            .insert("name", format!("Entity{i}-rev"))
            .insert("age", ((i + 1) % 100) as i64)
            .insert("category", format!("cat_{}", (i + 1) % 10))
            .insert("active", i % 3 == 0)
            .build();
        db.write(|tx| tx.update_node(node_id, props))
            .expect("update_node");
        sink = sink.wrapping_add(1);
    }

    // --- Delete phase: remove a distinct prefix of nodes (no connected
    // edges, so this never hits the DETACH/orphan-refusal path). ---
    for &node_id in &node_ids[..delete_nodes] {
        db.write(|tx| tx.delete_node(node_id)).expect("delete_node");
        sink = sink.wrapping_add(1);
    }

    println!(
        "sink={sink} nodes={node_count} update_iters={update_iters} delete_nodes={delete_nodes}"
    );
}
