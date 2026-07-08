//! Regression tests for two compounding LSN recovery bugs:
//!
//! - **Issue #3419 (manifest off-by-one)**: `persist_indexes()` (and the
//!   background/shutdown persistence paths) store `wal.current_lsn()`, which is
//!   the NEXT-to-allocate LSN, in the index manifest. Startup then replayed
//!   from `LSN(manifest.lsn).next()`, silently skipping the first durable WAL
//!   entry written after a persist. An acknowledged, fsynced write vanished
//!   after a crash + restart.
//!
//! - **Issue #3420 (LSN allocator restart)**: the WAL LSN allocator always
//!   started at 1 on every process start, ignoring LSNs already present in the
//!   WAL segments and the manifest. After a restart, new writes received LSNs
//!   *below* the manifest LSN, so the next startup's differential replay
//!   skipped the entire previous session's writes (and duplicate LSNs across
//!   segments broke LSN total ordering).
//!
//! Crash simulation: `std::mem::forget(db)` leaks the `AletheiaDB` handle so
//! `Drop` never runs — no shutdown persist, no manifest update — exactly like
//! a process kill after the WAL fsync. Because a leaked background
//! persistence worker keeps running inside the test process, every test uses
//! *inert* persistence policies (all thresholds and time intervals at
//! `u32::MAX`) so a leaked worker can never persist behind our back; the only
//! persists are explicit `persist_indexes()` calls and the worker's final
//! persist on *clean* drop.
//!
//! All tests are serialized behind a file-level mutex because index
//! persistence round-trips the process-global string interner.

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
use tempfile::tempdir;

/// Serialize tests: index persistence round-trips the global string interner.
static LSN_RECOVERY_TEST_MUTEX: Mutex<()> = Mutex::new(());

/// Lock helper that survives poisoning (a failing test must not cascade).
fn lock() -> std::sync::MutexGuard<'static, ()> {
    LSN_RECOVERY_TEST_MUTEX
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Persistence policies that never fire on their own.
///
/// Keeps leaked (crash-simulated) background workers inert so they cannot
/// persist state "post-crash" and invalidate the crash simulation.
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

/// Durable config: Synchronous WAL (fsync-per-commit) + index persistence
/// with load-on-startup. Explicit tempdir-rooted paths, never defaults.
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

/// WAL-only config (no index persistence): startup does a full WAL replay.
fn wal_only_config(db_path: &Path) -> AletheiaDBConfig {
    AletheiaDBConfig::builder()
        .wal(
            WalConfigBuilder::new()
                .wal_dir(db_path.join("wal"))
                .durability_mode(DurabilityMode::Synchronous)
                .build(),
        )
        .persistence(PersistenceConfig {
            enabled: false,
            ..PersistenceConfig::default()
        })
        .build()
}

fn open(db_path: &Path) -> AletheiaDB {
    AletheiaDB::with_unified_config(durable_config(db_path)).expect("open database")
}

fn create(db: &AletheiaDB, name: &str) -> NodeId {
    db.create_node(
        "LsnRegression",
        PropertyMapBuilder::new().insert("name", name).build(),
    )
    .unwrap_or_else(|e| panic!("create_node({name}) failed: {e}"))
}

/// Read the on-disk manifest LSN directly (bypassing any live database).
fn manifest_lsn(db_path: &Path) -> u64 {
    IndexPersistenceManager::new(db_path.join("indexes"))
        .load_manifest_and_strings()
        .expect("manifest must exist")
        .lsn
}

fn assert_node_present(db: &AletheiaDB, id: NodeId, name: &str) {
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
         fewer means lost writes, more means boundary double-apply duplication"
    );
}

/// T1 — Issue #3419 exact repro: the FIRST write after a persist must survive
/// a crash.
///
/// create A → persist_indexes → create B, C → hard crash (mem::forget) →
/// reopen: A, B, and C must all be present.
///
/// Pre-fix failure mode: manifest stored next-to-allocate LSN N; replay
/// started at N+1; B (the first post-persist write, at LSN N) was silently
/// dropped even though its WAL append was fsync-acknowledged.
#[test]
fn t1_first_post_persist_write_survives_crash() {
    let _g = lock();
    let tmp = tempdir().unwrap();
    let db_path = tmp.path();

    let (a, b, c, manifest_after_persist) = {
        let db = open(db_path);
        let a = create(&db, "A");
        db.persist_indexes().expect("manual persist");
        let manifest_after_persist = manifest_lsn(db_path);
        let b = create(&db, "B");
        let c = create(&db, "C");
        // Hard crash: no Drop, no shutdown persist.
        std::mem::forget(db);
        (a, b, c, manifest_after_persist)
    };

    // Sanity: mem::forget really skipped the Drop-time final persist —
    // the on-disk manifest LSN must NOT have advanced past the manual persist.
    assert_eq!(
        manifest_lsn(db_path),
        manifest_after_persist,
        "crash simulation invalid: manifest advanced after mem::forget"
    );

    let db = open(db_path);
    for (id, name) in [(a, "A"), (b, "B"), (c, "C")] {
        assert_node_present(&db, id, name);
        assert_history_len(&db, id, name, 1);
    }
}

