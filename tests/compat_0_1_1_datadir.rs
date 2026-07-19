//! Cross-version compatibility: open a data directory written by the published
//! AletheiaDB **0.1.1** crate under the current trunk (0.2.0) code.
//!
//! # STATUS: RELEASE BLOCKER (test is `#[ignore]`d so CI stays green)
//!
//! Opening a real 0.1.1-written data directory under trunk **corrupts the
//! interned-string labels of every entity recovered from the 0.1.1 WAL tail**,
//! and diverges from 0.1.1's own recovered state. Trunk *does* open the
//! directory without erroring, and the batch-1 index-snapshot entities (nodes
//! 0..8) and Bob's temporal version chain survive intact — but the WAL-replay
//! path mis-resolves interned strings for the post-snapshot tail.
//!
//! ## Observed failure (verbatim, `AletheiaDB::open()` on a copy of the fixture)
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
//! ## Why this blocks the in-place 0.1.x -> 0.2.0 upgrade
//!
//! The documented, supported upgrade path is an **in-place open** (there is no
//! 0.1.x `.albk` backup off-ramp). Any 0.1.x deployment with committed writes
//! after its last index-persistence snapshot — i.e. any non-cleanly-snapshotted
//! shutdown — will, on first 0.2.0 open, silently mislabel exactly those
//! tail entities. Label-based reads/traversals (`find_nodes`, `MATCH (n:Label)`)
//! then return wrong results with no error surfaced. This must be fixed (or the
//! migration path re-scoped) before 0.2.0 ships; the migration guide's
//! data-safety section must NOT be upgraded to a "tested / safe in-place"
//! claim on the strength of this fixture.
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
//! This test is retained (`#[ignore]`d) as an executable reproduction of the
//! blocker: `cargo test --test compat_0_1_1_datadir -- --ignored --nocapture`.

use aletheiadb::{AletheiaDB, NodeId, PropertyValue, Timestamp, time};
use std::path::{Path, PathBuf};

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

/// A tempdir holding a fresh copy of the fixture; cleaned up on drop. We do not
/// depend on the `tempfile` crate (not a dev-dependency), so we build a unique
/// path under `std::env::temp_dir()` from pid + a nanosecond timestamp.
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
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let unique = format!("aletheiadb-compat-0_1_1-{}-{}", std::process::id(), nanos);
        let path = std::env::temp_dir().join(unique);
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

