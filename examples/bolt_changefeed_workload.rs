//! Deterministic change-feed (`list_changes`) workload for instruction-count profiling.
//!
//! A consumer catching up on a bi-temporal database pages through the full
//! transaction-time window with `list_changes` (limit 100, following
//! `next_cursor`), once unfiltered and once with a label filter. Plain binary
//! (not criterion) so it runs under valgrind deterministically.
//!
//! ```text
//! cargo build --release --example bolt_changefeed_workload
//! valgrind --tool=callgrind --callgrind-out-file=/tmp/cg.out \
//!     target/release/examples/bolt_changefeed_workload
//! callgrind_annotate /tmp/cg.out | less
//! ```
//!
//! Env vars: `BOLT_NODES` (default 500), `BOLT_UPDATES_PER_NODE` (default 20),
//! `BOLT_PAGE` (default 100).

use aletheiadb::config::WalConfigBuilder;
use aletheiadb::core::ChangeFeedQuery;
use aletheiadb::core::version::AnchorConfig;
use aletheiadb::prelude::*;

fn env_usize(key: &str, default: usize) -> usize {
    std::env::var(key)
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(default)
}

/// Property map for node `node_index` at version `version_index`. `name`
/// stays fixed per node (an identity field); the rest evolve every version,
/// so each update produces a real, non-empty `PropertyDelta` rather than a
/// no-op one.
fn build_props(node_index: usize, version_index: usize) -> PropertyMap {
    PropertyMapBuilder::new()
        .insert("name", format!("Record{node_index}"))
        .insert("status", format!("status_{}", version_index % 5))
        .insert("score", (node_index * 7 + version_index * 3) as i64)
        .insert("priority", (version_index % 10) as i64)
        .insert("active", version_index.is_multiple_of(2))
        .insert(
            "category",
            format!("cat_{}", (node_index + version_index) % 12),
        )
        .insert(
            "owner",
            format!("owner_{}", (node_index + version_index) % 20),
        )
        .insert(
            "region",
            format!("region_{}", (node_index * 3 + version_index) % 8),
        )
        .insert("last_updated_seq", version_index as i64)
        .insert("weight", (version_index as f64) * 1.5)
        .build()
}

fn main() {
    let node_count = env_usize("BOLT_NODES", 500);
    let updates = env_usize("BOLT_UPDATES_PER_NODE", 20).max(1);
    let page = env_usize("BOLT_PAGE", 100);
    let tempdir = tempfile::Builder::new()
        .prefix("bolt-changefeed-")
        .tempdir()
        .expect("tempdir");
    let wal_config = WalConfigBuilder::new()
        .wal_dir(tempdir.path().join("wal"))
        .build();
    let db = AletheiaDB::with_full_config(AnchorConfig::default(), wal_config).expect("db");

    let start = aletheiadb::core::temporal::time::now();
    let mut ids = Vec::with_capacity(node_count);
    db.write(|tx| {
        for i in 0..node_count {
            let label = if i % 4 == 0 { "Hot" } else { "Record" };
            ids.push(tx.create_node(label, build_props(i, 0))?);
        }
        Ok::<_, Error>(())
    })
    .expect("create");
    for v in 1..updates {
        db.write(|tx| {
            for (i, &id) in ids.iter().enumerate() {
                tx.update_node(id, build_props(i, v))?;
            }
            Ok::<_, Error>(())
        })
        .expect("update");
    }
    let end = aletheiadb::core::temporal::time::now();

    for label in [None, Some("Hot".to_string())] {
        let mut total = 0usize;
        let mut pages = 0usize;
        let mut cursor: Option<String> = None;
        loop {
            let mut q = ChangeFeedQuery::new(start, end, page);
            q.label = label.clone();
            q.cursor = cursor.take();
            let p = db.list_changes(&q).expect("list_changes");
            total += p.changes.len();
            pages += 1;
            match p.next_cursor {
                Some(c) => cursor = Some(c),
                None => break,
            }
        }
        println!("label={label:?} pages={pages} changes={total}");
    }
}
