//! Deterministic workload for the point-in-time find path
//! (`find_nodes_at_time` / `find_nodes_by_property_at` / `schema_as_of`),
//! the engine behind the MCP `find_nodes_at_time` and bi-temporal `get_schema`
//! tools.
//!
//! ```text
//! cargo build --profile bench --example bolt_asof_find_workload
//! valgrind --tool=callgrind --callgrind-out-file=/tmp/cg.out \
//!     target/bench/examples/bolt_asof_find_workload
//! callgrind_annotate /tmp/cg.out
//! ```
//!
//! Env vars: `BOLT_NODES` (default 2000), `BOLT_UPDATES` (default 2, extra
//! versions per node), `BOLT_FIND_ITERS` (default 20).

use aletheiadb::core::property::PropertyValue;
use aletheiadb::core::temporal::time;
use aletheiadb::prelude::*;
use std::sync::Arc;

fn env_usize(key: &str, default: usize) -> usize {
    std::env::var(key)
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(default)
}

fn main() {
    let nodes = env_usize("BOLT_NODES", 2_000);
    let updates = env_usize("BOLT_UPDATES", 2);
    let iters = env_usize("BOLT_FIND_ITERS", 20);

    let db = AletheiaDB::new().expect("create ephemeral db");
    let mut ids = Vec::with_capacity(nodes);
    for i in 0..nodes {
        let label = if i % 4 == 0 { "Company" } else { "Person" };
        let props = PropertyMapBuilder::new()
            .insert("name", format!("Entity{i}"))
            .insert("age", (i % 80) as i64)
            .insert("category", format!("cat_{}", i % 10))
            .build();
        ids.push(db.create_node(label, props).expect("create_node"));
    }
    let checkpoint = time::now();
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
    let now = time::now();

    let mut sink = 0usize;
    for i in 0..iters {
        let at = if i % 2 == 0 { checkpoint } else { now };
        sink += db
            .find_nodes_at_time("Person", at, at)
            .expect("find")
            .nodes
            .len();
        let v = PropertyValue::String(Arc::from(format!("cat_{}", i % 10).as_str()));
        sink += db
            .find_nodes_by_property_at("Person", "category", &v, at, at)
            .expect("find prop")
            .nodes
            .len();
        let s = db.schema_as_of(at, at).expect("schema");
        sink += s.node_labels.len();
    }
    println!("sink={sink} nodes={nodes} updates={updates} iters={iters}");
}
