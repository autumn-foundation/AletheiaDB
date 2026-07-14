//! Regression tests for the lost-write crash-recovery race between an in-flight
//! commit and index persistence.
//!
//! # The bug
//!
//! The commit path makes a write DURABLE (WAL fsync + ack) BEFORE it is APPLIED
//! to the in-memory current/historical stores, and no lock spans both points.
//! `persist_indexes()` previously stamped the manifest with the WAL *allocation
//! frontier* (next-to-allocate LSN), and startup replay drops every WAL entry
//! with `lsn < manifest.lsn`. A write that was fsynced (so `lsn < frontier`) but
//! not yet applied when the snapshot was taken was therefore BOTH absent from
//! the persisted snapshot AND dropped by replay — a silently lost,
//! already-acknowledged write.
//!
//! # The fix
//!
//! An in-flight-LSN tracker records the base LSN of every commit that is durable
//! but not yet applied. `persist_indexes()` stamps the manifest with the minimum
//! in-flight LSN (the *applied watermark*) instead of the frontier, so replay
//! re-covers any durable-but-unapplied write. Re-applying a write that DID make
//! it into the snapshot is safe because replay is idempotent (keyed by
//! `version_id`) — Test B pins exactly that.
//!
//! # Determinism
//!
//! These tests do NOT rely on timing luck. They arm a one-shot commit-path seam
//! (`race_seam`) that parks exactly one commit at the durable-but-not-applied
//! point while the main thread drives `persist_indexes()`, so the race is forced
//! by ordering. Crash is simulated with `std::mem::forget(db)` (Drop never runs,
//! so no shutdown persist), exactly like a process kill after the WAL fsync.

use aletheiadb::api::transaction::write::race_seam;
use aletheiadb::config::{AletheiaDBConfig, WalConfigBuilder};
use aletheiadb::core::id::NodeId;
use aletheiadb::storage::index_persistence::formats::{
    GraphPersistencePolicy, PersistencePolicies, StringPersistencePolicy,
    TemporalPersistencePolicy, VectorPersistencePolicy,
};
use aletheiadb::storage::index_persistence::{IndexPersistenceManager, PersistenceConfig};
use aletheiadb::storage::wal::DurabilityMode;
use aletheiadb::{AletheiaDB, PropertyMapBuilder};
use std::path::Path;
use std::sync::Mutex;
use std::sync::mpsc;
use tempfile::tempdir;

/// Serialize tests: index persistence round-trips the process-global string
/// interner AND the `race_seam` is a process-global one-shot.
static TEST_MUTEX: Mutex<()> = Mutex::new(());

