#![cfg(test)]

//! Issue #3433: WAL replay tolerates a crash-torn trailing entry.
//!
//! Before #3433, WAL replay hard-errored on any torn-tail shape other than a
//! truncated payload (#3413) — a fully-written 24-byte entry header followed by
//! an unwritten/garbage operation-type byte propagated `CorruptedData` out of
//! replay and bricked `AletheiaDB::open` / `with_unified_config`, even though
//! every prior entry was intact and the torn entry was never acknowledged.
//!
//! These tests drive REAL on-disk WAL replay: write through a durable
//! `AletheiaDB`, drop the handle (flush), DELETE the index snapshot so the
//! reopen must replay the WAL from the beginning, corrupt the tail of the final
//! segment, then reopen and assert recovery behavior. Data reappearing after
//! the snapshot deletion is proof replay ran.

use aletheiadb::api::WriteOps;
use aletheiadb::config::WalConfigBuilder;
use aletheiadb::storage::index_persistence::PersistenceConfig;
use aletheiadb::storage::wal::DurabilityMode;
use aletheiadb::{AletheiaDB, AletheiaDBConfig, PropertyMapBuilder};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use tempfile::tempdir;

/// Delete the index-persistence snapshot so the next `open` must replay the WAL
/// from LSN 1 instead of loading current state from the snapshot.
fn force_full_replay(data_dir: &Path) {
    let indexes = data_dir.join("indexes");
    if indexes.exists() {
        fs::remove_dir_all(&indexes).expect("remove index snapshot");
    }
}

/// The highest-id, non-empty `*.log` segment under `{data_dir}/wal`.
fn last_wal_segment(wal_dir: &Path) -> PathBuf {
    let mut segs: Vec<(u64, PathBuf)> = fs::read_dir(wal_dir)
        .expect("read wal dir")
        .flatten()
        .filter_map(|e| {
            let p = e.path();
            let id = p.file_stem()?.to_str()?.parse::<u64>().ok()?;
            if p.extension().and_then(|s| s.to_str()) == Some("log")
                && fs::metadata(&p).map(|m| m.len() > 0).unwrap_or(false)
            {
                Some((id, p))
            } else {
                None
            }
        })
        .collect();
    segs.sort_by_key(|(id, _)| *id);
    segs.pop().expect("a non-empty WAL segment").1
}

/// Append a crash-torn trailing entry: a plausible, fully-written 24-byte entry
/// header (nonzero LSN + valid timestamp + garbage checksum) followed by
/// operation-type byte 0 — undecodable, non-truncated, non-zeroed. This is the
/// exact CI-observed shape ("Unknown WAL operation type: 0").
fn append_torn_optype_entry(segment: &Path) {
    let mut torn = Vec::new();
    torn.extend_from_slice(&999u64.to_le_bytes()); // LSN (nonzero)
    torn.extend_from_slice(&[1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]); // timestamp
    torn.extend_from_slice(&[0xAB; 4]); // checksum (garbage)
    torn.push(0); // op-type 0 => "Unknown WAL operation type: 0"
    let mut f = fs::OpenOptions::new().append(true).open(segment).unwrap();
    f.write_all(&torn).unwrap();
    f.sync_all().unwrap();
}

/// Commit `labels.len()` nodes as one transaction; return their ids.
fn commit(db: &AletheiaDB, labels: &[&str]) -> Vec<aletheiadb::core::NodeId> {
    db.write(|tx| {
        let mut ids = Vec::new();
        for l in labels {
            ids.push(tx.create_node(l, PropertyMapBuilder::new().build())?);
        }
        Ok::<_, aletheiadb::Error>(ids)
    })
    .expect("commit")
}

/// Durable config with a specific torn-tail recovery policy and index
/// persistence DISABLED (so replay always runs — no snapshot short-circuit).
fn config(wal_dir: &Path, tolerate_torn_tail: bool) -> AletheiaDBConfig {
    AletheiaDBConfig::builder()
        .wal(
            WalConfigBuilder::new()
                .wal_dir(wal_dir.to_path_buf())
                .durability_mode(DurabilityMode::Synchronous)
                .tolerate_torn_tail(tolerate_torn_tail)
                .build(),
        )
        .persistence(PersistenceConfig {
            enabled: false,
            ..Default::default()
        })
        .build()
}

/// #3433 (default policy): a crash-torn trailing entry (zeroed op-type) in the
/// final segment must NOT brick startup. Reopen succeeds, all prior committed
/// nodes are recovered via replay, and the torn entry is simply dropped.
#[test]
fn open_recovers_from_zeroed_optype_torn_tail() {
    let dir = tempdir().unwrap();
    let data_dir = dir.path().join("db");

    let ids = {
        let db = AletheiaDB::open(&data_dir).unwrap();
        commit(&db, &["Alice", "Bob", "Carol"])
    };

    force_full_replay(&data_dir);
    append_torn_optype_entry(&last_wal_segment(&data_dir.join("wal")));

    // Pre-#3433 this returned Err(CorruptedData("Unknown WAL operation type: 0")).
    let db2 = AletheiaDB::open(&data_dir)
        .expect("reopen must tolerate a crash-torn WAL tail, not brick startup");

    for id in &ids {
        assert!(
            db2.get_node(*id).is_ok(),
            "committed node {:?} must be recovered via replay past the torn tail",
            id
        );
    }
    assert_eq!(db2.node_count(), ids.len(), "exactly the committed nodes");
}

/// #3433 operator opt-out (c): with `tolerate_torn_tail = false`, the same torn
/// tail hard-errors — fail-stop recovery for operators who prefer manual
/// inspection over automatic tail truncation.
#[test]
fn open_fail_stop_errors_on_torn_tail_when_opted_out() {
    let dir = tempdir().unwrap();
    let wal_dir = dir.path().join("wal");

    let _ids = {
        let db = AletheiaDB::with_unified_config(config(&wal_dir, false)).unwrap();
        commit(&db, &["Alice", "Bob"])
    };

    append_torn_optype_entry(&last_wal_segment(&wal_dir));

    // Persistence is disabled, so this reopen genuinely replays the WAL and,
    // with tolerance off, must surface the torn-tail parse error.
    let reopened = AletheiaDB::with_unified_config(config(&wal_dir, false));
    assert!(
        reopened.is_err(),
        "tolerate_torn_tail=false must fail-stop on a torn WAL tail"
    );
}

/// Companion: with tolerance ON (default) and the SAME custom-config path, the
/// torn tail is tolerated — proving the outcome is driven by the flag, not by
/// the config-construction path.
#[test]
fn open_default_policy_tolerates_torn_tail_via_unified_config() {
    let dir = tempdir().unwrap();
    let wal_dir = dir.path().join("wal");

    let ids = {
        let db = AletheiaDB::with_unified_config(config(&wal_dir, true)).unwrap();
        commit(&db, &["X", "Y"])
    };

    append_torn_optype_entry(&last_wal_segment(&wal_dir));

    let db2 = AletheiaDB::with_unified_config(config(&wal_dir, true))
        .expect("default tolerate_torn_tail=true must recover");
    for id in &ids {
        assert!(db2.get_node(*id).is_ok(), "node {:?} recovered", id);
    }
}
