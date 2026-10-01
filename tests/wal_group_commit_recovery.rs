//! Crash-recovery regression tests for the two gaps in Issue #3430:
//!
//! - **Gap 1 — GroupCommit crash recovery**: every crash-recovery test in
//!   `tests/lsn_recovery_regression.rs` runs under
//!   `DurabilityMode::Synchronous`, but `AletheiaDB::open(path)` — the
//!   documented one-line durable entry point — defaults to
//!   `DurabilityMode::GroupCommit { max_delay_ms: 10, max_batch_size: 200 }`
//!   (see `durable_config_for_data_dir` in `src/config.rs`). GroupCommit
//!   batches fsyncs, so its crash window differs and was unexercised. These
//!   tests re-run the #3419 (persist-between-two-writes boundary) and #3420
//!   (LSN allocator seeding across sessions) recovery invariants under
//!   GroupCommit.
//!
//! - **Gap 2 — encrypted-WAL allocator seeding**: `seed_lsn_allocator_from_segments`
//!   (`src/db/config.rs`) passes the configured WAL cipher into
//!   `segment_reader::max_lsn_in_dir` so encrypted segments are scanned at
//!   startup to seed the LSN allocator (#3420, encrypted variant). These tests
//!   write an encrypted WAL, restart, and assert the allocator is seeded past
//!   the encrypted segments' max LSN with no LSN reuse. They also pin the
//!   documented (intentional) leniency discrepancy between the seeding scan
//!   and the recovery read path when an encrypted segment exists but no cipher
//!   is configured.
//!
//! **Why these are deterministic, not sleep-and-hope**: in GroupCommit mode a
//! write call blocks until its batch's fsync completes (ACID-compliant group
//! commit — see `tests/durability_modes.rs::test_group_commit_respects_max_delay`,
//! which relies on a single write returning only after the flush). So when
//! `create_node` / `write` **returns Ok**, the entry is already durable on
//! disk. The crash is then simulated with `std::mem::forget(db)` (leaks the
//! handle so `Drop`/final-persist never runs) and the reopen must recover
//! every acknowledged write. No test asserts durability of a write whose batch
//! never flushed, and no test times a sleep against `max_delay_ms`.
//!
//! Crash simulation and the inert-persistence-policy discipline mirror
//! `tests/lsn_recovery_regression.rs` exactly (leaked background workers must
//! never persist "post-crash" and invalidate the simulation). All tests are
//! serialized behind a file-level mutex because index persistence round-trips
//! the process-global string interner.

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

/// The exact GroupCommit durability the `open(path)` default uses
/// (`durable_config_for_data_dir` in `src/config.rs`).
const OPEN_DEFAULT_GROUP_COMMIT: DurabilityMode = DurabilityMode::GroupCommit {
    max_delay_ms: 10,
    max_batch_size: 200,
};

/// Serialize tests: index persistence round-trips the global string interner.
static GC_RECOVERY_TEST_MUTEX: Mutex<()> = Mutex::new(());

/// Lock helper that survives poisoning (a failing test must not cascade).
fn lock() -> std::sync::MutexGuard<'static, ()> {
    GC_RECOVERY_TEST_MUTEX
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Persistence policies that never fire on their own — keeps leaked
/// (crash-simulated) background workers inert so they cannot persist state
/// "post-crash" and invalidate the crash simulation.
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