fn lock() -> std::sync::MutexGuard<'static, ()> {
    TEST_MUTEX
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Persistence policies that never fire on their own (keeps a leaked,
/// crash-simulated background worker inert).
fn inert_policies() -> PersistencePolicies {
    PersistencePolicies {
        vector: VectorPersistencePolicy {
            mutation_threshold: u32::MAX,
            time_interval_secs: u32::MAX,
        },
        graph: GraphPersistencePolicy {
            on_adjacency_rebuild: false,
            mutation_threshold: u32::MAX,
            time_interval_secs: u32::MAX,
        },
        temporal: TemporalPersistencePolicy {
            version_threshold: u32::MAX,
            anchor_threshold: u32::MAX,
            time_interval_secs: u32::MAX,
        },
        strings: StringPersistencePolicy {
            new_strings_threshold: u32::MAX,
            time_interval_secs: u32::MAX,
        },
    }
}

/// Durable config: Synchronous WAL (fsync per commit) + index persistence with
/// load-on-startup.
fn durable_config(db_path: &Path) -> AletheiaDBConfig {
    AletheiaDBConfig::builder()
        .wal(
            WalConfigBuilder::new()
                .wal_dir(db_path.join("wal"))
                .durability_mode(DurabilityMode::Synchronous)
                .build(),
        )
        .persistence(PersistenceConfig {
            enabled: true,
            data_dir: db_path.join("indexes"),
            load_on_startup: true,
            policies: inert_policies(),
            use_mmap: false,
        })
        .build()
}

fn open(db_path: &Path) -> AletheiaDB {
    AletheiaDB::with_unified_config(durable_config(db_path)).expect("open database")
}

fn create(db: &AletheiaDB, name: &str) -> NodeId {
    db.create_node(
        "LostWrite",
        PropertyMapBuilder::new().insert("name", name).build(),
    )
    .unwrap_or_else(|e| panic!("create_node({name}) failed: {e}"))
}

fn manifest_lsn(db_path: &Path) -> u64 {
    IndexPersistenceManager::new(db_path.join("indexes"))
        .load_manifest_and_strings()
        .expect("manifest must exist")
        .lsn
}

fn assert_present(db: &AletheiaDB, id: NodeId, name: &str) {
    let node = db
        .get_node(id)
        .unwrap_or_else(|e| panic!("node {name} ({id:?}) lost after recovery: {e}"));
    assert_eq!(
        node.properties.get("name"),
        Some(&name.into()),
        "node {name} recovered with wrong properties"
    );
}

/// Assert a node exists and its history holds exactly `expected` versions —
/// catches both loss (0) and boundary double-apply duplication (>expected).
fn assert_history_len(db: &AletheiaDB, id: NodeId, name: &str, expected: usize) {
    let history = db
        .get_node_history(id)
        .unwrap_or_else(|e| panic!("history for node {name} ({id:?}) unavailable: {e}"));
    assert_eq!(
        history.versions.len(),
        expected,
        "node {name} ({id:?}) must have exactly {expected} history version(s); \
         fewer = lost write, more = boundary double-apply duplication"
    );
}

/// Test A — lost write.
///
/// A commit is parked AFTER its WAL fsync/ack but BEFORE its in-memory apply.
/// `persist_indexes()` runs in that window; the parked write is durable but not
/// in the snapshot. On the pre-fix code the manifest recorded the allocation
/// frontier, so replay dropped the write → it vanished after crash+reopen. With
/// the fix the manifest records the applied watermark (the parked commit's own
/// LSN), so replay re-covers it.
#[test]
fn test_a_durable_unapplied_write_survives_persist_then_crash() {
    let _g = lock();
    race_seam::clear();
    let tmp = tempdir().unwrap();
    let db_path = tmp.path();

    const PRELUDE: usize = 3; // nodes committed fully before the parked one

    let (prelude_ids, victim, manifest_after_persist) = {
        let db = open(db_path);

        // Fully commit PRELUDE nodes (no hook armed yet).
        let mut prelude_ids = Vec::new();
        for i in 0..PRELUDE {
            prelude_ids.push((create(&db, &format!("prelude-{i}")), format!("prelude-{i}")));
        }

        // Arm a one-shot seam: the next commit parks at the
        // durable-but-not-applied point, signals, and waits for release.
        let (parked_tx, parked_rx) = mpsc::channel::<()>();
        let (release_tx, release_rx) = mpsc::channel::<()>();
        race_seam::arm_pre_apply_once(Box::new(move || {
            parked_tx.send(()).expect("send parked signal");
            release_rx.recv().expect("await release");
        }));

        let victim = std::thread::scope(|scope| {
            // Writer thread commits the victim node; it parks in the seam.
            let writer = scope.spawn(|| create(&db, "victim"));

            // Wait until the victim is durable (fsynced) but not yet applied.
            parked_rx.recv().expect("victim must reach the seam");

            // Persist WHILE the victim is durable-but-unapplied. Pre-fix this
            // captured the frontier (above the victim's LSN); post-fix it
            // captures the victim's in-flight LSN as the watermark.
            db.persist_indexes().expect("racing persist");

            // Release the victim so it applies and the commit returns.
            release_tx.send(()).expect("release the victim");
            writer.join().expect("writer thread")
        });

        let manifest_after_persist = manifest_lsn(db_path);

        // Hard crash: no Drop, no shutdown persist.
        std::mem::forget(db);
        (prelude_ids, victim, manifest_after_persist)
    };

    // Crash simulation sanity: the on-disk manifest must not have advanced past
    // the racing persist.
    assert_eq!(
        manifest_lsn(db_path),
        manifest_after_persist,
        "crash simulation invalid: manifest advanced after mem::forget"
    );

    let db = open(db_path);
    assert_eq!(
        db.node_count(),
        PRELUDE + 1,
        "recovered node count must equal writes: fewer = lost write"
    );
    for (id, name) in &prelude_ids {
        assert_present(&db, *id, name);
        assert_history_len(&db, *id, name, 1);
    }
    assert_present(&db, victim, "victim");
    assert_history_len(&db, victim, "victim", 1);
}

/// Test B — no duplication (overlap band).
///
/// Forces the manifest watermark to land BELOW an already-applied HIGHER LSN, so
/// replay re-applies a write that IS present in the snapshot. That write must
/// end up with exactly one history version — the idempotency the watermark fix
/// relies on.
///
/// A (parked, LSN_A) is durable-but-unapplied; B is then committed fully on the
/// main thread (LSN_B > LSN_A, applied, in the snapshot). Persist captures the
/// watermark = LSN_A. On reopen, replay from LSN_A re-applies A (new) AND B
/// (already in the snapshot → idempotent, NOT duplicated).
#[test]
fn test_b_overlap_band_replay_does_not_duplicate() {
    let _g = lock();
    race_seam::clear();
    let tmp = tempdir().unwrap();
    let db_path = tmp.path();

    let (a, b, manifest_after_persist) = {
        let db = open(db_path);

        let (parked_tx, parked_rx) = mpsc::channel::<()>();
        let (release_tx, release_rx) = mpsc::channel::<()>();
        race_seam::arm_pre_apply_once(Box::new(move || {
            parked_tx.send(()).expect("send parked signal");
            release_rx.recv().expect("await release");
        }));

        let (a, b) = std::thread::scope(|scope| {
            // A parks in the seam (durable, unapplied, in-flight LSN_A).
            let writer = scope.spawn(|| create(&db, "A"));
            parked_rx.recv().expect("A must reach the seam");

            // B commits FULLY on this thread (the one-shot seam is already
            // consumed by A, so B does not park). LSN_B > LSN_A; B is applied
            // and deregistered, so it lands IN the snapshot below.
            let b = create(&db, "B");

            // Persist: watermark = min in-flight = LSN_A (< LSN_B). The snapshot
            // includes B but not A; the manifest records LSN_A.
            db.persist_indexes().expect("racing persist");

            release_tx.send(()).expect("release A");
            let a = writer.join().expect("writer thread");
            (a, b)
        });

        let manifest_after_persist = manifest_lsn(db_path);
        std::mem::forget(db);
        (a, b, manifest_after_persist)
    };

    assert_eq!(
        manifest_lsn(db_path),
        manifest_after_persist,
        "crash simulation invalid: manifest advanced after mem::forget"
    );

    let db = open(db_path);
    assert_eq!(
        db.node_count(),
        2,
        "both A and B must be present exactly once: fewer = lost, more = duplicated"
    );
    // A: lost-write path (not in snapshot, recovered by replay).
    assert_present(&db, a, "A");
    assert_history_len(&db, a, "A", 1);
    // B: overlap-band path (in snapshot AND replayed) — must NOT be duplicated.
    assert_present(&db, b, "B");
    assert_history_len(&db, b, "B", 1);
}
