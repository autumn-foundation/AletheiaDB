//! Crash-recovery interaction between an in-flight commit and index persistence.
//!
//! # History
//!
//! The commit path once made a write DURABLE (WAL fsync + ack) and released
//! `current_timestamp` BEFORE applying it to the in-memory stores. In that
//! window `persist_indexes()` could stamp the manifest at the WAL *allocation
//! frontier* (above the durable-but-unapplied write) while snapshotting a state
//! that lacked it, and startup replay drops every entry with `lsn <
//! manifest.lsn` — a silently lost, already-durable write. An in-flight-LSN
//! *applied watermark* was added so persist stamped `min(in_flight)` instead of
//! the frontier.
//!
//! # Now: subsumed by WAL abort framing (Issue #3413)
//!
//! The #3413 abort-framing fix holds `current_timestamp` across the ENTIRE
//! commit — checks, WAL append, durability, **and apply** — releasing it only
//! after finalize. `persist_indexes()` acquires `current_timestamp` (briefly, to
//! read the frontier + in-flight set consistently), so it now **serializes
//! behind any in-flight commit's apply**: it can no longer snapshot a
//! durable-but-unapplied write, and two commits can no longer overlap (the
//! second blocks on the clock until the first has fully applied). The lost-write
//! race is therefore *structurally impossible*, and the applied-watermark is
//! belt-and-suspenders (in-flight is always empty when persist observes it).
//!
//! These tests now pin that stronger invariant: a commit parked at the
//! durable-but-unapplied seam (holding `current_timestamp`) BLOCKS a racing
//! persist / a second commit until it releases the clock, and nothing is lost or
//! duplicated across a crash. The one-shot `race_seam` parks exactly one commit
//! there; crash is simulated with `std::mem::forget(db)` (Drop never runs, so no
//! shutdown persist), exactly like a process kill after the WAL fsync.

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
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::Duration;
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
            ..Default::default()
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

/// Test A — a racing persist serializes behind a parked commit; no write is lost.
///
/// A commit parks AFTER its WAL fsync/ack but BEFORE its in-memory apply, holding
/// `current_timestamp` (Issue #3413). `persist_indexes()` needs
/// `current_timestamp`, so it BLOCKS until the parked commit releases the clock
/// post-apply — it can never snapshot the durable-but-unapplied write. After the
/// commit finishes, persist snapshots the applied state; a crash + reopen loses
/// nothing.
#[test]
fn test_a_parked_commit_blocks_racing_persist_no_loss() {
    let _g = lock();
    race_seam::clear();
    let tmp = tempdir().unwrap();
    let db_path = tmp.path();

    const PRELUDE: usize = 3; // nodes committed fully before the parked one

    let (prelude_ids, victim) = {
        let db = Arc::new(open(db_path));

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

        let persist_done = Arc::new(AtomicBool::new(false));

        let victim = std::thread::scope(|scope| {
            // Writer thread commits the victim node; it parks in the seam holding
            // `current_timestamp` across apply.
            let db_w = Arc::clone(&db);
            let writer = scope.spawn(move || create(&db_w, "victim"));

            // Wait until the victim is durable (fsynced) but not yet applied.
            parked_rx.recv().expect("victim must reach the seam");

            // Racing persist on its OWN thread: it needs `current_timestamp`, held
            // by the parked commit, so it blocks until release (post-apply).
            let db_p = Arc::clone(&db);
            let done = Arc::clone(&persist_done);
            let persist = scope.spawn(move || {
                db_p.persist_indexes().expect("racing persist");
                done.store(true, Ordering::SeqCst);
            });

            // Give persist time to reach and block on the clock; while the commit
            // is parked it must NOT have completed (proves the serialization).
            std::thread::sleep(Duration::from_millis(300));
            assert!(
                !persist_done.load(Ordering::SeqCst),
                "persist must block behind the parked commit (current_timestamp held across apply)"
            );

            // Release the victim so it applies and the commit returns; persist then
            // unblocks and snapshots the APPLIED state.
            release_tx.send(()).expect("release the victim");
            persist.join().expect("persist thread");
            writer.join().expect("writer thread")
        });

        assert!(
            persist_done.load(Ordering::SeqCst),
            "persist must complete once the commit releases the clock"
        );
        assert!(
            manifest_lsn(db_path) > 0,
            "manifest must be stamped by persist"
        );

        // Hard crash: no Drop, no shutdown persist.
        let db = Arc::try_unwrap(db).unwrap_or_else(|_| panic!("db still shared at crash"));
        std::mem::forget(db);
        (prelude_ids, victim)
    };

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

/// Test B — a second commit serializes behind a parked one; no duplication.
///
/// Under #3413 there is no durable-but-unapplied *overlap* window: a commit `B`
/// cannot begin while a parked commit `A` still holds `current_timestamp`, so
/// `B` blocks until `A` fully applies. Both then land in the snapshot; a crash +
/// reopen recovers each exactly once (no loss, no duplication). Replay
/// idempotency itself is pinned separately by the `recovery/replay_*` tests.
#[test]
fn test_b_second_commit_serializes_behind_parked_commit_no_dup() {
    let _g = lock();
    race_seam::clear();
    let tmp = tempdir().unwrap();
    let db_path = tmp.path();

    let (a, b) = {
        let db = Arc::new(open(db_path));

        let (parked_tx, parked_rx) = mpsc::channel::<()>();
        let (release_tx, release_rx) = mpsc::channel::<()>();
        race_seam::arm_pre_apply_once(Box::new(move || {
            parked_tx.send(()).expect("send parked signal");
            release_rx.recv().expect("await release");
        }));

        let b_done = Arc::new(AtomicBool::new(false));

        let (a, b) = std::thread::scope(|scope| {
            // A parks in the seam holding `current_timestamp` across apply.
            let db_a = Arc::clone(&db);
            let writer_a = scope.spawn(move || create(&db_a, "A"));
            parked_rx.recv().expect("A must reach the seam");

            // B commits on its own thread: it needs `current_timestamp`, held by
            // parked A, so it BLOCKS until A releases the clock post-apply — there
            // is no durable-but-unapplied overlap window anymore.
            let db_b = Arc::clone(&db);
            let done = Arc::clone(&b_done);
            let writer_b = scope.spawn(move || {
                let id = create(&db_b, "B");
                done.store(true, Ordering::SeqCst);
                id
            });

            std::thread::sleep(Duration::from_millis(300));
            assert!(
                !b_done.load(Ordering::SeqCst),
                "B must serialize behind parked A (current_timestamp held across apply)"
            );

            release_tx.send(()).expect("release A");
            let a = writer_a.join().expect("writer A");
            let b = writer_b.join().expect("writer B");
            (a, b)
        });

        assert!(
            b_done.load(Ordering::SeqCst),
            "B must complete after A releases"
        );

        // Persist AFTER both commits applied: the snapshot holds both.
        db.persist_indexes().expect("persist after both commits");

        let db = Arc::try_unwrap(db).unwrap_or_else(|_| panic!("db still shared at crash"));
        std::mem::forget(db);
        (a, b)
    };

    let db = open(db_path);
    assert_eq!(
        db.node_count(),
        2,
        "both A and B must be present exactly once: fewer = lost, more = duplicated"
    );
    assert_present(&db, a, "A");
    assert_history_len(&db, a, "A", 1);
    assert_present(&db, b, "B");
    assert_history_len(&db, b, "B", 1);
}