/// T2 — Issue #3420 three-session repro: LSNs must continue monotonically
/// across restarts.
///
/// S1: write A + persist + CLEAN drop (shutdown persist runs).
/// S2: reopen, write D, hard crash.
/// S3: reopen → D must be present.
///
/// Pre-fix failure mode: S2's allocator restarted at LSN 1, so D's WAL entry
/// received an LSN below the manifest LSN and S3's differential replay never
/// saw it.
#[test]
fn t2_lsn_allocation_continues_across_sessions() {
    let _g = lock();
    let tmp = tempdir().unwrap();
    let db_path = tmp.path();

    // Session 1: clean shutdown (Drop runs the final shutdown persist).
    let a = {
        let db = open(db_path);
        let a = create(&db, "A");
        db.persist_indexes().expect("manual persist");
        a
        // clean drop here
    };

    // Session 2: write D, then crash.
    let d = {
        let db = open(db_path);
        assert_node_present(&db, a, "A");
        let d = create(&db, "D");
        std::mem::forget(db);
        d
    };

    // Session 3: D must have survived.
    let db = open(db_path);
    assert_node_present(&db, a, "A");
    assert_node_present(&db, d, "D");
    assert_history_len(&db, d, "D", 1);
}

/// T3 — combined multi-cycle: three write/persist/crash cycles with zero
/// cumulative loss and monotonically non-decreasing LSNs across sessions.
#[test]
fn t3_multi_cycle_zero_loss_and_monotonic_lsns() {
    let _g = lock();
    let tmp = tempdir().unwrap();
    let db_path = tmp.path();

    let mut expected: Vec<(NodeId, String)> = Vec::new();
    let mut last_session_end_lsn: u64 = 0;

    for cycle in 0..3 {
        let db = open(db_path);

        let session_start_lsn = db.__test_current_wal_lsn();
        assert!(
            session_start_lsn >= last_session_end_lsn,
            "cycle {cycle}: LSNs must be monotonic across sessions \
             (start {session_start_lsn} < previous end {last_session_end_lsn})"
        );

        // Everything from prior cycles must still be there.
        for (id, name) in &expected {
            assert_node_present(&db, *id, name);
        }

        // Pre-persist write, persist, then a post-persist write (the #3419
        // boundary victim), then crash.
        let pre = create(&db, &format!("pre-{cycle}"));
        expected.push((pre, format!("pre-{cycle}")));
        db.persist_indexes().expect("manual persist");
        let post = create(&db, &format!("post-{cycle}"));
        expected.push((post, format!("post-{cycle}")));

        last_session_end_lsn = db.__test_current_wal_lsn();
        std::mem::forget(db);
    }

    let db = open(db_path);
    assert!(
        db.__test_current_wal_lsn() >= last_session_end_lsn,
        "final session LSN regressed"
    );
    for (id, name) in &expected {
        assert_node_present(&db, *id, name);
        assert_history_len(&db, *id, name, 1);
    }
    assert_eq!(
        db.node_count(),
        expected.len(),
        "cumulative node count mismatch: loss or duplication across cycles"
    );
}

