//! Deterministic workload profiling `AletheiaDB::backup` + `AletheiaDB::restore`
//! (`src/db/backup.rs`) over a bi-temporal graph with version history.
//!
//! ```text
//! cargo build --release --example bolt_backup_workload
//! valgrind --tool=callgrind --callgrind-out-file=/tmp/cg.out \
//!     target/release/examples/bolt_backup_workload
//! callgrind_annotate /tmp/cg.out | head -40
//! ```
//!
//! Env vars: `BOLT_NODES` (default 3000), `BOLT_REVISIONS` (default 3).

use aletheiadb::prelude::*;

fn env_usize(key: &str, default: usize) -> usize {
    std::env::var(key)
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(default)
}

fn main() {
    let n = env_usize("BOLT_NODES", 3_000);
    let revs = env_usize("BOLT_REVISIONS", 3);
    let db = AletheiaDB::new().expect("db");
    let mut ids = Vec::with_capacity(n);
    for i in 0..n {
        let props = PropertyMapBuilder::new()
            .insert("name", format!("Entity{i}"))
            .insert("age", (i % 100) as i64)
            .insert("category", format!("cat_{}", i % 10))
            .build();
        ids.push(db.create_node("Person", props).expect("create"));
    }
    for i in 1..n {
        db.create_edge(
            ids[i - 1],
            ids[i],
            "KNOWS",
            PropertyMapBuilder::new().build(),
        )
        .expect("edge");
    }
    for r in 0..revs {
        for (i, id) in ids.iter().enumerate() {
            let props = PropertyMapBuilder::new()
                .insert("name", format!("Entity{i}-r{r}"))
                .insert("age", ((i + r) % 100) as i64)
                .insert("category", format!("cat_{}", (i + r) % 10))
                .build();
            db.write(|tx| {
                use aletheiadb::api::transaction::WriteOps;
                tx.update_node(*id, props)
            })
            .expect("update");
        }
    }
    let dir = std::env::temp_dir().join(format!("bolt_backup_{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("dir");
    let path = dir.join("b.albk");
    let summary = db.backup(&path).expect("backup");
    let restored = AletheiaDB::restore(&path).expect("restore");
    println!(
        "{summary:?} restored_nodes={}",
        restored.stats().current.node_count
    );
    let _ = std::fs::remove_dir_all(&dir);
}
