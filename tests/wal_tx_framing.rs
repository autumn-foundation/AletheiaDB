#![cfg(test)]

//! Crash-matrix integration tests for WAL transaction framing (Issue #3413).
//!
//! A committing `WriteTransaction` translates its N buffered writes into N
//! independent WAL operations under one LSN batch. Before this change there was
//! **no** begin/commit framing record, which produced two correctness bugs that
//! these tests pin:
//!
//! 1. **Prefix-replay / atomicity break** — a crash during the commit flush can
//!    persist and later replay a *prefix* of a batch whose commit was never
//!    acknowledged, so half a transaction becomes durable.
//! 2. **Timestamp bisection** — replay re-stamps each recovered version with
//!    that entry's *own* wallclock, so one atomic transaction's versions receive
//!    N distinct transaction times and `AS OF SYSTEM_TIME` between two of them
//!    can observe a half-batch.
//!
//! Every test here drives **real** WAL replay: it writes through a durable
//! on-disk `AletheiaDB::open`, drops the handle (synchronously flushing), then
//! **deletes the index-persistence snapshot** before reopening so the reopen is
//! forced to replay the WAL from LSN 1 rather than being short-circuited by the
//! snapshot. Data reappearing after that deletion is proof the WAL replay ran.

use aletheiadb::api::WriteOps;
use aletheiadb::core::NodeId;
use aletheiadb::{AletheiaDB, PropertyMapBuilder};
use std::fs;
use std::path::{Path, PathBuf};
use tempfile::tempdir;

/// Serialized size (bytes) of a terminal `CommitTx` marker entry:
/// 24 fixed overhead (LSN 8 + timestamp 12 + checksum 4) + 1 op tag +
/// 8 tx_id + 4 entry_count + 12 commit_timestamp = 49.
///
/// Truncating exactly this many bytes off the tail of the last segment removes
/// the marker of the last transaction while leaving its data ops on disk —
/// deterministically simulating "crash after the data ops, before/at the commit
/// record reached durability".
const COMMIT_MARKER_LEN: u64 = 49;

/// Delete the index-persistence snapshot so the next `open` must replay the WAL
/// from the beginning instead of loading current state from the snapshot.
fn force_full_replay(data_dir: &Path) {
    let indexes = data_dir.join("indexes");
    if indexes.exists() {
        fs::remove_dir_all(&indexes).expect("remove index snapshot");
    }
}

/// Return the `*.log` WAL segment files under `{data_dir}/wal`, sorted by their
/// numeric segment id (ascending) — the last is the most-recently written.
fn wal_segments(data_dir: &Path) -> Vec<PathBuf> {
    let wal_dir = data_dir.join("wal");
    let mut segs: Vec<(u64, PathBuf)> = fs::read_dir(&wal_dir)
        .expect("read wal dir")
        .flatten()
        .filter_map(|e| {
            let p = e.path();
            let stem = p.file_stem()?.to_str()?.to_string();
            if p.extension().and_then(|s| s.to_str()) == Some("log") {
                stem.parse::<u64>().ok().map(|id| (id, p))
            } else {
                None
            }
        })
        .collect();
    segs.sort_by_key(|(id, _)| *id);
    segs.into_iter().map(|(_, p)| p).collect()
}

/// Truncate `bytes` off the tail of the last (highest-id) non-empty WAL segment,
/// simulating a torn write / lost tail. Returns the new length.
fn truncate_last_segment_tail(data_dir: &Path, bytes: u64) -> u64 {
    let seg = wal_segments(data_dir)
        .into_iter()
        .rev()
        .find(|p| fs::metadata(p).map(|m| m.len() > 0).unwrap_or(false))
        .expect("a non-empty WAL segment must exist");
    let len = fs::metadata(&seg).unwrap().len();
    assert!(
        len > bytes,
        "segment {:?} ({} bytes) too small to truncate {} bytes",
        seg,
        len,
        bytes
    );
    let new_len = len - bytes;
    let f = fs::OpenOptions::new().write(true).open(&seg).unwrap();
    f.set_len(new_len).unwrap();
    f.sync_all().unwrap();
    new_len
}

