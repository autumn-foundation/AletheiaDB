//! Deterministic durable write-path workload for instruction-count profiling.
//!
//! An ingest session against `AletheiaDB::open(dir)` (WAL + index
//! persistence, group-commit): batches of node creates, edges between them,
//! then repeated property updates that build version history.
//!
//! ```text
//! cargo build --profile bench --example bolt_durable_write_workload
//! valgrind --tool=callgrind --callgrind-out-file=/tmp/cg.out \
//!     target/bench/examples/bolt_durable_write_workload
//! ```
//! Env: `BOLT_TXS` (default 150), `BOLT_TX_SIZE` (default 20), `BOLT_UPDATE_ROUNDS` (default 3).

use aletheiadb::AletheiaDB;
use aletheiadb::prelude::*;

fn env_usize(key: &str, default: usize) -> usize {
    std::env::var(key)
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(default)
}

fn main() {
    let txs = env_usize("BOLT_TXS", 150);
    let size = env_usize("BOLT_TX_SIZE", 20);
    let rounds = env_usize("BOLT_UPDATE_ROUNDS", 3);
    let dir = std::env::temp_dir().join(format!("bolt-durable-{}", std::process::id()));
    let db = AletheiaDB::open(&dir).expect("open durable db");

    let mut ids: Vec<NodeId> = Vec::new();
    for t in 0..txs {
        let new: Vec<NodeId> = db
            .write(|tx| -> Result<Vec<NodeId>> {
                let mut v = Vec::with_capacity(size);
                for i in 0..size {
                    let n = t * size + i;
                    v.push(
                        tx.create_node(
                            "Doc",
                            PropertyMapBuilder::new()
                                .insert("title", format!("doc-{n}"))
                                .insert("rank", n as i64)
                                .insert("topic", format!("t{}", n % 12))
                                .build(),
                        )?,
                    );
                }
                for w in v.windows(2) {
                    tx.create_edge(
                        w[0],
                        w[1],
                        "NEXT",
                        PropertyMapBuilder::new().insert("w", 1i64).build(),
                    )?;
                }
                Ok(v)
            })
            .expect("write tx");
        ids.extend(new);
    }
    for r in 0..rounds {
        for chunk in ids.chunks(size) {
            db.write(|tx| -> Result<()> {
                for id in chunk {
                    tx.update_node(
                        *id,
                        PropertyMapBuilder::new()
                            .insert("rank", (r * 7) as i64)
                            .build(),
                    )?;
                }
                Ok(())
            })
            .expect("update tx");
        }
    }
    println!("nodes={} txs={txs} rounds={rounds}", ids.len());
    drop(db);
    let _ = std::fs::remove_dir_all(&dir);
}
