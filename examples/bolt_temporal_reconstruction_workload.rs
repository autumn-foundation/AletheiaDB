//! Deterministic bi-temporal reconstruction workload for instruction-count
//! profiling.
//!
//! Exercises `AletheiaDB::get_node_at_time`'s anchor+delta reconstruction
//! path (`HistoricalStorage::reconstruct_node_properties_iterative`) -- the
//! mechanism behind CLAUDE.md's "<10ms temporal reconstruction" target and
//! the "Storage Efficiency: Anchor+delta compression" principle. No existing
//! `examples/bolt_*.rs` harness drives this path: `bolt_workload.rs`,
//! `bolt_batch_workload.rs`, and `bolt_cypher_workload.rs` all read
//! current-state (single version, no delta-chain walk); `bolt_recovery_workload.rs`
//! profiles checkpoint load + WAL replay, not query-time reconstruction.
//!
//! Builds `BOLT_NODES` records, each updated `BOLT_UPDATES_PER_NODE - 1`
//! times with `anchor_interval` set to `BOLT_ANCHOR_INTERVAL` (default 50,
//! matched to `BOLT_UPDATES_PER_NODE`'s default so each node accrues one
//! full anchor + 49-delta chain -- the worst case for chain-walk cost,
//! deliberately longer than the engine's own default `anchor_interval` of
//! 10 so the delta-chain walk isn't diluted by the write phase's cost in
//! the profile), then issues one `get_node_at_time` call per recorded
//! version -- an audit/history-browsing access pattern ("what did we know
//! about X at each point it changed?") where every point-in-time query lands
//! on a version that has never been reconstructed before, so the per-version
//! reconstruction cache (`HistoricalStorage::node_property_cache`) never
//! masks the delta-chain-walk cost. Query timestamps are read back from
//! `get_node_history`'s own recorded bi-temporal intervals rather than
//! guessed from wall-clock deltas, so each query resolves to exactly the
//! intended version regardless of clock resolution.
//!
//! This is a plain binary, not a criterion benchmark: criterion's
//! iteration/warmup loop is prohibitively slow stacked under valgrind's own
//! 20-50x slowdown. Running one fixed, deterministic pass keeps instruction
//! counts reproducible run to run.
//!
//! ```text
//! cargo build --profile bench --example bolt_temporal_reconstruction_workload
//! valgrind --tool=callgrind --callgrind-out-file=/tmp/callgrind.out \
//!     target/release/examples/bolt_temporal_reconstruction_workload
//! callgrind_annotate /tmp/callgrind.out | less
//!
//! valgrind --tool=dhat --dhat-out-file=/tmp/dhat.out \
//!     target/release/examples/bolt_temporal_reconstruction_workload
//! ```
//!
//! Env vars: `BOLT_NODES` (default 500), `BOLT_UPDATES_PER_NODE` (default 50),
//! `BOLT_ANCHOR_INTERVAL` (default 50).

use aletheiadb::config::WalConfigBuilder;
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
    let updates_per_node = env_usize("BOLT_UPDATES_PER_NODE", 50).max(1);
    let anchor_interval = env_usize("BOLT_ANCHOR_INTERVAL", 50) as u32;

    // `AletheiaDBConfig::builder().build()` defaults index persistence to a
    // fixed `./aletheiadb` directory in the current working directory (not a
    // tempdir), which would silently accumulate WAL entries and replay them
    // across runs, breaking this harness's determinism. Build the WAL config
    // against a fresh tempdir explicitly, mirroring what `AletheiaDB::new()`
    // does internally, so every run starts genuinely empty.
    let tempdir = tempfile::Builder::new()
        .prefix("bolt-temporal-reconstruction-")
        .tempdir()
        .expect("create tempdir");
    let wal_config = WalConfigBuilder::new()
        .wal_dir(tempdir.path().join("wal"))
        .build();
    let anchor_config = AnchorConfig {
        anchor_interval,
        max_delta_chain: anchor_interval,
    };
    let db = AletheiaDB::with_full_config(anchor_config, wal_config).expect("create ephemeral db");

    // --- Write phase: each node accrues a long anchor+delta version chain.
    //
    // Each round below batches every node's update into a single
    // `WriteTransaction` (one fsync under the default Synchronous durability
    // mode) rather than one transaction per node -- otherwise this phase
    // alone would issue `node_count * updates_per_node` (thousands of)
    // fsyncs and swamp both wall-clock iteration and (irrelevantly, since
    // syscalls are cheap in instruction terms, but not in profiling
    // wall-time) the profiling session with WAL durability cost this
    // benchmark isn't targeting. The resulting version chains -- one version
    // per node per round, same content -- are identical either way. ---
    let mut node_ids = Vec::with_capacity(node_count);
    db.write(|tx| {
        for i in 0..node_count {
            let props = build_props(i, 0);
            node_ids.push(tx.create_node("Record", props)?);
        }
        Ok::<_, Error>(())
    })
    .expect("create batch");

    for v in 1..updates_per_node {
        db.write(|tx| {
            for (i, &node_id) in node_ids.iter().enumerate() {
                let props = build_props(i, v);
                tx.update_node(node_id, props)?;
            }
            Ok::<_, Error>(())
        })
        .expect("update round");
    }

    // --- Query phase: one get_node_at_time call per recorded version, using
    // that version's own bi-temporal interval start as the query point so it
    // resolves to exactly that version -- never the same version_id twice,
    // so every call is a reconstruction-cache miss into the full
    // anchor+delta-chain walk. ---
    let mut sink: u64 = 0;
    let mut reconstructions: u64 = 0;
    for &node_id in &node_ids {
        let history = db.get_node_history(node_id).expect("get_node_history");
        for version in &history.versions {
            let valid_time = version.temporal.valid_time().start();
            let tx_time = version.temporal.transaction_time().start();
            let node = db
                .get_node_at_time(node_id, valid_time, tx_time)
                .expect("get_node_at_time");
            sink = sink.wrapping_add(node.properties.len() as u64);
            reconstructions += 1;
        }
    }

    println!(
        "sink={sink} nodes={node_count} updates_per_node={updates_per_node} anchor_interval={anchor_interval} reconstructions={reconstructions}"
    );
}