/// Commit `labels.len()` nodes as ONE atomic transaction and return their ids.
fn commit_batch(db: &AletheiaDB, labels: &[&str]) -> Vec<NodeId> {
    db.write(|tx| {
        let mut ids = Vec::new();
        for (i, label) in labels.iter().enumerate() {
            let id = tx.create_node(
                label,
                PropertyMapBuilder::new().insert("i", i as i64).build(),
            )?;
            ids.push(id);
        }
        Ok::<_, aletheiadb::Error>(ids)
    })
    .expect("batch commit")
}

/// AC#3 (timestamp coherence): after replay, every version produced by ONE
/// atomic batch must carry an *identical* transaction time. On trunk each op is
/// re-stamped with its own per-entry wallclock, so the five differ.
#[test]
fn atomic_batch_shares_single_transaction_time() {
    let dir = tempdir().unwrap();
    let data_dir = dir.path().join("db");

    let ids = {
        let db = AletheiaDB::open(&data_dir).unwrap();
        commit_batch(&db, &["A", "B", "C", "D", "E"])
    };

    force_full_replay(&data_dir);

    let db2 = AletheiaDB::open(&data_dir).unwrap();
    let tx_times: Vec<_> = ids
        .iter()
        .map(|id| {
            let hist = db2.get_node_history(*id).expect("history after replay");
            hist.versions
                .last()
                .expect("at least one version")
                .temporal
                .transaction_time()
                .start()
        })
        .collect();

    let first = tx_times[0];
    for (i, t) in tx_times.iter().enumerate() {
        assert_eq!(
            *t, first,
            "version {} transaction time {:?} must equal the batch's single commit time {:?}",
            i, t, first
        );
    }
}

/// AC#2 (prefix discard): a batch whose commit marker never reached disk must
/// replay as **all-or-nothing** — zero of its writes may survive. On trunk the
/// unframed prefix leaks (some nodes survive).
#[test]
fn replay_discards_uncommitted_batch_prefix() {
    let dir = tempdir().unwrap();
    let data_dir = dir.path().join("db");

    let ids = {
        let db = AletheiaDB::open(&data_dir).unwrap();
        commit_batch(&db, &["P", "Q", "R", "S"])
    };

    // Drop the marker (and, on the unframed trunk, tear the last data op).
    truncate_last_segment_tail(&data_dir, COMMIT_MARKER_LEN);
    force_full_replay(&data_dir);

    let db2 = AletheiaDB::open(&data_dir).unwrap();
    for id in &ids {
        assert!(
            db2.get_node(*id).is_err(),
            "node {:?} from an uncommitted (marker-less) batch must NOT survive replay",
            id
        );
    }
}

/// Companion to the prefix-discard test: a fully-committed batch (marker intact)
/// must replay in full. Guards against over-discarding.
#[test]
fn replay_keeps_fully_committed_batch() {
    let dir = tempdir().unwrap();
    let data_dir = dir.path().join("db");

    let ids = {
        let db = AletheiaDB::open(&data_dir).unwrap();
        commit_batch(&db, &["P", "Q", "R", "S"])
    };

    force_full_replay(&data_dir);

    let db2 = AletheiaDB::open(&data_dir).unwrap();
    for id in &ids {
        assert!(
            db2.get_node(*id).is_ok(),
            "node {:?} from a fully committed batch must survive replay",
            id
        );
    }
}

