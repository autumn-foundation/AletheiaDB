//! Deterministic workload for named-snapshot reads (`Snapshot::get_node`,
//! `get_outgoing_edges`, `get_incoming_edges`, `get_edge`): the reproducible-
//! read path of Issue #3370.
//!
//! ```text
//! cargo build --profile bench --example bolt_snapshot_read_workload
//! valgrind --tool=callgrind --callgrind-out-file=/tmp/cg.out \
//!     target/bench/examples/bolt_snapshot_read_workload
//! callgrind_annotate /tmp/cg.out
//! ```
//!
//! Env vars: `BOLT_NODES` (default 1000), `BOLT_DEGREE` (default 8),
//! `BOLT_UPDATES` (default 2), `BOLT_READ_ITERS` (default 5).

use aletheiadb::prelude::*;

fn env_usize(key: &str, default: usize) -> usize {
    std::env::var(key)
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(default)
}

fn main() {
    let nodes = env_usize("BOLT_NODES", 1_000);
    let degree = env_usize("BOLT_DEGREE", 8);
    let updates = env_usize("BOLT_UPDATES", 2);
    let iters = env_usize("BOLT_READ_ITERS", 5);

    let db = AletheiaDB::new().expect("create ephemeral db");
    let mut ids = Vec::with_capacity(nodes);
    for i in 0..nodes {
        let props = PropertyMapBuilder::new()
            .insert("name", format!("Entity{i}"))
            .insert("age", (i % 80) as i64)
            .build();
        ids.push(db.create_node("Person", props).expect("create_node"));
    }
    let mut edges = Vec::new();
    for i in 0..nodes {
        for d in 1..=degree {
            let j = (i + d * 7) % nodes;
            edges.push(
                db.create_edge(ids[i], ids[j], "KNOWS", PropertyMapBuilder::new().build())
                    .expect("create_edge"),
            );
        }
    }
    db.create_snapshot("pin", None).expect("snapshot");
    for round in 0..updates {
        db.write(|tx| {
            for (i, id) in ids.iter().enumerate() {
                let props = PropertyMapBuilder::new()
                    .insert("age", ((i + round + 1) % 80) as i64)
                    .build();
                tx.update_node(*id, props)?;
            }
            Ok::<_, Error>(())
        })
        .expect("update round");
    }

    let snap = db.snapshot("pin").expect("handle");
    let mut sink = 0usize;
    for _ in 0..iters {
        for id in &ids {
            sink += snap.get_node(*id).map(|n| n.properties.len()).unwrap_or(0);
            sink += snap.get_outgoing_edges(*id).len();
            sink += snap.get_incoming_edges(*id).len();
        }
        for e in edges.iter().step_by(4) {
            sink += snap.get_edge(*e).map(|_| 1).unwrap_or(0);
        }
    }
    println!("sink={sink}");
}
