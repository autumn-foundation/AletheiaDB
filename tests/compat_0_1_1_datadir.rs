//! Cross-version compatibility: open a data directory written by the published
//! AletheiaDB **0.1.1** crate under the current trunk (0.2.0) code.
//!
//! # STATUS: RESOLVED (Issue #3746) — open() now REFUSES the corrupting tail
//!
//! Opening a real 0.1.1-written data directory that still carries an unreplayed
//! **pre-v13 WAL tail** used to silently **corrupt the interned-string labels of
//! every entity recovered from that tail** (see the `Observed failure` transcript
//! below). Rather than replay a tail whose raw interner ids can no longer be
//! resolved to their original strings, `AletheiaDB::open()` now **refuses** such a
//! directory with a FAILED_PRECONDITION-style error that points at
//! `docs/guides/migration-0.1-to-0.2.md` and tells the operator to drain /
//! checkpoint the WAL on 0.1.x before upgrading (`refuses_to_open_..._pre_v13_wal_tail`).
//! A cleanly-checkpointed 0.1.1 directory (zero replay) still opens with full
//! integrity (`checkpointed_..._with_full_integrity`), and a CURRENT-version
//! (v13+) unreplayed tail still opens cleanly (`current_version_..._opens`) — the
//! guard is scoped to the pre-v13 replay window only.
//!
//! ## Observed failure BEFORE the fix (verbatim, on a copy of the fixture)
//!
//! ```text
//! Index restoration completed successfully: 9 nodes, 9 edges loaded
//! Replaying 7 WAL entries
//! node_count = 13   edge_count = 12          (fixture ground truth: 12 / 12)
//!
//! node 0 Alice   label=Person    OK    node 5 Globex  label=Company  OK
//! node 1 Bob     label=Person    OK    node 6 Initech label=Company  OK
//! node 2 Carol   label=Person    OK    node 7 London  label=City     OK
//! node 3 Dave    label=Person    OK    node 8 Boston  label=City     OK
//! node 4 Acme    label=Company   OK
//!
//! node 9  (Sentinel)  label=Interned(37)  <-- UNRESOLVED interned string
//!                                             (0.1.1 dropped this node via its
//!                                              snapshot-boundary off-by-one;
//!                                              trunk replays it but cannot
//!                                              resolve its label)
//! node 10 Eve      label="founded"   <-- CORRUPT, must be "Person"
//! node 11 Frank    label="founded"   <-- CORRUPT, must be "Person"
//! node 12 Umbrella label="since"     <-- CORRUPT, must be "Company"
//!
//! get_node_history(Bob) = 2 versions, age 41 then 42                  OK
//! get_node_at_time(Bob, before_update, now) = Storage(NodeNotFound)   (0.1.1
//!     restore-path limitation NOT fixed on trunk — same as 0.1.1)
//! ```
//!
//! The batch-2 node **names** (`Eve`/`Frank`/`Umbrella`) survive, but their
//! **labels** resolve to unrelated interned strings — in fact to *property
//! keys* from the same dataset (`founded`, `since`). This is a classic
//! interner-ID drift: the 0.1.1 WAL encodes labels as process-local interner
//! IDs, and after trunk rebuilds the interner from the 0.1.1 index snapshot
//! those IDs no longer denote the same strings, so replayed entities are
//! mislabeled. Batch-1 (snapshot) labels are unaffected because they were
//! materialized directly from the snapshot's string table.
//!
//! ## Why refusing is the right fix for the in-place 0.1.x -> 0.2.0 upgrade
//!
//! The documented, supported upgrade path is an **in-place open** (there is no
//! 0.1.x `.albk` backup off-ramp). Any 0.1.x deployment with committed writes
//! after its last index-persistence snapshot — i.e. any non-cleanly-snapshotted
//! shutdown — would, on first 0.2.0 open, silently mislabel exactly those tail
//! entities. Because the pre-v13 WAL never stored the label *string* (only a raw
//! process-local interner id), there is nothing to recover it from at replay
//! time; the only safe answer is to refuse and direct the operator to drain the
//! WAL on 0.1.x first (`persist_indexes()` + graceful shutdown), which captures
//! every label as a string in the index snapshot.
//!
//! ## What the fixture is
//!
//! `tests/fixtures/compat/aletheiadb-0.1.1/` (52K) was produced by the released
//! `aletheiadb = "=0.1.1"` binary — a `wal/` segment plus a nested `indexes/`
//! snapshot with a real **unreplayed WAL tail** (batch 2: 3 nodes + 3 edges
//! written after the last snapshot). See `tests/fixtures/compat/README.md` for
//! the full regeneration recipe and ground truth. Regenerate ONLY via the
//! pinned 0.1.1 crate, never trunk.
//!
//! Run: `cargo test --test compat_0_1_1_datadir -- --nocapture`.