/// AC#1/AC#2: a committed transaction (marker intact) and a following
/// transaction whose marker was lost must recover with an **exact** boundary —
/// the first fully present, the second fully absent. On trunk the second tx's
/// prefix leaks in.
#[test]
fn committed_tx_kept_following_uncommitted_tx_discarded() {
    let dir = tempdir().unwrap();
    let data_dir = dir.path().join("db");

    let (committed, uncommitted) = {
        let db = AletheiaDB::open(&data_dir).unwrap();
        let committed = commit_batch(&db, &["C1", "C2"]);
        let uncommitted = commit_batch(&db, &["U1", "U2"]);
        (committed, uncommitted)
    };

    // Remove only the last transaction's commit marker.
    truncate_last_segment_tail(&data_dir, COMMIT_MARKER_LEN);
    force_full_replay(&data_dir);

    let db2 = AletheiaDB::open(&data_dir).unwrap();
    for id in &committed {
        assert!(
            db2.get_node(*id).is_ok(),
            "committed tx node {:?} must survive",
            id
        );
    }
    for id in &uncommitted {
        assert!(
            db2.get_node(*id).is_err(),
            "uncommitted (marker-less) tx node {:?} must be discarded",
            id
        );
    }
}

/// A single-op transaction is framed too: it survives a clean replay, and losing
/// its marker discards it (all-or-nothing even for N=1).
#[test]
fn single_op_tx_is_framed() {
    let dir = tempdir().unwrap();
    let data_dir = dir.path().join("db");

    // Clean replay keeps it.
    {
        let id = {
            let db = AletheiaDB::open(&data_dir).unwrap();
            commit_batch(&db, &["Solo"])[0]
        };
        force_full_replay(&data_dir);
        let db2 = AletheiaDB::open(&data_dir).unwrap();
        assert!(
            db2.get_node(id).is_ok(),
            "committed single-op tx must survive"
        );
    }

    // Fresh dir: losing the marker discards the single op.
    let dir2 = tempdir().unwrap();
    let data_dir2 = dir2.path().join("db");
    let id2 = {
        let db = AletheiaDB::open(&data_dir2).unwrap();
        commit_batch(&db, &["SoloUncommitted"])[0]
    };
    truncate_last_segment_tail(&data_dir2, COMMIT_MARKER_LEN);
    force_full_replay(&data_dir2);
    let db3 = AletheiaDB::open(&data_dir2).unwrap();
    assert!(
        db3.get_node(id2).is_err(),
        "single-op tx whose marker was lost must be discarded"
    );
}

/// Concurrency: several transactions committed from different threads each carry
/// their own single commit time, and after replay every committed batch is fully
/// present with all its versions sharing one transaction time (no marker crosses
/// between concurrent transactions).
#[test]
fn interleaved_concurrent_txs_each_atomic() {
    use std::sync::Arc;
    use std::thread;

    let dir = tempdir().unwrap();
    let data_dir = dir.path().join("db");

    let all_ids: Vec<Vec<NodeId>> = {
        let db = Arc::new(AletheiaDB::open(&data_dir).unwrap());
        let mut handles = Vec::new();
        for t in 0..4u32 {
            let db = Arc::clone(&db);
            handles.push(thread::spawn(move || {
                let l1 = format!("T{}a", t);
                let l2 = format!("T{}b", t);
                let l3 = format!("T{}c", t);
                commit_batch(&db, &[&l1, &l2, &l3])
            }));
        }
        let ids: Vec<Vec<NodeId>> = handles.into_iter().map(|h| h.join().unwrap()).collect();
        // Arc dropped at end of scope, flushing.
        Arc::try_unwrap(db)
            .map(drop)
            .unwrap_or_else(|_| panic!("db still shared"));
        ids
    };

    force_full_replay(&data_dir);

    let db2 = AletheiaDB::open(&data_dir).unwrap();
    for batch in &all_ids {
        let mut batch_ts = None;
        for id in batch {
            let hist = db2.get_node_history(*id).expect("history after replay");
            let ts = hist
                .versions
                .last()
                .expect("version present")
                .temporal
                .transaction_time()
                .start();
            match batch_ts {
                None => batch_ts = Some(ts),
                Some(prev) => assert_eq!(
                    ts, prev,
                    "all versions of one atomic batch must share one transaction time"
                ),
            }
        }
    }
}
