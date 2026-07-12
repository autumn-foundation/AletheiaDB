//! Integration tests for the tamper-evident provenance hash chain database
//! layer (Issue #3351). Each test maps to an acceptance criterion (AC1..AC7).

use std::path::Path;
use std::time::Duration;

use aletheiadb::config::{AletheiaDBConfig, WalConfigBuilder};
use aletheiadb::provenance_chain::{ChainConfig, ChainFsyncMode, ChainHead, EntityKind};
use aletheiadb::{AletheiaDB, DurabilityMode, PersistenceConfig, PropertyMapBuilder};

/// Build a config with the provenance chain enabled, WAL + index persistence
/// durable, rooted at `data_dir`. The chain lives at `<data_dir>/chain`.
fn enabled_config(data_dir: &Path) -> AletheiaDBConfig {
    AletheiaDBConfig::builder()
        .wal(
            WalConfigBuilder::new()
                .wal_dir(data_dir.join("wal"))
                .durability_mode(DurabilityMode::Synchronous)
                .build(),
        )
        .persistence(PersistenceConfig {
            enabled: true,
            data_dir: data_dir.join("indexes"),
            load_on_startup: true,
            ..Default::default()
        })
        .chain(ChainConfig {
            enabled: true,
            fsync: ChainFsyncMode::PerTransaction,
            dir: None,
        })
        .build()
}

fn props(name: &str) -> aletheiadb::PropertyMap {
    PropertyMapBuilder::new().insert("name", name).build()
}

