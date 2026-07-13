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
use aletheiadb::index::vector::{DistanceMetric, HnswConfig};
use aletheiadb::storage::index_persistence::formats::{
    GraphPersistencePolicy, PersistencePolicies, StringPersistencePolicy,
    TemporalPersistencePolicy, VectorPersistencePolicy,
};
use aletheiadb::storage::index_persistence::{IndexPersistenceManager, PersistenceConfig};
use aletheiadb::storage::wal::DurabilityMode;
use aletheiadb::{AletheiaDB, PropertyMapBuilder, SimilarityQuery, WriteOps};
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

fn create_with_embedding(db: &AletheiaDB, name: &str, embedding: &[f32]) -> NodeId {
    db.create_node(
        "LsnRegression",
        PropertyMapBuilder::new()
            .insert("name", name)
            .insert_vector("embedding", embedding)
            .build(),
    )
    .unwrap_or_else(|e| panic!("create_node({name}) with embedding failed: {e}"))
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
///
/// Vector coverage (PR #3428 review): Issue #3419's original report cites a
/// node with an EMBEDDING vanishing from `find_similar` after crash+reopen.
/// A and B carry embeddings and the post-recovery assertion includes the
/// vector index (B must be findable via `find_similar` from A), so this test
/// guards the HNSW re-indexing path of replayed writes, not just the graph.
#[test]
fn t1_first_post_persist_write_survives_crash() {
    let _g = lock();
    let tmp = tempdir().unwrap();
    let db_path = tmp.path();

    let embedding_a: [f32; 4] = [1.0, 0.0, 0.0, 0.0];
    let embedding_b: [f32; 4] = [0.9, 0.1, 0.0, 0.0];

    let (a, b, c, manifest_after_persist) = {
        let db = open(db_path);
        db.vector_index("embedding")
            .hnsw(HnswConfig::new(4, DistanceMetric::Cosine))
            .enable()
            .expect("enable vector index");
        let a = create_with_embedding(&db, "A", &embedding_a);
        db.persist_indexes().expect("manual persist");
        let manifest_after_persist = manifest_lsn(db_path);
        // B is the #3419 boundary victim — WITH an embedding, per the
        // issue's original repro.
        let b = create_with_embedding(&db, "B", &embedding_b);
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

    // The replayed post-persist write must be back in the VECTOR index too:
    // this is the exact loss mode Issue #3419 was reported with.
    let similar = db
        .similarity_search(SimilarityQuery::from_node(a).k(5))
        .expect("similarity search must work after crash recovery");
    assert!(
        similar.iter().any(|(id, _)| *id == b),
        "node B (the post-persist boundary victim) must be findable via \
         find_similar after crash+reopen; got: {similar:?}"
    );
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

    // PR #3428 review: LSN TOTAL ORDERING across all sessions. Read every
    // entry back from the on-disk segments through the public segment reader
    // and assert no LSN was ever issued twice — the #3420 failure mode was
    // precisely duplicate LSNs across restart-created segments.
    let entries = aletheiadb::storage::wal::segment_reader::read_entries_from_dir(
        &db_path.join("wal"),
        aletheiadb::storage::wal::LSN(1),
    )
    .expect("read all WAL entries from disk");
    assert!(!entries.is_empty(), "expected durable WAL entries on disk");
    let mut seen = std::collections::HashSet::new();
    for entry in &entries {
        assert!(
            seen.insert(entry.lsn),
            "duplicate LSN {} found across WAL segments — allocator was \
             re-seeded incorrectly across sessions",
            entry.lsn.0
        );
    }
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

/// T8 — manifest-LSN floor (PR #3428 review; kills the surviving mutant in
/// the `db/config.rs` manifest-floor seeding).
///
/// Session 1 writes, persists, and drops CLEANLY; then every WAL segment is
/// deleted (simulating LSN-based WAL truncation after cold-storage
/// migration). On reopen the segment scan finds NOTHING, so the allocator
/// would restart at LSN 1 — BELOW the manifest LSN — unless the manifest
/// floor re-seeds it. A new write must then survive one more crash cycle:
/// without the floor, the write's LSN lands below the manifest LSN and the
/// next startup's differential replay silently skips it.
#[test]
fn t8_manifest_lsn_floor_survives_wal_truncation() {
    let _g = lock();
    let tmp = tempdir().unwrap();
    let db_path = tmp.path();

    // Session 1: clean shutdown (final persist runs, manifest advances).
    let a = {
        let db = open(db_path);
        let a = create(&db, "A");
        db.persist_indexes().expect("manual persist");
        a
        // clean drop
    };

    // Simulate WAL truncation: remove every segment file.
    let wal_dir = db_path.join("wal");
    let mut removed = 0usize;
    for entry in std::fs::read_dir(&wal_dir).expect("read wal dir") {
        let entry = entry.expect("dir entry");
        if entry.file_name().to_string_lossy().contains(".log") {
            std::fs::remove_file(entry.path()).expect("remove segment");
            removed += 1;
        }
    }
    assert!(removed > 0, "expected at least one WAL segment to remove");

    let manifest = manifest_lsn(db_path);
    assert!(manifest > 1, "manifest LSN must have advanced in session 1");

    // Session 2: allocator must be floored at the manifest LSN even though
    // the segment scan found nothing; the new write must survive a crash.
    let b = {
        let db = open(db_path);
        assert_node_present(&db, a, "A");
        let next_lsn = db.__test_current_wal_lsn();
        assert!(
            next_lsn >= manifest,
            "allocator not floored at manifest LSN after WAL truncation: \
             next LSN {next_lsn} < manifest {manifest}"
        );
        let b = create(&db, "B");
        std::mem::forget(db);
        b
    };

    // Session 3: B must have survived.
    let db = open(db_path);
    assert_node_present(&db, a, "A");
    assert_node_present(&db, b, "B");
    assert_history_len(&db, b, "B", 1);
}

/// T9 — multi-segment WAL rotation (PR #3428 review): recovery and allocator
/// seeding must span ALL rotated segments — `max_lsn_in_dir` has to return
/// the maximum across every segment, not just the first one it reads.
///
/// A tiny segment size forces rotation mid-session; after a crash the reopen
/// must recover every write and keep allocating LSNs above everything on
/// disk, then survive one more crash cycle.
#[test]
fn t9_recovery_and_seeding_across_rotated_segments() {
    let _g = lock();
    let tmp = tempdir().unwrap();
    let db_path = tmp.path();

    const WRITES: usize = 40;

    let rotating_config = || {
        AletheiaDBConfig::builder()
            .wal(
                WalConfigBuilder::new()
                    .wal_dir(db_path.join("wal"))
                    .durability_mode(DurabilityMode::Synchronous)
                    .segment_size(2048)
                    .expect("segment size >= minimum")
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
    };
    let open_rotating =
        || AletheiaDB::with_unified_config(rotating_config()).expect("open database");

    // Session 1: enough padded writes to force several rotations, then crash.
    let ids: Vec<(NodeId, String)> = {
        let db = open_rotating();
        let padding = "x".repeat(200);
        let ids = (0..WRITES)
            .map(|i| {
                let name = format!("seg-{i}");
                let id = db
                    .create_node(
                        "LsnRegression",
                        PropertyMapBuilder::new()
                            .insert("name", name.as_str())
                            .insert("padding", padding.as_str())
                            .build(),
                    )
                    .unwrap_or_else(|e| panic!("create_node({name}) failed: {e}"));
                (id, name)
            })
            .collect();
        std::mem::forget(db);
        ids
    };

    // Rotation must actually have happened: multiple segments on disk.
    let segment_count = std::fs::read_dir(db_path.join("wal"))
        .expect("read wal dir")
        .filter(|e| {
            e.as_ref()
                .map(|e| e.file_name().to_string_lossy().contains(".log"))
                .unwrap_or(false)
        })
        .count();
    assert!(
        segment_count > 1,
        "test setup must force WAL rotation (found {segment_count} segment)"
    );

    // Session 2: recovery must restore every write from all segments and the
    // allocator must be seeded above the global max LSN.
    let session_end_lsn;
    let extra = {
        let db = open_rotating();
        for (id, name) in &ids {
            assert_node_present(&db, *id, name);
            assert_history_len(&db, *id, name, 1);
        }
        assert_eq!(db.node_count(), WRITES);
        let extra = create(&db, "post-rotation");
        session_end_lsn = db.__test_current_wal_lsn();
        std::mem::forget(db);
        extra
    };

    // Session 3: the post-rotation write survived, LSNs stayed monotonic and
    // unique across all segments.
    let db = open_rotating();
    assert!(db.__test_current_wal_lsn() >= session_end_lsn);
    assert_node_present(&db, extra, "post-rotation");
    assert_history_len(&db, extra, "post-rotation", 1);

    let entries = aletheiadb::storage::wal::segment_reader::read_entries_from_dir(
        &db_path.join("wal"),
        aletheiadb::storage::wal::LSN(1),
    )
    .expect("read all WAL entries");
    let mut seen = std::collections::HashSet::new();
    for entry in &entries {
        assert!(
            seen.insert(entry.lsn),
            "duplicate LSN {} across rotated segments",
            entry.lsn.0
        );
    }
}

/// T10 — deleted-entity id reuse across restart (PR #3428 review probe;
/// guards fix 1 — historical id-generator seeding — and fix 2 — the
/// tombstone-aware create guard — end-to-end).
///
/// S1: create A, B; delete B; persist; crash.
/// S2: reopen; create C. Pre-fix, the id generators were seeded from the
///     LIVE snapshot only (A), so C RE-USED deleted B's NodeId; crash.
/// S3: reopen; replaying C's create found B's tombstone at the head of the
///     shared id's history and (pre-fix) skipped appending any version — C
///     silently pointed at B's tombstone: bi-temporal corruption.
#[test]
fn t10_deleted_node_id_is_never_reissued_across_restart() {
    let _g = lock();
    let tmp = tempdir().unwrap();
    let db_path = tmp.path();

    // S1: A and B live; B deleted; persist (snapshot: A live, B only in
    // history); crash.
    let (a, b) = {
        let db = open(db_path);
        let a = create(&db, "A");
        let b = create(&db, "B");
        db.write(|tx| tx.delete_node(b)).expect("delete B");
        db.persist_indexes().expect("manual persist");
        std::mem::forget(db);
        (a, b)
    };

    // S2: C must get a FRESH id, not deleted B's.
    let c = {
        let db = open(db_path);
        assert_node_present(&db, a, "A");
        let c = create(&db, "C");
        assert_ne!(
            c, b,
            "id generator reissued deleted node B's id to C — seeding must \
             cover HISTORICAL (deleted) entities, not just the live snapshot"
        );
        std::mem::forget(db);
        c
    };

    // S3: everything intact after replaying C's create.
    let db = open(db_path);
    assert_node_present(&db, a, "A");
    assert_node_present(&db, c, "C");
    assert_history_len(&db, c, "C", 1);
    assert!(
        db.get_node(b).is_err(),
        "deleted node B must not resurrect in current state"
    );
    // B's bi-temporal history is intact: create + delete tombstone.
    let b_history = db
        .get_node_history(b)
        .expect("deleted B's history must remain queryable");
    assert_eq!(
        b_history.versions.len(),
        2,
        "B's history must be exactly create + tombstone"
    );
}

/// T11 — CI-proven regression on this branch: a torn tail in a WAL segment
/// must not fail database construction.
///
/// The #3420 seeding scan (`seed_lsn_allocator_from_segments` →
/// `max_lsn_in_dir`) runs inside every constructor. Pre-fix it fully parsed
/// every segment and propagated parse errors, so a torn entry — valid
/// 24-byte entry header followed by operation-type byte 0, the exact shape
/// from the CI failure in `tests/temporal_vector_integration.rs` (which
/// shares the cwd-relative default WAL dir with concurrently running tests)
/// — bricked `with_config`/`with_full_config` with
/// `Storage(CorruptedData("Unknown WAL operation type: 0"))`. The same shape
/// arises in production from a genuine crash-torn tail, permanently bricking
/// startup. Post-fix the scan stops at the undecodable entry, seeds from the
/// decodable prefix, and construction succeeds.
///
/// Uses `with_wal_config` (→ `with_full_config`), the exact constructor path
/// the CI failure went through: it seeds the allocator from segments but
/// performs no WAL replay.
#[test]
fn t11_constructor_tolerates_torn_wal_tail() {
    let _g = lock();
    let tmp = tempdir().unwrap();
    let db_path = tmp.path();

    let wal_config = || {
        WalConfigBuilder::new()
            .wal_dir(db_path.join("wal"))
            .durability_mode(DurabilityMode::Synchronous)
            .build()
    };

    // Session 1: real fsynced writes so the segment has a decodable prefix.
    let session1_end_lsn = {
        let db = AletheiaDB::with_wal_config(wal_config()).expect("open session 1");
        create(&db, "A");
        create(&db, "B");
        let lsn = db.__test_current_wal_lsn();
        std::mem::forget(db); // crash: leave segments exactly as fsynced
        lsn
    };
    assert!(session1_end_lsn > 1, "session 1 must have allocated LSNs");

    // Append a torn entry to the HIGHEST segment: 24 bytes of plausible
    // entry header (nonzero LSN + timestamp + checksum) followed by
    // operation-type byte 0 — undecodable, non-truncated, non-zeroed.
    let wal_dir = db_path.join("wal");
    let last_segment = std::fs::read_dir(&wal_dir)
        .expect("read wal dir")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|ext| ext == "log"))
        .max()
        .expect("at least one WAL segment");
    let mut torn = Vec::new();
    torn.extend_from_slice(&999u64.to_le_bytes()); // LSN (nonzero)
    torn.extend_from_slice(&[1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]); // timestamp
    torn.extend_from_slice(&[0xAB; 4]); // checksum (garbage)
    torn.push(0); // op-type 0 — "Unknown WAL operation type: 0"
    {
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&last_segment)
            .expect("open segment for append");
        file.write_all(&torn).expect("append torn entry");
        file.sync_all().expect("sync torn entry");
    }

    // Pre-fix: this constructor returned
    // Err(Storage(CorruptedData("Unknown WAL operation type: 0"))).
    let db = AletheiaDB::with_wal_config(wal_config())
        .expect("constructor must tolerate a torn WAL tail");

    // The allocator must still be seeded from the decodable prefix: new LSNs
    // continue above everything session 1 durably wrote.
    let seeded_lsn = db.__test_current_wal_lsn();
    assert!(
        seeded_lsn >= session1_end_lsn,
        "allocator under-seeded after torn-tail scan: next LSN {seeded_lsn} \
         < session 1 end {session1_end_lsn}"
    );
}

// ──────────────────────────────────────────────────────────────
// Issue #3429 — single-pass WAL startup fold
// ──────────────────────────────────────────────────────────────

/// Issue #3429: startup folds the three separate full WAL scans (max-LSN
/// allocator seed, constraint declaration net-state, differential replay) into
/// a single decode. This test pins the two invariants that fold must preserve
/// and that a naive fold would break:
///
///   1. A unique constraint DECLARED BEFORE the index snapshot LSN — i.e. below
///      the LSN the differential replay starts at — must still be recovered.
///      The constraint net-state is folded from the *whole* WAL history
///      (`apply_constraint_declarations` over every entry from LSN 0), not just
///      the post-manifest suffix fed to the differential replay. A fold that
///      only inspected the replay suffix would silently drop it.
///   2. A node WRITTEN AFTER the persist (a genuine post-snapshot tail) is
///      recovered by the differential replay driven off the *same* single read
///      (the LSN-filtered suffix), with no lost or duplicated versions, and the
///      allocator is seeded past every durable LSN.
///
/// Layout: declare constraint → create A → persist_indexes (manifest LSN now
/// sits above the constraint declaration) → create tail B → hard crash
/// (mem::forget) → reopen.
#[test]
fn t3429_below_manifest_constraint_and_tail_survive_single_pass_startup() {
    let _g = lock();
    let tmp = tempdir().unwrap();
    let db_path = tmp.path();

    let (a, b, manifest_after_persist) = {
        let db = open(db_path);
        // Constraint declaration is written to the WAL here, BEFORE the persist.
        db.unique_constraint("Person", "email")
            .enable()
            .expect("enable unique constraint");
        let a = db
            .create_node("Person", aletheiadb::properties! { "email" => "a@x" })
            .expect("create A");

        // Snapshot: the manifest LSN captured here is strictly above the
        // constraint declaration's LSN, so the next startup's differential
        // replay begins past it.
        db.persist_indexes().expect("manual persist");
        let manifest_after_persist = manifest_lsn(db_path);

        // Genuine post-snapshot tail write, recovered only via differential
        // replay of the single read's LSN-filtered suffix.
        let b = db
            .create_node("Person", aletheiadb::properties! { "email" => "b@x" })
            .expect("create tail B");

        // Hard crash: no Drop, no shutdown persist.
        std::mem::forget(db);
        (a, b, manifest_after_persist)
    };

    assert!(
        manifest_after_persist > 1,
        "manifest LSN must sit above the constraint declaration for this test \
         to exercise below-manifest constraint recovery (got {manifest_after_persist})"
    );

    // Reopen: exactly one full WAL decode now feeds seed + constraints + replay.
    let db = open(db_path);

    // Invariant 1: below-manifest constraint recovered and still ENFORCED.
    let active = db.list_unique_constraints();
    assert!(
        active.iter().any(|(l, p)| l == "Person" && p == "email"),
        "below-manifest constraint Person.email must survive single-pass startup: {active:?}"
    );
    db.create_node("Person", aletheiadb::properties! { "email" => "a@x" })
        .expect_err("constraint must still reject the duplicate email after reopen");

    // Invariant 2: both the pre-persist node and the post-persist tail recovered
    // cleanly (no loss, no boundary double-apply), and a distinct email is still
    // allowed.
    let node_a = db
        .get_node(a)
        .expect("pre-persist node A lost after recovery");
    assert_eq!(node_a.properties.get("email"), Some(&"a@x".into()));
    let node_b = db
        .get_node(b)
        .expect("post-persist tail node B lost after recovery");
    assert_eq!(node_b.properties.get("email"), Some(&"b@x".into()));
    assert_history_len(&db, a, "A", 1);
    assert_history_len(&db, b, "B", 1);
    db.create_node("Person", aletheiadb::properties! { "email" => "c@x" })
        .expect("a distinct email must still be allowed after reopen");

    // Allocator seeded past every durable LSN from the same single read.
    assert!(
        db.__test_current_wal_lsn() > manifest_after_persist,
        "allocator must be seeded past the persisted tail after single-pass startup"
    );
}
