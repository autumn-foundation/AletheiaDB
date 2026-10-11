//! Deterministic changefeed pull (`list_changes`) workload for instruction-count profiling.
//!
//! Models a consumer draining the full feed in pages (the "what changed?"
//! pattern). Setup writes `BOLT_NODES` nodes updated `BOLT_UPDATES` times plus
//! edges; the measured phase pages through the whole window with
//! `BOLT_PAGE` rows per page.
//!
//! ```text
//! cargo build --profile bench --example bolt_changefeed_pull_workload
//! valgrind --tool=callgrind --toggle-collect=bolt_changefeed_pull_workload::drain \
//!     --callgrind-out-file=/tmp/cg.out target/release/examples/bolt_changefeed_pull_workload
//! valgrind --tool=dhat --dhat-out-file=/tmp/dhat.out target/release/examples/bolt_changefeed_pull_workload
//! ```

use aletheiadb::config::WalConfigBuilder;
use aletheiadb::core::changefeed::ChangeFeedQuery;
use aletheiadb::prelude::*;

fn env_usize(key: &str, default: usize) -> usize {
    std::env::var(key)
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(default)
}

fn props(i: usize, v: usize) -> PropertyMap {
    PropertyMapBuilder::new()
        .insert("name", format!("Record{i}"))
        .insert("status", format!("status_{}", v % 5))
        .insert("score", (i * 7 + v * 3) as i64)
        .insert("active", v.is_multiple_of(2))
        .build()
}

#[inline(never)]
fn drain(db: &AletheiaDB, page: usize) -> (u64, u64) {
    let mut total = 0u64;
    let mut pages = 0u64;
    let mut q = ChangeFeedQuery::new(
        Timestamp::new(0, 0).expect("ts"),
        Timestamp::new(i64::MAX / 2, 0).expect("ts"),
        page,
    );
    loop {
        let p = db.list_changes(&q).expect("list_changes");
        total += p.changes.len() as u64;
        pages += 1;
        match p.next_cursor {
            Some(c) => q.cursor = Some(c),
            None => break,
        }
    }
    (total, pages)
}

fn main() {
    let nodes = env_usize("BOLT_NODES", 200);
    let updates = env_usize("BOLT_UPDATES", 10).max(1);
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
            ids.push(tx.create_node("Record", props(i, 0))?);
        }
        for w in ids.windows(2) {
            tx.create_edge(w[0], w[1], "NEXT", PropertyMap::new())?;
        }
        Ok::<_, Error>(())
    })
    .expect("create");
    for v in 1..=updates {
        db.write(|tx| {
            for (i, &id) in ids.iter().enumerate() {
                tx.update_node(id, props(i, v))?;
            }
            Ok::<_, Error>(())
        })
        .expect("update");
    }
    let (total, pages) = drain(&db, page);
    println!("changes={total} pages={pages} nodes={nodes} updates={updates} page={page}");
}