/// Wait for the async sealer to reach at least `expected` sealed transactions.
fn wait_for_head_seq(db: &AletheiaDB, expected: u64) {
    for _ in 0..300 {
        if db.export_chain_head().expect("chain enabled").seq >= expected {
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    panic!(
        "chain head did not reach seq {expected} (stuck at {})",
        db.export_chain_head().unwrap().seq
    );
}

// ---------------------------------------------------------------------------
// AC1: every committed transaction advances the chain head by exactly one seq
// and changes the head digest; a no-op/read-only transaction adds no record.
// ---------------------------------------------------------------------------
#[test]
fn ac1_head_advances_one_seq_per_transaction() {
    let dir = tempfile::tempdir().unwrap();
    let db = AletheiaDB::with_unified_config(enabled_config(dir.path())).unwrap();

    assert_eq!(db.export_chain_head().unwrap().seq, 0, "starts at genesis");
    let genesis_digest = db.export_chain_head().unwrap().digest;

    // Tx 1: a node.
    let alice = db.create_node("Person", props("Alice")).unwrap();
    wait_for_head_seq(&db, 1);
    let h1 = db.export_chain_head().unwrap();
    assert_eq!(h1.seq, 1);
    assert_ne!(h1.digest, genesis_digest, "head digest advances");

    // Tx 2: another node.
    let bob = db.create_node("Person", props("Bob")).unwrap();
    wait_for_head_seq(&db, 2);
    let h2 = db.export_chain_head().unwrap();
    assert_eq!(h2.seq, 2);
    assert_ne!(h2.digest, h1.digest, "each tx changes the head digest");

    // Tx 3: an edge.
    db.create_edge(alice, bob, "KNOWS", props("")).unwrap();
    wait_for_head_seq(&db, 3);
    assert_eq!(db.export_chain_head().unwrap().seq, 3);

    // Tx 4: a multi-op batch (two nodes + an edge) — still ONE record.
    db.write(|tx| {
        use aletheiadb::api::transaction::WriteOps;
        let c = tx.create_node("Person", props("Carol"))?;
        let d = tx.create_node("Person", props("Dave"))?;
        tx.create_edge(c, d, "KNOWS", props(""))?;
        Ok::<_, aletheiadb::Error>(())
    })
    .unwrap();
    wait_for_head_seq(&db, 4);
    assert_eq!(
        db.export_chain_head().unwrap().seq,
        4,
        "a multi-op batch seals as a single transaction record"
    );

    // A read-only op and an empty write add NO record.
    let _ = db.get_node(alice).unwrap();
    db.write(|_tx| Ok::<_, aletheiadb::Error>(())).unwrap();
    std::thread::sleep(Duration::from_millis(50));
    assert_eq!(
        db.export_chain_head().unwrap().seq,
        4,
        "read-only / empty transactions do not add chain records"
    );
}

// ---------------------------------------------------------------------------
// AC2: full-chain and entity-scoped verification pass on a clean database.
// ---------------------------------------------------------------------------
#[test]
fn ac2_full_and_entity_verification_pass_clean() {
    let dir = tempfile::tempdir().unwrap();
    let db = AletheiaDB::with_unified_config(enabled_config(dir.path())).unwrap();

    let alice = db.create_node("Person", props("Alice")).unwrap();
    let _bob = db.create_node("Person", props("Bob")).unwrap();
    // A second write to `alice` so it appears in two transactions.
    db.write(|tx| {
        use aletheiadb::api::transaction::WriteOps;
        tx.update_node(alice, props("Alice2"))?;
        Ok::<_, aletheiadb::Error>(())
    })
    .unwrap();
    wait_for_head_seq(&db, 3);

    let full = db.verify_chain().unwrap();
    assert!(full.passed, "clean full verify: {:?}", full.reason);
    assert_eq!(full.transactions_checked, 3);

    // Entity-scoped: alice appears in tx 1 and tx 3 only — verification touches
    // exactly those two records, not all three.
    let ev = db
        .verify_entity_chain(EntityKind::Node, alice.as_u64())
        .unwrap();
    assert!(ev.passed, "clean entity verify: {:?}", ev.reason);
    assert_eq!(
        ev.transactions_checked, 2,
        "entity verify is scoped to the entity's records"
    );
}

// ---------------------------------------------------------------------------
// AC3 (headline): a persisted sealed record no longer matching stored history
// is caught and localized.
//
// Tamper mechanism: after sealing, we flip one byte of a persisted sealed leaf
// in `chain.log`, then reopen. Verify recomputes that leaf from stored history
// and finds it disagrees with the persisted sealed leaf — passed = false, with
// `earliest_broken_seq` localizing the exact transaction. This exercises the
// identical recompute-vs-sealed comparison that catches a tampered *data*
// version (the dual): AletheiaDB's WAL/index stores checksum a raw data-byte
// flip, and storage internals are out of this lane's scope (no in-process API
// to mutate a stored historical version), so per the design's documented
// fallback we tamper the persisted verifiable state and assert the chain both
// detects and localizes the divergence.
// ---------------------------------------------------------------------------
#[test]
fn ac3_tamper_is_detected_and_localized() {
    let dir = tempfile::tempdir().unwrap();
    {
        let db = AletheiaDB::with_unified_config(enabled_config(dir.path())).unwrap();
        db.create_node("Person", props("Alice")).unwrap();
        db.create_node("Person", props("Bob")).unwrap();
        db.create_node("Person", props("Carol")).unwrap();
        wait_for_head_seq(&db, 3);
        assert!(db.verify_chain().unwrap().passed);
        // db drops here: sealer flushes + head checkpoint persisted.
    }

    // Flip one byte inside the first sealed record's first leaf. Layout of
    // chain.log: header(12) + [frame_len(4) + payload]; payload =
    // seq(8) commit_ts(8) commit_ts_logical(4) tx_id(8) anchor_lsn(8)
    // n_leaves(8) leaves(32*n)..., so the first leaf begins at offset
    // 12 + 4 + (8+8+4+8+8+8) = 60.
    let log_path = dir.path().join("chain").join("chain.log");
    let mut bytes = std::fs::read(&log_path).unwrap();
    assert!(bytes.len() > 60, "chain log unexpectedly small");
    bytes[60] ^= 0xFF;
    std::fs::write(&log_path, &bytes).unwrap();

    // Reopen and verify: the tampered sealed leaf no longer matches history.
    let db = AletheiaDB::with_unified_config(enabled_config(dir.path())).unwrap();
    let v = db.verify_chain().unwrap();
    assert!(!v.passed, "tamper must be detected");
    assert_eq!(
        v.earliest_broken_seq,
        Some(1),
        "tamper is localized to the affected transaction"
    );
}

// ---------------------------------------------------------------------------
// AC4: an exported head anchors append-only extension; a truncated/divergent
// anchor is rejected.
// ---------------------------------------------------------------------------
#[test]
fn ac4_anchor_proves_extension_and_detects_rollback_fork() {
    let dir = tempfile::tempdir().unwrap();
    let db = AletheiaDB::with_unified_config(enabled_config(dir.path())).unwrap();

    db.create_node("Person", props("Alice")).unwrap();
    db.create_node("Person", props("Bob")).unwrap();
    wait_for_head_seq(&db, 2);
    let anchor = db.export_chain_head().unwrap();
    assert_eq!(anchor.seq, 2);

    // Extend the chain, then prove it append-only-extends the earlier anchor.
    db.create_node("Person", props("Carol")).unwrap();
    wait_for_head_seq(&db, 3);
    let ext = db.verify_chain_against(&anchor).unwrap();
    assert!(
        ext.passed,
        "extension of an anchor must verify: {:?}",
        ext.reason
    );

    // Rollback: an anchor at a seq the (truncated) chain no longer has.
    let rollback_anchor = ChainHead {
        seq: 99,
        digest: [0x11u8; 32],
        commit_ts: anchor.commit_ts,
        anchor_lsn: anchor.anchor_lsn,
        genesis_digest: anchor.genesis_digest,
    };
    let rb = db.verify_chain_against(&rollback_anchor).unwrap();
    assert!(!rb.passed, "a rollback anchor must fail");
    assert!(rb.reason.unwrap().contains("rollback"));

    // Fork: an anchor at an existing seq but with a divergent digest.
    let fork_anchor = ChainHead {
        seq: 2,
        digest: [0xABu8; 32],
        commit_ts: anchor.commit_ts,
        anchor_lsn: anchor.anchor_lsn,
        genesis_digest: anchor.genesis_digest,
    };
    let fk = db.verify_chain_against(&fork_anchor).unwrap();
    assert!(!fk.passed, "a divergent anchor must fail");
    assert!(fk.reason.unwrap().contains("fork"));
}

// ---------------------------------------------------------------------------
// AC5: with the chain DISABLED (the default), no chain files are created, the
// verify API reports "not enabled", stats.chain.enabled is false, and writes
// behave identically.
// ---------------------------------------------------------------------------
#[test]
fn ac5_disabled_is_zero_behavior_change() {
    let dir = tempfile::tempdir().unwrap();
    // Durable config WITHOUT enabling the chain (chain defaults to disabled).
    let config = AletheiaDBConfig::builder()
        .wal(
            WalConfigBuilder::new()
                .wal_dir(dir.path().join("wal"))
                .durability_mode(DurabilityMode::Synchronous)
                .build(),
        )
        .persistence(PersistenceConfig {
            enabled: true,
            data_dir: dir.path().join("indexes"),
            load_on_startup: true,
            ..Default::default()
        })
        .build();
    assert!(!config.chain.enabled, "chain disabled by default");

    let db = AletheiaDB::with_unified_config(config).unwrap();

    // Writes succeed identically.
    let alice = db.create_node("Person", props("Alice")).unwrap();
    let node = db.get_node(alice).unwrap();
    assert_eq!(
        node.properties.get("name").and_then(|v| v.as_str()),
        Some("Alice")
    );

    // No chain directory or files were created.
    assert!(
        !dir.path().join("chain").exists(),
        "no chain dir when disabled"
    );

    // stats reports the chain disabled.
    let stats = db.stats();
    assert!(!stats.chain.enabled);
    assert!(stats.chain.head_seq.is_none());
    assert!(stats.chain.head_digest.is_none());

    // The verify API returns a clear "not enabled" error (proving the field is
    // None).
    assert!(db.verify_chain().is_err());
    assert!(db.export_chain_head().is_err());
}

// ---------------------------------------------------------------------------
// AC6: after an unclean stop (the chain sidecar lost while durable history
// survives), reopening rebuilds the tail from replayed history and verifies.
// ---------------------------------------------------------------------------
#[test]
fn ac6_crash_recovery_rebuilds_tail() {
    let dir = tempfile::tempdir().unwrap();
    let node_ids;
    {
        let db = AletheiaDB::with_unified_config(enabled_config(dir.path())).unwrap();
        let a = db.create_node("Person", props("Alice")).unwrap();
        let b = db.create_node("Person", props("Bob")).unwrap();
        db.create_edge(a, b, "KNOWS", props("")).unwrap();
        node_ids = (a, b);
        wait_for_head_seq(&db, 3);
        assert!(db.verify_chain().unwrap().passed);
        // db drops here.
    }

    // Simulate an unclean stop: the durable WAL/index survive, but the chain
    // sidecar never made it to disk (delete it entirely). On reopen the chain
    // must re-derive its records from replayed history.
    std::fs::remove_dir_all(dir.path().join("chain")).unwrap();
    assert!(!dir.path().join("chain").exists());

    let db = AletheiaDB::with_unified_config(enabled_config(dir.path())).unwrap();

    // Data survived recovery.
    assert_eq!(
        db.get_node(node_ids.0)
            .unwrap()
            .properties
            .get("name")
            .and_then(|v| v.as_str()),
        Some("Alice")
    );

    // The chain was rebuilt from history and verifies over the recovered
    // prefix (3 transactions -> 3 records).
    let head = db.export_chain_head().unwrap();
    assert_eq!(head.seq, 3, "chain rebuilt one record per recovered tx");
    let v = db.verify_chain().unwrap();
    assert!(v.passed, "rebuilt chain must verify: {:?}", v.reason);
    assert_eq!(v.transactions_checked, 3);
}

// ---------------------------------------------------------------------------
// Finding 4 (Issue #3351): the rebuilt-from-history chain must reproduce the
// EXACT pre-crash head digest, so an anchor exported before the crash still
// verifies as an append-only extension after rebuild. This fails without the
// deterministic commit-ordered digest (tx_id removed from the fold; full HLC
// bound; records folded in commit-timestamp order in both live and rebuild
// paths).
//
// Crash model: the head checkpoint (which carries the immutable genesis digest)
// survives, but the sealed log tail is lost — forcing a full tail rebuild on
// reopen. The genesis cannot be reproduced from scratch (it is seeded from the
// open-time LSN, which differs after replay), so preserving the checkpoint's
// genesis is exactly what a real durable chain does.
// ---------------------------------------------------------------------------
#[test]
fn finding4_rebuilt_chain_matches_precrash_anchor() {
    let dir = tempfile::tempdir().unwrap();
    let anchor;
    {
        let db = AletheiaDB::with_unified_config(enabled_config(dir.path())).unwrap();
        let a = db.create_node("Person", props("Alice")).unwrap();
        let b = db.create_node("Person", props("Bob")).unwrap();
        db.create_edge(a, b, "KNOWS", props("")).unwrap();
        db.create_node("Person", props("Carol")).unwrap();
        wait_for_head_seq(&db, 4);
        anchor = db.export_chain_head().unwrap();
        assert_eq!(anchor.seq, 4);
        // db drops here: log + head checkpoint flushed.
    }

    // Simulate the lost sealed tail: truncate chain.log back to just its 12-byte
    // header (dropping every sealed record) while KEEPING the head checkpoint so
    // the genesis digest survives. On reopen the chain loads zero records
    // (head == genesis) and rebuilds all four transactions from replayed history.
    let log_path = dir.path().join("chain").join("chain.log");
    let bytes = std::fs::read(&log_path).unwrap();
    std::fs::write(&log_path, &bytes[..12]).unwrap();

    let db = AletheiaDB::with_unified_config(enabled_config(dir.path())).unwrap();
    let head = db.export_chain_head().unwrap();
    assert_eq!(head.seq, 4, "tail rebuilt one record per recovered tx");

    // The rebuilt chain reproduces the pre-crash digest exactly: the pre-crash
    // anchor verifies as an append-only extension (no false fork).
    assert_eq!(
        head.digest, anchor.digest,
        "rebuilt head digest must equal the pre-crash head digest"
    );
    let v = db.verify_chain_against(&anchor).unwrap();
    assert!(
        v.passed,
        "pre-crash anchor must verify against the rebuilt chain: {:?}",
        v.reason
    );

    // And full verification still passes over the rebuilt records.
    assert!(db.verify_chain().unwrap().passed);
}

// ---------------------------------------------------------------------------
// Finding 4 / AC6 (mid-workload crash): a *genuine* mid-workload crash leaves a
// SEALED PREFIX on disk plus an UNSEALED TAIL (transactions committed to the WAL
// but not yet sealed). On reopen the chain loads the durable prefix and rebuilds
// only the tail from replayed history — and the recombined chain must reproduce
// the exact pre-crash head digest, so a pre-crash anchor still verifies and full
// verify passes over the whole recovered prefix.
//
// This is stronger than `finding4_rebuilt_chain_matches_precrash_anchor` (which
// drops the ENTIRE log — an empty prefix). Here we keep the first two sealed
// records and drop the rest, exercising the load-prefix + rebuild-tail merge.
// ---------------------------------------------------------------------------

/// Return the byte offset in a `chain.log` just past the first `keep` sealed
/// record frames. Layout: 12-byte header, then repeated `[u32 BE len][len bytes]`.
fn offset_after_records(bytes: &[u8], keep: usize) -> usize {
    let mut pos = 12usize; // header
    for _ in 0..keep {
        assert!(pos + 4 <= bytes.len(), "log shorter than requested prefix");
        let len = u32::from_be_bytes([bytes[pos], bytes[pos + 1], bytes[pos + 2], bytes[pos + 3]])
            as usize;
        pos += 4 + len;
        assert!(pos <= bytes.len(), "frame runs past end of log");
    }
    pos
}

#[test]
fn finding4_partial_tail_rebuild_matches_precrash_anchor() {
    let dir = tempfile::tempdir().unwrap();
    let anchor;
    {
        let db = AletheiaDB::with_unified_config(enabled_config(dir.path())).unwrap();
        let a = db.create_node("Person", props("Alice")).unwrap();
        let b = db.create_node("Person", props("Bob")).unwrap();
        db.create_edge(a, b, "KNOWS", props("")).unwrap();
        db.create_node("Person", props("Carol")).unwrap();
        wait_for_head_seq(&db, 4);
        anchor = db.export_chain_head().unwrap();
        assert_eq!(anchor.seq, 4);
        // db drops here: all four records + head checkpoint flushed.
    }

    // Simulate a mid-workload crash: the sealer had durably sealed the first TWO
    // transactions, but the last two records never reached the sidecar (their
    // commits are in the WAL/index and will be replayed). Keep the head
    // checkpoint so the genesis digest survives, matching a real durable chain.
    let log_path = dir.path().join("chain").join("chain.log");
    let bytes = std::fs::read(&log_path).unwrap();
    let cut = offset_after_records(&bytes, 2);
    assert!(cut < bytes.len(), "expected an unsealed tail to drop");
    std::fs::write(&log_path, &bytes[..cut]).unwrap();

    // Reopen: loads the 2-record prefix (head at seq 2) and rebuilds txns 3..4
    // from replayed history.
    let db = AletheiaDB::with_unified_config(enabled_config(dir.path())).unwrap();
    let head = db.export_chain_head().unwrap();
    assert_eq!(
        head.seq, 4,
        "prefix loaded + tail rebuilt to the full 4 txns"
    );

    // The merged (loaded prefix + rebuilt tail) chain reproduces the pre-crash
    // head digest exactly — no false fork.
    assert_eq!(
        head.digest, anchor.digest,
        "recovered head digest must equal the pre-crash head digest"
    );
    let v = db.verify_chain_against(&anchor).unwrap();
    assert!(
        v.passed,
        "pre-crash anchor must verify against the recovered chain: {:?}",
        v.reason
    );

    // Full verify passes over the whole recovered prefix.
    let full = db.verify_chain().unwrap();
    assert!(
        full.passed,
        "full verify over recovered chain: {:?}",
        full.reason
    );
    assert_eq!(full.transactions_checked, 4);
}

// ---------------------------------------------------------------------------
// AC7: database_stats surfaces chain status (enabled, head, genesis, and the
// last verification result once a verify has run).
// ---------------------------------------------------------------------------
#[test]
fn ac7_stats_reports_chain_status() {
    let dir = tempfile::tempdir().unwrap();
    let db = AletheiaDB::with_unified_config(enabled_config(dir.path())).unwrap();

    db.create_node("Person", props("Alice")).unwrap();
    wait_for_head_seq(&db, 1);

    let stats = db.stats();
    assert!(stats.chain.enabled);
    assert_eq!(stats.chain.head_seq, Some(1));
    assert!(stats.chain.head_digest.is_some());
    assert!(stats.chain.genesis_digest.is_some());
    assert!(stats.chain.last_verified.is_none(), "no verify has run yet");

    // After a verify, stats carries the cached result.
    assert!(db.verify_chain().unwrap().passed);
    let stats = db.stats();
    let lv = stats.chain.last_verified.expect("verify recorded");
    assert!(lv.passed);
    assert!(lv.at_micros > 0);
}