/// Cross-version integrity check. Encodes the fixture's CORRECT ground truth
/// (`tests/fixtures/compat/README.md`) — the state a faithful 0.1.x -> 0.2.0
/// open should reproduce — and collects every divergence. It currently FAILS
/// (hence `#[ignore]`): trunk mislabels the WAL-tail entities. Re-enable by
/// removing `#[ignore]` once the WAL-replay interner-ID drift is fixed.
#[test]
#[ignore = "BLOCKER: 0.1.x data dir does not open cleanly under trunk — WAL-tail interned labels are corrupted; see the //! header and PR body"]
fn opens_0_1_1_data_dir_with_full_integrity() {
    // Open a COPY (opening the checked-in fixture in place would let WAL
    // replay / re-snapshot mutate the committed fixture).
    let copy = FixtureCopy::new();
    let db = AletheiaDB::open(&copy.path)
        .expect("trunk (0.2.0) must at least open a data directory written by 0.1.1");

    // Collect all integrity violations rather than stopping at the first, so a
    // single `--ignored` run documents the full blast radius.
    let mut violations: Vec<String> = Vec::new();

    // --- Current-state counts -------------------------------------------------
    // Fixture ground truth is 12 / 12. Trunk yields 13 / 12: it replays one more
    // WAL entry than 0.1.1 (the throwaway "Sentinel" boundary slot 0.1.1's
    // snapshot-boundary off-by-one dropped), so the recovered set diverges from
    // the data 0.1.1 itself recovers.
    if db.node_count() != 12 {
        violations.push(format!(
            "node_count = {} (expected 12; trunk replays the sentinel boundary slot 0.1.1 dropped)",
            db.node_count()
        ));
    }
    if db.edge_count() != 12 {
        violations.push(format!("edge_count = {} (expected 12)", db.edge_count()));
    }

    // --- Quirk (1): sentinel node id 9 should be absent -----------------------
    // 0.1.1 dropped node 9 via its snapshot-boundary off-by-one. Trunk instead
    // replays it — but cannot resolve its label (shows as `Interned(N)`).
    let sentinel = NodeId::new(9).unwrap();
    if let Ok(n) = db.get_node(sentinel) {
        violations.push(format!(
            "sentinel node id 9 is PRESENT under trunk (expected absent); label renders as {}",
            n.label
        ));
    }

    // --- Batch-1 nodes (index snapshot): expected fully intact ----------------
    // These come from the 0.1.1 index snapshot's string table and are the
    // control group — they must round-trip correctly.
    check_node(
        &db,
        0,
        "Person",
        "name",
        "String(\"Alice\")",
        &mut violations,
    );
    check_int(&db, 0, "age", 30, &mut violations);
    check_float(&db, 0, "score", 4.5, &mut violations);
    check_bool(&db, 0, "active", true, &mut violations);
    check_node(
        &db,
        4,
        "Company",
        "name",
        "String(\"Acme\")",
        &mut violations,
    );
    check_int(&db, 4, "founded", 1999, &mut violations);
    check_bool(&db, 4, "public", true, &mut violations);

    // --- Batch-2 nodes (WAL tail): where the corruption lives -----------------
    // Names survive; labels are mis-resolved to unrelated interned strings.
    check_node(
        &db,
        10,
        "Person",
        "name",
        "String(\"Eve\")",
        &mut violations,
    );
    check_int(&db, 10, "age", 34, &mut violations);
    check_float(&db, 10, "score", 5.0, &mut violations);
    check_node(
        &db,
        12,
        "Company",
        "name",
        "String(\"Umbrella\")",
        &mut violations,
    );

    // --- Specific edges with properties --------------------------------------
    // (Person:Alice) -[WORKS_AT role="Engineer"]-> (Company:Acme)  [batch-1]
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
        None => violations
            .push("alice -[WORKS_AT]-> acme edge not found by label (batch-1)".to_string()),
    }

    // (Person:Eve) -[WORKS_AT role="Researcher"]-> (Company:Umbrella) [batch-2].
    // If batch-2 EDGE labels are also interner-corrupted, this find-by-label
    // fails too — additional evidence of the same root cause.
    let umbrella_id = NodeId::new(12).unwrap();
    let eve_works_at_umbrella = db
        .get_outgoing_edges(NodeId::new(10).unwrap())
        .into_iter()
        .filter_map(|eid| db.get_edge(eid).ok())
        .find(|e| e.has_label_str("WORKS_AT") && e.target == umbrella_id);
    match eve_works_at_umbrella {
        Some(e) if e.get_property("role").and_then(|v| v.as_str()) == Some("Researcher") => {}
        Some(e) => violations.push(format!(
            "eve->umbrella WORKS_AT role = {} (expected Researcher)",
            prop_str(e.get_property("role"))
        )),
        None => violations.push(
            "eve -[WORKS_AT]-> umbrella edge not found by label (batch-2 WAL tail)".to_string(),
        ),
    }

    // --- Temporal: Bob's superseded history (from the snapshot) ---------------
    // This path (temporal index snapshot) survives — asserted as a control.
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
        }
        Ok(h) => violations.push(format!(
            "bob history has {} versions (expected 2)",
            h.versions.len()
        )),
        Err(e) => violations.push(format!("bob history errored: {e:?}")),
    }

    // --- Probe (non-fatal): get_node_at_time on a restored node ---------------
    // 0.1.1 returned NodeNotFound here for restored nodes. This records whether
    // trunk fixed it. On the observed run trunk did NOT (same NodeNotFound), so
    // this is reported, not counted as a blocker violation.
    let before_update: Timestamp = Timestamp::from(1_784_435_154_774_824_i64);
    let now = time::now();
    match db.get_node_at_time(bob, before_update, now) {
        Ok(node) => eprintln!(
            "PROBE: trunk FIXED the 0.1.1 restore-path limitation — \
             get_node_at_time(bob, before_update, now) = {} (expected Int(41))",
            prop_str(node.get_property("age"))
        ),
        Err(err) => eprintln!(
            "PROBE: 0.1.1 restore-path limitation NOT fixed — \
             get_node_at_time(bob, before_update, now) = Err({err:?})"
        ),
    }

    assert!(
        violations.is_empty(),
        "\n0.1.1 -> trunk cross-version integrity violations ({}):\n  - {}\n",
        violations.len(),
        violations.join("\n  - ")
    );
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
    // eve (id 9): name/age.
    check_int(&db, 9, "age", 34, &mut violations);

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