/// Durable config: **GroupCommit** WAL (the `open()` default) + index
/// persistence with load-on-startup. Explicit tempdir-rooted paths.
fn group_commit_config(db_path: &Path) -> AletheiaDBConfig {
    AletheiaDBConfig::builder()
        .wal(
            WalConfigBuilder::new()
                .wal_dir(db_path.join("wal"))
                .durability_mode(OPEN_DEFAULT_GROUP_COMMIT)
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

fn open_gc(db_path: &Path) -> AletheiaDB {
    AletheiaDB::with_unified_config(group_commit_config(db_path))
        .expect("open GroupCommit database")
}

/// Create a node in the database's default durability mode. In GroupCommit
/// mode this call blocks until the group-commit fsync completes, so once it
/// returns the write is durable (this is the acknowledgment barrier the crash
/// tests depend on).
fn create(db: &AletheiaDB, name: &str) -> NodeId {
    db.create_node(
        "GcRecovery",
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

/// GC1 — GroupCommit acknowledged-writes-survive-crash (Gap 1a).
///
/// Under GroupCommit, every write that has *returned* has already been fsynced
/// by its group-commit batch. This is the crash-window that Synchronous tests
/// never exercise. Pattern:
///
/// create A → persist_indexes (capture manifest) → create B, C → hard crash
/// (`mem::forget`) → reopen: A, B, and C must all be present exactly once.
///
/// B and C are the post-persist WAL tail: they exist only in the WAL, not the
/// index snapshot, so recovery MUST replay the post-manifest tail. If
/// GroupCommit recovery were broken (tail lost, or replay window off), the
/// `assert_node_present` for B/C would panic — that is the genuine-failure
/// property this test guards.
#[test]
fn gc1_acknowledged_writes_survive_group_commit_crash() {
    let _g = lock();
    let tmp = tempdir().unwrap();
    let db_path = tmp.path();

    let (a, b, c, manifest_after_persist) = {
        let db = open_gc(db_path);
        let a = create(&db, "A");
        db.persist_indexes().expect("manual persist");
        let manifest_after_persist = manifest_lsn(db_path);
        // B and C are acknowledged (group-commit-fsynced) AFTER the snapshot,
        // so they live only in the post-manifest WAL tail.
        let b = create(&db, "B");
        let c = create(&db, "C");
        // Hard crash: no Drop, no shutdown persist.
        std::mem::forget(db);
        (a, b, c, manifest_after_persist)
    };

    // Crash-simulation sanity: mem::forget really skipped the Drop-time final
    // persist — the on-disk manifest LSN must NOT have advanced past the
    // manual persist. If it had, the "recovery" below would be served from the
    // snapshot, not a real WAL replay, and the test would be vacuous.
    assert_eq!(
        manifest_lsn(db_path),
        manifest_after_persist,
        "crash simulation invalid: manifest advanced after mem::forget"
    );

    let db = open_gc(db_path);
    for (id, name) in [(a, "A"), (b, "B"), (c, "C")] {
        assert_node_present(&db, id, name);
        assert_history_len(&db, id, name, 1);
    }
    assert_eq!(
        db.node_count(),
        3,
        "recovered node count must equal acknowledged writes: fewer = lost, more = duplicated"
    );
}

/// GC2 — GroupCommit persist-between-two-writes boundary (#3419 under
/// GroupCommit, Gap 1b).
///
/// The #3419 hazard: the manifest stores the NEXT-to-allocate LSN captured at
/// snapshot time; the first write AFTER a persist receives exactly that LSN.
/// Startup must replay from the manifest LSN **inclusive** (a pre-fix
/// off-by-one replayed from `manifest.lsn.next()` and silently dropped that
/// first post-persist write). The complementary hazard is double-apply: if a
/// racing write was captured in the snapshot AND is >= the manifest LSN, the
/// inclusive replay re-applies it and must be idempotent.
///
/// This pins the boundary under GroupCommit: create A → persist → create B
/// (B's LSN == manifest.lsn) → crash → reopen. B must be present with EXACTLY
/// one history version — not zero (lost, the #3419 off-by-one) and not two
/// (double-applied).
#[test]
fn gc2_persist_between_two_writes_boundary() {
    let _g = lock();
    let tmp = tempdir().unwrap();
    let db_path = tmp.path();

    let (a, b) = {
        let db = open_gc(db_path);
        let a = create(&db, "A");
        db.persist_indexes().expect("manual persist");

        // The manifest captured the next-to-allocate LSN; the next write lands
        // exactly on it — the #3419 boundary.
        let manifest = manifest_lsn(db_path);
        let lsn_before_b = db.__test_current_wal_lsn();
        assert_eq!(
            lsn_before_b, manifest,
            "post-persist boundary precondition: next LSN {lsn_before_b} must equal \
             the captured manifest LSN {manifest} so B lands exactly on the boundary"
        );

        let b = create(&db, "B");
        std::mem::forget(db);
        (a, b)
    };

    let db = open_gc(db_path);
    assert_node_present(&db, a, "A");
    // The load-bearing assertions: B is neither lost (#3419 off-by-one) nor
    // double-applied by the inclusive replay.
    assert_node_present(&db, b, "B");
    assert_history_len(&db, b, "B", 1);
    assert_eq!(
        db.node_count(),
        2,
        "no loss and no duplication at the boundary"
    );
}

/// GC3 — GroupCommit LSN-allocator seeding across sessions (#3420 under
/// GroupCommit, Gap 1c).
///
/// Three-session repro: the allocator must continue monotonically across
/// restarts, never re-issuing an LSN already durable on disk.
///
/// S1: write A + persist + CLEAN drop (shutdown persist runs).
/// S2: reopen, write D, hard crash.
/// S3: reopen → D must be present; LSNs stayed monotonic and unique.
///
/// Pre-fix (#3420) failure mode: S2's allocator restarted at LSN 1, so D's WAL
/// entry received an LSN below the manifest LSN and S3's differential replay
/// never saw it (and duplicate LSNs across segments broke total ordering).
#[test]
fn gc3_lsn_allocation_continues_across_group_commit_sessions() {
    let _g = lock();
    let tmp = tempdir().unwrap();
    let db_path = tmp.path();

    // Session 1: clean shutdown (Drop runs the final shutdown persist).
    let (a, s1_end_lsn) = {
        let db = open_gc(db_path);
        let a = create(&db, "A");
        db.persist_indexes().expect("manual persist");
        let s1_end_lsn = db.__test_current_wal_lsn();
        (a, s1_end_lsn)
        // clean drop here
    };

    // Session 2: allocator must resume ABOVE session 1's durable LSNs, not at 1.
    let (d, s2_end_lsn) = {
        let db = open_gc(db_path);
        assert_node_present(&db, a, "A");
        let s2_start_lsn = db.__test_current_wal_lsn();
        assert!(
            s2_start_lsn >= s1_end_lsn,
            "allocator not seeded across GroupCommit restart: session-2 start LSN \
             {s2_start_lsn} < session-1 end LSN {s1_end_lsn}"
        );
        let d = create(&db, "D");
        let s2_end_lsn = db.__test_current_wal_lsn();
        std::mem::forget(db);
        (d, s2_end_lsn)
    };

    // Session 3: D survived, allocator still monotonic, and no LSN was ever
    // issued twice across the restart-created segments (the exact #3420
    // corruption signature).
    let db = open_gc(db_path);
    assert_node_present(&db, a, "A");
    assert_node_present(&db, d, "D");
    assert_history_len(&db, d, "D", 1);
    assert!(
        db.__test_current_wal_lsn() >= s2_end_lsn,
        "session-3 allocator regressed below session-2 end LSN"
    );

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
            "duplicate LSN {} across GroupCommit sessions — allocator re-seeded incorrectly",
            entry.lsn.0
        );
    }
}

// =============================================================================
// Gap 2 — encrypted-WAL allocator seeding (feature `encryption`)
// =============================================================================

#[cfg(feature = "encryption")]
mod encrypted {
    use super::{
        AletheiaDB, AletheiaDBConfig, NodeId, Path, PersistenceConfig, WalConfigBuilder,
        assert_history_len, assert_node_present, create, inert_policies, lock, tempdir,
    };
    use aletheiadb::encryption::cipher::Cipher;
    use aletheiadb::encryption::key_provider::FileKeyProvider;
    use aletheiadb::encryption::{EncryptionConfig, EncryptionManager};
    use aletheiadb::storage::wal::DurabilityMode;
    use aletheiadb::storage::wal::LSN;
    use aletheiadb::storage::wal::segment_reader;
    use std::sync::Arc;

    /// Durable config with an **encrypted** WAL (file-based MEK) + index
    /// persistence. Synchronous durability keeps the encrypted-seeding focus
    /// crisp: every returned write is fsynced, so `current_lsn` == max durable
    /// LSN + 1 with no batching ambiguity.
    fn encrypted_config(db_path: &Path, key_path: &Path) -> AletheiaDBConfig {
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
            .encryption(EncryptionConfig::file_based(key_path))
            .build()
    }

    /// Build the WAL cipher from the same file-based key an open database uses,
    /// so tests can independently scan encrypted segments.
    fn wal_cipher(key_path: &Path) -> Arc<dyn Cipher> {
        let mgr = EncryptionManager::from_config(&EncryptionConfig::file_based(key_path))
            .expect("build encryption manager from key file");
        Arc::clone(mgr.wal_cipher())
    }

    fn open_encrypted(db_path: &Path, key_path: &Path) -> AletheiaDB {
        AletheiaDB::with_unified_config(encrypted_config(db_path, key_path))
            .expect("open encrypted database")
    }

    /// E1 — encrypted-WAL allocator seeding across sessions (#3420 encrypted
    /// variant, Gap 2a).
    ///
    /// Writes an encrypted WAL, restarts, and asserts the LSN allocator is
    /// seeded PAST the encrypted segments' max LSN (proving
    /// `seed_lsn_allocator_from_segments` decrypted the segments to find their
    /// max), and that a subsequent write gets a strictly-greater LSN — no LSN
    /// reuse across the restart.
    #[test]
    fn e1_encrypted_wal_allocator_seeded_across_sessions() {
        let _g = lock();
        let tmp = tempdir().unwrap();
        let db_path = tmp.path();
        let key_path = db_path.join("mek.key");
        FileKeyProvider::generate_key_file(&key_path).expect("generate MEK key file");
        let cipher = wal_cipher(&key_path);
        let wal_dir = db_path.join("wal");

        // Session 1: three encrypted, fsynced writes; then crash.
        let (a, b, c, s1_end_lsn) = {
            let db = open_encrypted(db_path, &key_path);
            let a = create(&db, "A");
            let b = create(&db, "B");
            let c = create(&db, "C");
            let s1_end_lsn = db.__test_current_wal_lsn();
            std::mem::forget(db);
            (a, b, c, s1_end_lsn)
        };

        // The segments on disk are genuinely encrypted: reading them WITHOUT a
        // cipher fails, and scanning WITH the cipher yields a real max LSN.
        assert!(
            segment_reader::read_entries_from_dir_with_cipher(&wal_dir, LSN(1), None).is_err(),
            "segments must be encrypted: a cipher-less recovery read must fail"
        );
        let encrypted_max = segment_reader::max_lsn_in_dir(&wal_dir, Some(&cipher))
            .expect("scan encrypted segments")
            .expect("encrypted segments must contain at least one LSN");
        // Synchronous mode: current_lsn is exactly max durable LSN + 1.
        assert_eq!(
            s1_end_lsn,
            encrypted_max.0 + 1,
            "next-to-allocate LSN must be one past the encrypted segments' max LSN"
        );

        // Session 2: reopen must decrypt-replay (A/B/C recovered) AND seed the
        // allocator past the encrypted max LSN.
        let (d, lsn_before_d) = {
            let db = open_encrypted(db_path, &key_path);
            assert_node_present(&db, a, "A");
            assert_node_present(&db, b, "B");
            assert_node_present(&db, c, "C");

            let lsn_before_d = db.__test_current_wal_lsn();
            assert!(
                lsn_before_d > encrypted_max.0,
                "allocator not seeded from encrypted segments: next LSN {lsn_before_d} \
                 is not past the encrypted max LSN {}",
                encrypted_max.0
            );

            // The next write must NOT reuse any LSN already durable on disk.
            let d = create(&db, "D");
            std::mem::forget(db);
            (d, lsn_before_d)
        };

        // Session 3: D survived and every LSN on disk is unique (no reuse).
        let db = open_encrypted(db_path, &key_path);
        assert_node_present(&db, d, "D");
        assert_history_len(&db, d, "D", 1);

        let entries =
            segment_reader::read_entries_from_dir_with_cipher(&wal_dir, LSN(1), Some(&cipher))
                .expect("decrypt-read all encrypted WAL entries");
        let mut seen = std::collections::HashSet::new();
        let mut d_seen_above_max = false;
        for entry in &entries {
            assert!(
                seen.insert(entry.lsn),
                "duplicate LSN {} across encrypted segments — allocator reused a durable LSN",
                entry.lsn.0
            );
            if entry.lsn.0 >= lsn_before_d {
                d_seen_above_max = true;
            }
        }
        assert!(
            d_seen_above_max,
            "the post-restart write D must have been assigned an LSN strictly above \
             the encrypted segments' pre-restart max"
        );
        // Keep `NodeId` used even if the compiler ever changes inference here.
        let _: NodeId = d;
    }

    /// E2 — encrypted segment present but NO cipher configured (Gap 2b).
    ///
    /// This pins the **intentional, documented** leniency discrepancy between
    /// the two encrypted-segment consumers when no cipher is available:
    ///
    /// - the LSN **seeding scan** (`max_lsn_in_dir`, used by
    ///   `seed_lsn_allocator_from_segments` in every constructor) SKIPS the
    ///   encrypted segment with a warning and returns `Ok(None)` — by design,
    ///   because skipping cannot under-seed relative to what replay would apply,
    ///   and it keeps construction usable when another process is mid-creating a
    ///   segment in a shared WAL dir (see the rationale comment in
    ///   `max_lsn_in_segment`);
    /// - the **recovery read path** (`read_entries_from_dir_with_cipher`)
    ///   propagates a hard `Encryption` error — you cannot silently recover an
    ///   encrypted WAL without its key.
    ///
    /// This is asserting current behavior, not driving a production change; if
    /// this discrepancy is ever intentionally unified, this test documents what
    /// must change.
    #[test]
    fn e2_encrypted_segment_without_cipher_scan_skips_but_read_errors() {
        let _g = lock();
        let tmp = tempdir().unwrap();
        let db_path = tmp.path();
        let key_path = db_path.join("mek.key");
        FileKeyProvider::generate_key_file(&key_path).expect("generate MEK key file");
        let wal_dir = db_path.join("wal");

        // Produce at least one encrypted segment with real entries.
        {
            let db = open_encrypted(db_path, &key_path);
            create(&db, "A");
            create(&db, "B");
            std::mem::forget(db);
        }

        // Seeding scan without a cipher: lenient skip → Ok(None).
        let scanned = segment_reader::max_lsn_in_dir(&wal_dir, None).expect(
            "the LSN seeding scan must not propagate an error for a cipher-less encrypted segment",
        );
        assert!(
            scanned.is_none(),
            "seeding scan must SKIP a cipher-less encrypted segment (Ok(None)), got {scanned:?}"
        );

        // Recovery read path without a cipher: hard error.
        let read = segment_reader::read_entries_from_dir_with_cipher(&wal_dir, LSN(1), None);
        assert!(
            read.is_err(),
            "recovery read of a cipher-less encrypted segment must hard-error, got {read:?}"
        );
    }
}
