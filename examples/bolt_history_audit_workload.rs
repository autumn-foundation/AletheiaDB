//! Deterministic version-history audit workload for instruction-count
//! profiling.
//!
//! Drives the engine behind the MCP `get_node_history`, `diff_node_versions`
//! and logical-version reads (`get_node_at_version`) -- the "how has Y
//! changed?" pattern from CLAUDE.md's LLM-integration section. The existing
//! `bolt_temporal_reconstruction_workload.rs` profiles `get_node_at_time`;
//! none of the harnesses profile the history/diff surface itself.
//!
//! Each node accrues `BOLT_UPDATES_PER_NODE` versions under the engine's
//! default anchor config, then the audit phase issues, per node, one
//! `get_node_history`, one `diff_node_versions` between every pair of
//! consecutive versions, and one `get_node_at_version` for each version.
//!
//! ```text
//! cargo build --release --example bolt_history_audit_workload
//! valgrind --tool=callgrind --callgrind-out-file=/tmp/callgrind.out \
//!     target/release/examples/bolt_history_audit_workload
//! callgrind_annotate /tmp/callgrind.out | less
//!
//! valgrind --tool=dhat --dhat-out-file=/tmp/dhat.out \
//!     target/release/examples/bolt_history_audit_workload
//! ```
//!
//! Audit-phase-only count: add
//! `--toggle-collect=bolt_history_audit_workload::audit` to the callgrind
//! command.
//!
//! Env vars: `BOLT_NODES` (default 100), `BOLT_UPDATES_PER_NODE` (default 40).

use aletheiadb::config::WalConfigBuilder;
use aletheiadb::prelude::*;

fn env_usize(key: &str, default: usize) -> usize {
    std::env::var(key)
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(default)
}

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
        .insert("last_updated_seq", version_index as i64)
        .insert("weight", (version_index as f64) * 1.5)
        .build()
}

/// The audit phase, isolated so callgrind can count it alone (the write
/// phase that builds the version chains is setup, not the workload under
/// test): `--toggle-collect=bolt_history_audit_workload::audit`.
#[inline(never)]
fn audit(db: &AletheiaDB, node_ids: &[NodeId]) -> (u64, u64) {
    let mut sink: u64 = 0;
    let mut ops: u64 = 0;
    for &id in node_ids {
        let history = db.get_node_history(id).expect("history");
        ops += 1;
        for pair in history.versions.windows(2) {
            let d = db
                .diff_node_versions(id, pair[0].version_id, pair[1].version_id)
                .expect("diff");
            sink = sink.wrapping_add(d.change_count() as u64);
            ops += 1;
        }
        for v in 1..=history.versions.len() as u64 {
            let n = db.get_node_at_version(id, v).expect("at_version");
            sink = sink.wrapping_add(n.properties.len() as u64);
            ops += 1;
        }
    }
    (sink, ops)
}

fn main() {
    let node_count = env_usize("BOLT_NODES", 100);
    let updates = env_usize("BOLT_UPDATES_PER_NODE", 40).max(2);

    // Fresh tempdir so every run starts empty (see
    // `bolt_temporal_reconstruction_workload.rs` for why).
    let tempdir = tempfile::Builder::new()
        .prefix("bolt-history-audit-")
        .tempdir()
        .expect("create tempdir");
    let wal_config = WalConfigBuilder::new()
        .wal_dir(tempdir.path().join("wal"))
        .build();
    let db = AletheiaDB::with_full_config(Default::default(), wal_config).expect("create db");

    let mut node_ids = Vec::with_capacity(node_count);
    db.write(|tx| {
        for i in 0..node_count {
            node_ids.push(tx.create_node("Record", build_props(i, 0))?);
        }
        Ok::<_, Error>(())
    })
    .expect("create batch");
    for v in 1..updates {
        db.write(|tx| {
            for (i, &id) in node_ids.iter().enumerate() {
                tx.update_node(id, build_props(i, v))?;
            }
            Ok::<_, Error>(())
        })
        .expect("update round");
    }

    let (sink, ops) = audit(&db, &node_ids);
    println!("sink={sink} nodes={node_count} updates={updates} ops={ops}");
}
