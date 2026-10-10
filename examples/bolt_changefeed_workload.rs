//! Deterministic changefeed (`list_changes`) pagination workload for
//! instruction-count profiling.
//!
//! Drives the pull feed behind the MCP `list_changes` tool and the Parquet
//! history export: a consumer pages through the full transaction-time window
//! with a continuation cursor, `BOLT_PAGE` rows at a time.
//!
//! ```text
//! cargo build --release --example bolt_changefeed_workload
//! valgrind --tool=callgrind --callgrind-out-file=/tmp/callgrind.out \
//!     --toggle-collect=bolt_changefeed_workload::page_all \
//!     target/release/examples/bolt_changefeed_workload
//! valgrind --tool=dhat --dhat-out-file=/tmp/dhat.out \
//!     target/release/examples/bolt_changefeed_workload
//! ```
//!
//! Env vars: `BOLT_NODES` (default 200), `BOLT_UPDATES_PER_NODE` (default 10),
//! `BOLT_PAGE` (default 100).

use aletheiadb::config::WalConfigBuilder;
use aletheiadb::core::temporal::TIMESTAMP_MAX;
use aletheiadb::prelude::*;
use aletheiadb::{ChangeFeedQuery, Timestamp};

fn env_usize(key: &str, default: usize) -> usize {
    std::env::var(key)
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(default)
}

fn build_props(i: usize, v: usize) -> PropertyMap {
    PropertyMapBuilder::new()
        .insert("name", format!("Record{i}"))
        .insert("status", format!("status_{}", v % 5))
        .insert("score", (i * 7 + v * 3) as i64)
        .insert("active", v.is_multiple_of(2))
        .build()
}

/// Isolated so callgrind can count the paging phase alone.
#[inline(never)]
fn page_all(db: &AletheiaDB, page: usize) -> (u64, u64) {
    let mut rows = 0u64;
    let mut pages = 0u64;
    let mut cursor: Option<String> = None;
    loop {
        let mut q = ChangeFeedQuery::new(Timestamp::from(0), TIMESTAMP_MAX, page);
        q.cursor = cursor.take();
        let p = db.list_changes(&q).expect("list_changes");
        rows += p.changes.len() as u64;
        pages += 1;
        match p.next_cursor {
            Some(c) => cursor = Some(c),
            None => break,
        }
    }
    (rows, pages)
}

fn main() {
    let nodes = env_usize("BOLT_NODES", 200);
    let updates = env_usize("BOLT_UPDATES_PER_NODE", 10).max(1);
    let page = env_usize("BOLT_PAGE", 100);

    let tempdir = tempfile::Builder::new()
        .prefix("bolt-changefeed-")
        .tempdir()
        .expect("tempdir");
    let wal = WalConfigBuilder::new()
        .wal_dir(tempdir.path().join("wal"))
        .build();
    let db = AletheiaDB::with_full_config(Default::default(), wal).expect("db");

    let mut ids = Vec::with_capacity(nodes);
    db.write(|tx| {
        for i in 0..nodes {
            ids.push(tx.create_node("Record", build_props(i, 0))?);
        }
        Ok::<_, Error>(())
    })
    .expect("create");
    for v in 1..=updates {
        db.write(|tx| {
            for (i, &id) in ids.iter().enumerate() {
                tx.update_node(id, build_props(i, v))?;
            }
            Ok::<_, Error>(())
        })
        .expect("update");
    }

    let (rows, pages) = page_all(&db, page);
    println!("rows={rows} pages={pages} nodes={nodes} updates={updates} page={page}");
}