/// T4 — edge: persist then crash with NO post-persist writes. Reopen must be
/// clean and the allocator must be seeded at/above the manifest LSN so the
/// next write lands inside the replay window.
#[test]
fn t4_persist_then_crash_without_post_persist_writes() {
    let _g = lock();
    let tmp = tempdir().unwrap();
    let db_path = tmp.path();

    let a = {
        let db = open(db_path);
        let a = create(&db, "A");
        db.persist_indexes().expect("manual persist");
        std::mem::forget(db);
        a
    };

    let manifest = manifest_lsn(db_path);

    // Reopen: clean, nothing lost.
    let b = {
        let db = open(db_path);
        assert_node_present(&db, a, "A");
        assert_history_len(&db, a, "A", 1);

        // Allocator must be seeded so the next write's LSN is not below the
        // manifest LSN (otherwise the next restart's differential replay
        // would skip it).
        let next_lsn = db.__test_current_wal_lsn();
        assert!(
            next_lsn >= manifest,
            "allocator not seeded: next LSN {next_lsn} is below manifest LSN {manifest}"
        );

        let b = create(&db, "B");
        std::mem::forget(db);
        b
    };

    let db = open(db_path);
    assert_node_present(&db, a, "A");
    assert_node_present(&db, b, "B");
    assert_history_len(&db, b, "B", 1);
}

/// T5 — edge: a fresh directory still starts at LSN 1 (no behavior change).
#[test]
fn t5_fresh_directory_first_lsn_is_one() {
    let _g = lock();
    let tmp = tempdir().unwrap();
    let db_path = tmp.path();

    let db = open(db_path);
    assert_eq!(
        db.__test_current_wal_lsn(),
        1,
        "fresh database must start allocating at LSN 1"
    );
    let a = create(&db, "A");
    assert_node_present(&db, a, "A");
    assert!(
        db.__test_current_wal_lsn() > 1,
        "LSN must advance after the first write"
    );
}

/// T6 — edge: WAL without a manifest (index persistence disabled) still does
/// a full replay on startup. Guards the pre-existing WAL-only recovery path.
#[test]
fn t6_wal_only_full_replay_without_manifest() {
    let _g = lock();
    let tmp = tempdir().unwrap();
    let db_path = tmp.path();

    let (a, b) = {
        let db = AletheiaDB::with_unified_config(wal_only_config(db_path)).expect("open");
        let a = create(&db, "A");
        let b = create(&db, "B");
        // Synchronous mode: entries are already fsynced; crash.
        std::mem::forget(db);
        (a, b)
    };

    let db = AletheiaDB::with_unified_config(wal_only_config(db_path)).expect("reopen");
    assert_node_present(&db, a, "A");
    assert_node_present(&db, b, "B");
    assert_history_len(&db, a, "A", 1);
    assert_history_len(&db, b, "B", 1);
}

/// T7 — boundary double-apply pin (Issue #3419 semantics decision).
///
/// The manifest LSN is captured BEFORE the snapshot is read, so a write that
/// races a persist can receive an LSN >= manifest.lsn AND still be included
/// in the snapshot. Startup replays from the manifest LSN inclusive, so such
/// entries are re-applied — replay must treat them as no-ops.
///
/// This test hammers `create_node` from a writer thread while the main thread
/// runs `persist_indexes()` repeatedly, then crashes and reopens. Regardless
/// of how the race resolved, every write must be present EXACTLY once: a
/// missing node means the replay window lost a write (#3419), a node with
/// more than one history version means boundary re-application duplicated
/// bi-temporal history (the double-apply hazard this fix guards).
///
/// (A deterministic pin of the same property lives in
/// `storage::recovery::tests::replay_is_idempotent_for_already_applied_entries`,
/// which replays the same WAL twice.)
#[test]
fn t7_write_racing_persist_is_neither_lost_nor_duplicated() {
    let _g = lock();
    let tmp = tempdir().unwrap();
    let db_path = tmp.path();

    const WRITES: usize = 120;

    let ids: Vec<(NodeId, String)> = {
        let db = open(db_path);

        let ids = std::thread::scope(|scope| {
            let writer = scope.spawn(|| {
                let mut ids = Vec::with_capacity(WRITES);
                for i in 0..WRITES {
                    let name = format!("racer-{i}");
                    let id = create(&db, &name);
                    ids.push((id, name));
                    std::thread::sleep(std::time::Duration::from_millis(2));
                }
                ids
            });

            // Persist repeatedly while the writer is running so several
            // manifest LSN captures land inside the write stream.
            for _ in 0..8 {
                db.persist_indexes().expect("racing persist");
                std::thread::sleep(std::time::Duration::from_millis(25));
            }

            writer.join().expect("writer thread")
        });

        // Crash without any final persist.
        std::mem::forget(db);
        ids
    };

    let db = open(db_path);
    assert_eq!(
        db.node_count(),
        WRITES,
        "recovered node count must equal writes: fewer = lost, more = duplicated"
    );
    for (id, name) in &ids {
        assert_node_present(&db, *id, name);
        assert_history_len(&db, *id, name, 1);
    }
}