use aletheiadb::{AletheiaDB, NodeId, PropertyValue, StorageError, Timestamp, time};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// Process-global monotonic counter that makes every test tempdir path this
/// module hands out distinct, even when two constructors are invoked within the
/// same nanosecond on a coarse-resolution clock. Before this existed the two
/// 0.1.1 compat tests derived their tempdir name from ONLY pid + nanosecond
/// clock and, running concurrently in one binary (same pid) on macOS (coarser
/// `SystemTime` resolution), computed the SAME path and clobbered each other's
/// fixture copy. pid + nanos keep cross-process / cross-run uniqueness; this
/// atomic keeps per-call uniqueness within one process regardless of clock
/// resolution.
static FIXTURE_TEMP_SEQ: AtomicU64 = AtomicU64::new(0);

/// Build a collision-proof tempdir path under `std::env::temp_dir()`. Composes
/// the pid (cross-process), a caller `tag` (readability), a nanosecond timestamp
/// (cross-run), and a process-global monotonic sequence number (per-call
/// uniqueness, independent of clock resolution). Every fixture-copy / temp-dir
/// constructor in this file routes through here so no two calls can ever return
/// the same path within one process. We do not depend on the `tempfile` crate
/// (not a dev-dependency), so we build the path by hand.
fn unique_temp_dir(tag: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let seq = FIXTURE_TEMP_SEQ.fetch_add(1, Ordering::Relaxed);
    let unique = format!(
        "aletheiadb-compat-0_1_1-{}-{}-{}-{}",
        std::process::id(),
        tag,
        nanos,
        seq
    );
    std::env::temp_dir().join(unique)
}

/// Absolute path to the checked-in 0.1.1 WAL-tail fixture (never opened in place).
fn fixture_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("compat")
        .join("aletheiadb-0.1.1")
}

/// Absolute path to the checked-in 0.1.1 CLEANLY CHECKPOINTED fixture (WAL
/// drained; never opened in place). Deliberate opposite of `fixture_dir()`.
fn checkpointed_fixture_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("compat")
        .join("aletheiadb-0.1.1-checkpointed")
}

