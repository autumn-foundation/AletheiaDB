//! Bolt profiling harness (not a criterion benchmark).
//!
//! Wall-clock timing on this hardware is unreliable, so this binary is meant
//! to be driven through a deterministic profiler instead of `cargo bench`:
//!
//! ```bash
//! cargo build --release --profile bench --example bolt_profile_workload
//! valgrind --tool=callgrind --callgrind-out-file=callgrind.out \
//!     target/release/examples/bolt_profile_workload
//! callgrind_annotate callgrind.out | less
//!
//! valgrind --tool=dhat --dhat-out-file=dhat.out \
//!     target/release/examples/bolt_profile_workload
//! ```
//!
//! The workload mirrors what `benches/current_state.rs` and
//! `benches/aletheiadb.rs` already measure with criterion (graph
//! CRUD + traversal through the public `AletheiaDB` API, the thing this
//! database spends its life doing per the <1us single-hop / <100us 3-hop
//! targets in the docs), but as a single realistic run through a public
//! entry point rather than a microbenchmark of one function.

use aletheiadb::api::transaction::WriteRequestOptions;
use aletheiadb::storage::wal::DurabilityMode;
use aletheiadb::{AletheiaDB, NodeId, PropertyMapBuilder, WalConfigBuilder};
use std::hint::black_box;

const NODE_COUNT: usize = 3_000;
const OUT_DEGREE: usize = 8;
const SINGLE_HOP_LOOKUPS: usize = 5_000;
const MULTI_HOP_LOOKUPS: usize = 500;
const NODE_READS: usize = 2_000;
const NODE_UPDATES: usize = 1_000;

/// Isolate graph/storage cost from WAL fsync cost, matching the convention
/// already used by `benches/aletheiadb.rs` and `benches/current_state.rs`.
fn create_db() -> AletheiaDB {
    let wal_config = WalConfigBuilder::new()
        .durability_mode(DurabilityMode::Async {
            flush_interval_ms: 100,
        })
        .build();
    AletheiaDB::with_wal_config(wal_config).unwrap()
}

fn build_graph(db: &AletheiaDB) -> Vec<NodeId> {
    let mut node_ids = Vec::with_capacity(NODE_COUNT);
    for i in 0..NODE_COUNT {
        let props = PropertyMapBuilder::new()
            .insert("id", i as i64)
            .insert("name", format!("node-{i}"))
            .build();
        node_ids.push(db.create_node("Person", props).unwrap());
    }

    for i in 0..NODE_COUNT {
        for j in 0..OUT_DEGREE {
            let target = (i + j + 1) % NODE_COUNT;
            db.create_edge(
                node_ids[i],
                node_ids[target],
                if j % 2 == 0 { "KNOWS" } else { "FOLLOWS" },
                PropertyMapBuilder::new()
                    .insert("weight", (i + j) as i64)
                    .build(),
            )
            .unwrap();
        }
    }

    node_ids
}

fn main() {
    let db = create_db();
    let node_ids = build_graph(&db);

    for i in 0..SINGLE_HOP_LOOKUPS {
        let node = node_ids[i % node_ids.len()];
        let edges = db.get_outgoing_edges(node);
        black_box(edges);
    }

    for i in 0..MULTI_HOP_LOOKUPS {
        let start = node_ids[i % node_ids.len()];
        let mut frontier = vec![start];
        for _ in 0..3 {
            let mut next = Vec::new();
            for node in &frontier {
                for edge_id in db.get_outgoing_edges(*node) {
                    if let Ok(edge) = db.get_edge(edge_id) {
                        next.push(edge.target);
                    }
                }
            }
            frontier = next;
        }
        black_box(frontier);
    }

    for i in 0..NODE_READS {
        let node = node_ids[i % node_ids.len()];
        black_box(db.get_node(node).unwrap());
    }

    for i in 0..NODE_UPDATES {
        let node = node_ids[i % node_ids.len()];
        let props = PropertyMapBuilder::new()
            .insert("id", i as i64)
            .insert("value", i as i64)
            .build();
        db.update_node_with_options(node, props, WriteRequestOptions::new())
            .unwrap();
    }
}