/// Recursively copy `src` into `dst`, preserving the `wal/` +
/// `indexes/indexes/...` subdirectory structure exactly.
fn copy_dir_recursive(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let target = dst.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir_recursive(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}

/// A tempdir holding a fresh copy of the fixture; cleaned up on drop. The unique
/// path is built by `unique_temp_dir` (pid + tag + nanos + a process-global
/// monotonic sequence), so two concurrent copies never collide even when the
/// wall clock returns identical nanos.
struct FixtureCopy {
    path: PathBuf,
}

impl FixtureCopy {
    fn new() -> Self {
        Self::from_source(&fixture_dir())
    }

    /// Copy an arbitrary checked-in fixture dir into a unique tempdir. Used by
    /// both the WAL-tail (`fixture_dir`) and checkpointed
    /// (`checkpointed_fixture_dir`) fixtures so neither is ever opened in place.
    fn from_source(src: &Path) -> Self {
        // Tag the tempdir with the source fixture's basename for readability;
        // the monotonic sequence in `unique_temp_dir` (not the tag) is what
        // guarantees uniqueness between the two concurrent compat tests.
        let tag = src
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("fixture");
        let path = unique_temp_dir(tag);
        let _ = std::fs::remove_dir_all(&path);
        copy_dir_recursive(src, &path).expect("copy 0.1.1 fixture into tempdir");
        FixtureCopy { path }
    }
}

impl Drop for FixtureCopy {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// Render a property value for diagnostics.
fn prop_str(v: Option<&PropertyValue>) -> String {
    match v {
        Some(PropertyValue::Bool(b)) => format!("Bool({b})"),
        Some(PropertyValue::Int(i)) => format!("Int({i})"),
        Some(PropertyValue::Float(f)) => format!("Float({f})"),
        Some(PropertyValue::String(s)) => format!("String({:?})", &**s),
        Some(other) => format!("{other:?}"),
        None => "<absent>".to_string(),
    }
}

/// Cross-version SAFETY check (Issue #3746): opening a 0.1.x data directory that
/// still carries an **unreplayed pre-v13 WAL tail** must be REFUSED, not silently
/// corrupted. Pre-v13 (0.1.x) WAL segments encode node/edge labels as raw
/// process-local interner ids; trunk rebuilds `GLOBAL_INTERNER` in a different
/// order, so replaying that tail resolves labels to WRONG strings (verified in
/// the `//! Observed failure` header above: node 10 -> "founded", node 12 ->
/// "since"). The accepted fix is REFUSE-DON'T-CORRUPT: `AletheiaDB::open()`
/// detects a pre-v13 tail inside the replay window and fails with a
/// FAILED_PRECONDITION-style error that points at the migration guide, so the
/// operator drains/checkpoints on 0.1.x first.
///
/// (This test replaces the former `#[ignore]`d `opens_..._with_full_integrity`
/// blocker reproduction: we no longer aspire to replay the corrupt tail
/// losslessly — there is no string to recover from a raw id — we refuse it.)
#[test]
fn refuses_to_open_0_1_1_data_dir_with_pre_v13_wal_tail() {
    // Open a COPY (opening the checked-in fixture in place would let WAL
    // replay / re-snapshot mutate the committed fixture).
    let copy = FixtureCopy::new();
    let result = AletheiaDB::open(&copy.path);

    let err = match result {
        Ok(_) => panic!(
            "trunk (0.2.0) must REFUSE to open a 0.1.x data dir with an unreplayed pre-v13 WAL \
             tail rather than silently corrupt its interned labels"
        ),
        Err(e) => e,
    };

    // The refusal must be the TYPED variant, not merely a message that happens to
    // contain the right substrings — this pins the classification (a caller
    // matching on `StorageError::PreV13WalTailRequiresMigration` gets a stable
    // FAILED_PRECONDITION contract, per src/mcp/error.rs / src/http/error.rs).
    assert!(
        matches!(
            &err,
            aletheiadb::Error::Storage(StorageError::PreV13WalTailRequiresMigration { .. })
        ),
        "refusal must be the typed StorageError::PreV13WalTailRequiresMigration variant; got: {err:?}"
    );

    let msg = err.to_string();

    // The refusal must point the operator at the migration guide.
    assert!(
        msg.contains("migration-0.1-to-0.2"),
        "refusal must cite docs/guides/migration-0.1-to-0.2.md; got: {msg}"
    );
    // ...and tell them HOW to make the directory safe: drain / checkpoint the WAL
    // (persist_indexes) before upgrading.
    let lower = msg.to_lowercase();
    assert!(
        lower.contains("drain")
            || lower.contains("checkpoint")
            || lower.contains("persist_indexes"),
        "refusal must direct the operator to drain/checkpoint the WAL before upgrading; got: {msg}"
    );
    // ...and it must name what it is protecting: interned labels that would
    // otherwise be silently corrupted.
    assert!(
        lower.contains("label"),
        "refusal must explain the interned-label corruption it prevents; got: {msg}"
    );
}

/// RAII tempdir guard: recursively removes its path on drop, even on panic
/// (Q6). Used by the positive-control test so no leaked
/// `aletheiadb-compat-0_1_1-*-current-tail-*` directory survives a run (success
/// OR failure). Mirrors `FixtureCopy`'s cleanup discipline for a dir we create
/// rather than copy.
struct TempDirGuard {
    path: PathBuf,
}

impl Drop for TempDirGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// Positive control (Issue #3746): the pre-v13 guard must NOT be over-broad — it
/// must still admit a genuine **v13+ (current-version) unreplayed WAL tail** and
/// replay it with labels intact.
///
/// # Why this test forces a REAL replay tail (non-vacuous)
///
/// The earlier version of this test merely created two nodes and dropped the db
/// without `persist_indexes()`. That proved nothing: `open()`'s drop-time final
/// persist drains the whole tail into the index snapshot, so on reopen the replay
/// window is EMPTY and the guard passes trivially (a vacuous positive control —
/// it never exercised a non-empty v13+ replay window at all).
///
/// This version forces a genuine post-snapshot WAL tail and PROVES it was
/// replayed, so it actually guards against the pre-v13 refusal being over-broad:
///
/// 1. Open with **`DurabilityMode::Synchronous`** (fsync on every commit) so each
///    write is durably on disk in the WAL *before* we abandon the handle — no
///    reliance on the drop-time final persist.
/// 2. `create_node(A)` (label `PersonAlpha`), then `persist_indexes()` — the index
///    snapshot now captures A at the manifest LSN (call it LSN₁).
/// 3. `create_node(B)` (label `CompanyBeta`) — B is durably in the WAL but AHEAD
///    of LSN₁, i.e. genuinely inside the reopen replay window.
/// 4. `std::mem::forget(db)` — abandon the handle WITHOUT running `Drop`, so the
///    drop-time final persist does NOT snapshot B. Synchronous durability already
///    put B on disk; the background persistence thread will not tick within this
///    sub-second window (default policy is 300 s / 1000-mutation), so B is left
///    undrained in the replay window. (`forget` leaks the first db's WAL
///    threads/handles; on Linux reopening the same dir is fine, and the RAII
///    `TempDirGuard` unlinks the dir at test end regardless.)
///
/// On reopen the snapshot restores ONLY A, so **B's presence with the correct
/// label is the proof** that `open()` permitted and replayed a non-empty v13+
/// tail — the pre-v13 guard is confirmed not over-broad. If the guard regresses
/// to refusing modern tails, phase 2 fails to open; if replay itself regresses, B
/// is missing.
#[test]
fn current_version_data_dir_with_unreplayed_tail_opens() {
    use aletheiadb::{AletheiaDBConfig, DurabilityMode, PersistenceConfig, WalConfigBuilder};

    // A fresh durable data dir written by THIS build; RAII-cleaned on drop (Q6).
    // Routed through the same collision-proof `unique_temp_dir` helper as the
    // fixture copies so it can never share a path with a concurrent constructor.
    let dir = TempDirGuard {
        path: unique_temp_dir("current-tail"),
    };
    let _ = std::fs::remove_dir_all(&dir.path);

    // Durable config with SYNCHRONOUS durability: a committed write is fsync'd to
    // the WAL before the call returns, so B survives an abandoned handle without a
    // graceful shutdown. Same layout as `AletheiaDB::open` (wal/ + indexes/,
    // load_on_startup) but with the durability mode swapped for determinism.
    let make_config = |data_dir: &Path| -> AletheiaDBConfig {
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
            .build()
    };

    // Phase 1: snapshot A, then write B AFTER the snapshot so B is a genuine
    // post-snapshot replay-window entry, and abandon the handle without draining.
    {
        let db = AletheiaDB::with_unified_config(make_config(&dir.path))
            .expect("open fresh current-version data dir");
        db.create_node(
            "PersonAlpha",
            aletheiadb::PropertyMapBuilder::new()
                .insert("name", "Ada")
                .build(),
        )
        .expect("create PersonAlpha node (A)");
        // Snapshot: captures A at the manifest LSN. Nothing after this is in the
        // snapshot.
        db.persist_indexes()
            .expect("persist_indexes must snapshot A at LSN1");
        db.create_node(
            "CompanyBeta",
            aletheiadb::PropertyMapBuilder::new()
                .insert("name", "Analytical")
                .build(),
        )
        .expect("create CompanyBeta node (B) — durably in WAL, ahead of LSN1");
        // Abandon WITHOUT Drop so the drop-time final persist does NOT drain B
        // into the snapshot. B remains an unreplayed v13+ WAL tail.
        std::mem::forget(db);
    }

    // Phase 2: reopen. The guard must NOT fire (this tail is v13+ string-labels).
    let db = AletheiaDB::with_unified_config(make_config(&dir.path))
        .expect("current-version data dir with an unreplayed v13+ WAL tail must open cleanly");

    let mut violations: Vec<String> = Vec::new();
    // A is restored from the snapshot.
    check_node(
        &db,
        0,
        "PersonAlpha",
        "name",
        "String(\"Ada\")",
        &mut violations,
    );
    // PROOF OF NON-VACUITY: the snapshot held ONLY A (id 0), so B (id 1) can be
    // present with the correct label ONLY IF open() replayed the non-empty v13+
    // WAL tail past the snapshot LSN. Its presence is what distinguishes this
    // positive control from the old vacuous one (empty replay window).
    check_node(
        &db,
        1,
        "CompanyBeta",
        "name",
        "String(\"Analytical\")",
        &mut violations,
    );
    assert!(
        violations.is_empty(),
        "\ncurrent-version WAL-tail reopen violations ({}):\n  - {}\n",
        violations.len(),
        violations.join("\n  - ")
    );

    // Drop the phase-2 db BEFORE the guard unlinks the dir (Q6): a live db still
    // holds WAL handles/threads on files under `dir.path`.
    drop(db);
    drop(dir);
}

/// Cross-version integrity check for the CLEANLY CHECKPOINTED / WAL-drained
/// 0.1.1 fixture (`tests/fixtures/compat/aletheiadb-0.1.1-checkpointed/`).
///
/// This is the deliberate opposite of `opens_0_1_1_data_dir_with_full_integrity`
/// above: that fixture carries an unreplayed WAL tail and reproduces the label
/// corruption blocker; THIS fixture captured every node/edge/version in a single
/// `persist_indexes()` snapshot and then shut down gracefully, so reopening under
/// trunk replays ZERO WAL entries. The question this test answers: does a cleanly
/// checkpointed 0.1.x data dir open under 0.2.0 with FULL integrity? Ground truth
/// is `tests/fixtures/compat/README.md` (the checkpointed section) — note the ids
/// differ from fixture 1: eve=9, frank=10, umbrella=11, and there is NO sentinel.
#[test]
fn checkpointed_0_1_1_datadir_opens_under_trunk_with_full_integrity() {
    // Open a COPY, never the checked-in fixture in place (opening replays /
    // re-snapshots and would mutate the committed fixture bytes).
    let copy = FixtureCopy::from_source(&checkpointed_fixture_dir());
    let db = AletheiaDB::open(&copy.path)
        .expect("trunk (0.2.0) must open a cleanly-checkpointed 0.1.1 data directory");

    // --- Current-state counts -------------------------------------------------
    // Everything was captured in ONE index snapshot; the WAL tail is drained (all
    // entries <= the snapshot watermark, so reopen replays 0). Unlike the WAL-tail
    // fixture (which yields 13/12 because trunk replays the dropped sentinel slot),
    // there is no sentinel and no tail here, so counts are exactly 12/12.
    assert_eq!(
        db.node_count(),
        12,
        "checkpointed fixture: node_count must be 12 (no WAL tail, no sentinel)"
    );
    assert_eq!(
        db.edge_count(),
        12,
        "checkpointed fixture: edge_count must be 12"
    );

    // --- No sentinel node -----------------------------------------------------
    // Fixture 1 has a throwaway Sentinel at id 9; this fixture omits it entirely,
    // so id 9 is Eve (a real Person), not a sentinel. id 12 does not exist.
    assert!(
        db.get_node(NodeId::new(12).unwrap()).is_err(),
        "checkpointed fixture: node id 12 must NOT exist (only 0..=11)"
    );

    // --- Labels are CORRECT — including the entities corrupted in fixture 1 ----
    // In the WAL-tail fixture, trunk mis-resolves eve/frank/umbrella labels to
    // interned property-key strings ("founded"/"since"). Here every label — batch-1
    // AND the ex-"batch 2" nodes — comes straight from the index snapshot's string
    // table, so labels round-trip correctly. This is the whole point of the second
    // fixture: checkpointing before upgrade is the LABEL-SAFE path.
    let mut violations: Vec<String> = Vec::new();

    // Batch-1 control group (from snapshot; correct in BOTH fixtures).
    check_node(
        &db,
        0,
        "Person",
        "name",
        "String(\"Alice\")",
        &mut violations,
    );
    check_node(
        &db,
        4,
        "Company",
        "name",
        "String(\"Acme\")",
        &mut violations,
    );
    check_node(
        &db,
        7,
        "City",
        "name",
        "String(\"London\")",
        &mut violations,
    );

    // The three ex-"batch 2" nodes: eve=9, frank=10, umbrella=11. These labels are
    // CORRUPTED in the WAL-tail fixture (eve/frank -> "founded", umbrella -> "since")
    // but must be CORRECT here because they were checkpointed, not replayed.
    check_node(&db, 9, "Person", "name", "String(\"Eve\")", &mut violations);
    check_node(
        &db,
        10,
        "Person",
        "name",
        "String(\"Frank\")",
        &mut violations,
    );
    check_node(
        &db,
        11,
        "Company",
        "name",
        "String(\"Umbrella\")",
        &mut violations,
    );

    // --- Property round-trips (String/Int/Float/Bool) -------------------------
    // alice (id 0): name/age/score/active.
    check_int(&db, 0, "age", 30, &mut violations);
    check_float(&db, 0, "score", 4.5, &mut violations);
    check_bool(&db, 0, "active", true, &mut violations);
    // acme (id 4): name/founded/public.
    check_int(&db, 4, "founded", 1999, &mut violations);
    check_bool(&db, 4, "public", true, &mut violations);
    // eve (id 9): name/age/score. The float `score` round-trip (Float(5.0)) is
    // restored here (previously covered by a since-deleted test) to keep a
    // batch-2 Float property in the checkpointed integrity net.
    check_int(&db, 9, "age", 34, &mut violations);
    check_float(&db, 9, "score", 5.0, &mut violations);

    // --- Specific edges with label + property ---------------------------------
    // (Person:Alice) -[WORKS_AT role="Engineer"]-> (Company:Acme)  [snapshot].
    let acme_id = NodeId::new(4).unwrap();
    let alice_works_at_acme = db
        .get_outgoing_edges(NodeId::new(0).unwrap())
        .into_iter()
        .filter_map(|eid| db.get_edge(eid).ok())
        .find(|e| e.has_label_str("WORKS_AT") && e.target == acme_id);
    match alice_works_at_acme {
        Some(e) if e.get_property("role").and_then(|v| v.as_str()) == Some("Engineer") => {}
        Some(e) => violations.push(format!(
            "alice->acme WORKS_AT role = {} (expected Engineer)",
            prop_str(e.get_property("role"))
        )),
        None => violations.push("alice -[WORKS_AT]-> acme edge not found by label".to_string()),
    }

    // (Person:Eve) -[WORKS_AT role="Researcher"]-> (Company:Umbrella).
    // In the WAL-tail fixture this find-by-label can fail because eve's/umbrella's
    // labels are interner-corrupted; here it must succeed with correct labels.
    let umbrella_id = NodeId::new(11).unwrap();
    let eve_works_at_umbrella = db
        .get_outgoing_edges(NodeId::new(9).unwrap())
        .into_iter()
        .filter_map(|eid| db.get_edge(eid).ok())
        .find(|e| e.has_label_str("WORKS_AT") && e.target == umbrella_id);
    match eve_works_at_umbrella {
        Some(e) if e.get_property("role").and_then(|v| v.as_str()) == Some("Researcher") => {}
        Some(e) => violations.push(format!(
            "eve->umbrella WORKS_AT role = {} (expected Researcher)",
            prop_str(e.get_property("role"))
        )),
        None => violations.push("eve -[WORKS_AT]-> umbrella edge not found by label".to_string()),
    }

    // --- Temporal: Bob's superseded history (from the temporal snapshot) ------
    // get_node_history(bob=1) must return 2 versions, age 41 then 42, correctly
    // intervalled: v1's valid interval is CLOSED and abuts v2's OPEN valid interval.
    let bob = NodeId::new(1).unwrap();
    match db.get_node_history(bob) {
        Ok(h) if h.versions.len() == 2 => {
            if prop_str(h.versions[0].properties.get("age")) != "Int(41)" {
                violations.push(format!(
                    "bob v1 age = {} (expected Int(41))",
                    prop_str(h.versions[0].properties.get("age"))
                ));
            }
            if prop_str(h.versions[1].properties.get("age")) != "Int(42)" {
                violations.push(format!(
                    "bob v2 age = {} (expected Int(42))",
                    prop_str(h.versions[1].properties.get("age"))
                ));
            }
            // Correctly intervalled: v1.valid_to == v2.valid_from (contiguous),
            // v1's valid interval is closed, v2's is open (current).
            let v1_valid = h.versions[0].temporal.valid_time();
            let v2_valid = h.versions[1].temporal.valid_time();
            if v1_valid.end().wallclock() != v2_valid.start().wallclock() {
                violations.push(format!(
                    "bob v1.valid_to ({}) != v2.valid_from ({}) — not contiguous",
                    v1_valid.end().wallclock(),
                    v2_valid.start().wallclock()
                ));
            }
            if v1_valid.is_current() {
                violations
                    .push("bob v1 valid interval is open (expected closed/superseded)".to_string());
            }
            if !v2_valid.is_current() {
                violations
                    .push("bob v2 valid interval is closed (expected open/current)".to_string());
            }
        }
        Ok(h) => violations.push(format!(
            "bob history has {} versions (expected 2)",
            h.versions.len()
        )),
        Err(e) => violations.push(format!("bob history errored: {e:?}")),
    }

    assert!(
        violations.is_empty(),
        "\ncheckpointed 0.1.1 -> trunk cross-version integrity violations ({}):\n  - {}\n",
        violations.len(),
        violations.join("\n  - ")
    );

    // --- Probe (SOFT — never fails the integrity test) ------------------------
    // Orthogonal to the checkpoint question: does trunk fix 0.1.1's restore-path
    // limitation where get_node_at_time() returns NodeNotFound for RESTORED nodes?
    // before_update_valid_micros is the authoritative pre-update coordinate from
    // the checkpointed fixture's ground truth (README): Bob was age 41 at this time.
    let before_update: Timestamp = Timestamp::from(1_784_437_154_389_396_i64);
    let now = time::now();
    match db.get_node_at_time(bob, before_update, now) {
        Ok(node) => eprintln!(
            "PROBE (checkpointed): trunk FIXED the 0.1.1 restore-path limitation — \
             get_node_at_time(bob, before_update, now) = {} (expected Int(41))",
            prop_str(node.get_property("age"))
        ),
        Err(err) => eprintln!(
            "PROBE (checkpointed): 0.1.1 restore-path limitation NOT fixed — \
             get_node_at_time(bob, before_update, now) = Err({err:?})"
        ),
    }
}

fn check_node(
    db: &AletheiaDB,
    id: u64,
    label: &str,
    name_key: &str,
    name_expected: &str,
    violations: &mut Vec<String>,
) {
    let node_id = NodeId::new(id).unwrap();
    match db.get_node(node_id) {
        Ok(n) => {
            if !n.has_label_str(label) {
                violations.push(format!(
                    "node {id} label = {} (expected {label})  <-- interner corruption",
                    n.label
                ));
            }
            let got = prop_str(n.get_property(name_key));
            if got != name_expected {
                violations.push(format!(
                    "node {id} {name_key} = {got} (expected {name_expected})"
                ));
            }
        }
        Err(e) => violations.push(format!("node {id} missing: {e:?}")),
    }
}

fn check_int(db: &AletheiaDB, id: u64, key: &str, expected: i64, violations: &mut Vec<String>) {
    if let Ok(n) = db.get_node(NodeId::new(id).unwrap()) {
        let got = prop_str(n.get_property(key));
        if got != format!("Int({expected})") {
            violations.push(format!(
                "node {id} {key} = {got} (expected Int({expected}))"
            ));
        }
    }
}

fn check_float(db: &AletheiaDB, id: u64, key: &str, expected: f64, violations: &mut Vec<String>) {
    if let Ok(n) = db.get_node(NodeId::new(id).unwrap()) {
        match n.get_property(key) {
            Some(PropertyValue::Float(v)) if (*v - expected).abs() < 1e-9 => {}
            other => violations.push(format!(
                "node {id} {key} = {other:?} (expected Float({expected}))"
            )),
        }
    }
}

fn check_bool(db: &AletheiaDB, id: u64, key: &str, expected: bool, violations: &mut Vec<String>) {
    if let Ok(n) = db.get_node(NodeId::new(id).unwrap()) {
        let got = prop_str(n.get_property(key));
        if got != format!("Bool({expected})") {
            violations.push(format!(
                "node {id} {key} = {got} (expected Bool({expected}))"
            ));
        }
    }
}

/// Regression lock for the test-harness tempdir race.
///
/// This pins the structural hole that caused the two 0.1.1 compat tests to
/// intermittently open each other's fixture directory on macOS: the tempdir
/// path used to derive ONLY from pid + a nanosecond clock, so two constructors
/// racing within one coarse-resolution tick (both tests run concurrently in the
/// same binary, hence the same pid) computed the SAME path and clobbered each
/// other's copy. `unique_temp_dir`'s process-global monotonic counter closes
/// that hole: no two calls in one process can return the same path, regardless
/// of clock resolution.
///
/// This test spawns many threads that each ask for one path at once and asserts
/// they are all distinct. It is deterministic on the fixed code (the atomic
/// guarantees uniqueness) and does NOT depend on any real fixture. A future
/// revert to clock-only naming would collide under this concurrency and fail
/// here.
#[test]
fn fixture_temp_dirs_are_unique_under_concurrency() {
    use std::collections::HashSet;
    use std::sync::{Arc, Mutex};

    const THREADS: usize = 64;
    let seen: Arc<Mutex<HashSet<PathBuf>>> = Arc::new(Mutex::new(HashSet::new()));
    let handles: Vec<_> = (0..THREADS)
        .map(|_| {
            let seen = Arc::clone(&seen);
            std::thread::spawn(move || {
                let path = unique_temp_dir("lock");
                seen.lock().expect("seen mutex poisoned").insert(path);
            })
        })
        .collect();
    for h in handles {
        h.join().expect("temp-dir generator thread panicked");
    }

    let set = seen.lock().expect("seen mutex poisoned");
    assert_eq!(
        set.len(),
        THREADS,
        "expected {THREADS} distinct tempdir paths, got {}: a collision means the \
         pid+nanos-only naming regressed and the macOS fixture-swap race is back",
        set.len()
    );
}
