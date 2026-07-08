//! WAL recovery integration test (Issue #365)
//!
//! Full-database crash + reopen test: write through the public `AletheiaDB`
//! API with a durable WAL, drop the database WITHOUT a checkpoint (simulated
//! crash), reopen with the same configuration (full WAL replay from LSN 0),
//! and verify that:
//!
//! - current-state reads are identical (labels, properties, counts, deleted
//!   entities absent),
//! - every entity's bi-temporal history matches what was captured pre-crash
//!   (version counts, valid-time bounds, open/closed transaction intervals),
//! - bi-temporal point-in-time reads behave correctly (superseded states
//!   recallable by anchoring transaction time; deleted/retracted entities
//!   visible before and absent at/after their deletion/retraction),
//! - no data corruption (node/edge counts match).
//!
//! Covers all operation types: create (plain + backdated valid_from,
//! Issue #3221), update, delete (node + edge), and valid-time retraction
//! (node detach + edge, Issue #3230).
//!
//! ## What is deliberately NOT asserted
//!
//! Exact transaction-time equality across a crash is impossible by design:
//! the live path stamps historical versions with the HLC *commit* timestamp,
//! while replay stamps them with the WAL entry's *logged* timestamp (assigned
//! at append time, a few microseconds earlier). Both are correct
//! representations of "when the fact was recorded"; the tests therefore
//! assert transaction-time *structure* (open/closed flags, chain continuity)
//! rather than exact bounds. Valid-time bounds ARE asserted exactly — they
//! are logged in the WAL and must be honored faithfully (the one known gap,
//! delete tombstone valid_from, is pinned by the ignored test at the bottom
//! of this file).

use aletheiadb::config::WalConfigBuilder;
use aletheiadb::core::history::EntityHistory;
use aletheiadb::core::hlc::HybridTimestamp;
use aletheiadb::core::temporal::{Timestamp, time};
use aletheiadb::storage::index_persistence::PersistenceConfig;
use aletheiadb::storage::wal::DurabilityMode;
use aletheiadb::{AletheiaDB, AletheiaDBConfig, PropertyMap, PropertyMapBuilder, PropertyValue};
use std::path::Path;

/// A timestamp `hours` hours before `now_micros` (microseconds since epoch).
fn hours_ago(now_micros: i64, hours: i64) -> Timestamp {
    HybridTimestamp::new(now_micros - hours * 3_600_000_000, 0)
        .expect("backdated timestamp must be valid")
}

/// Durable config: synchronous WAL in `wal_dir`, index persistence DISABLED.
///
/// Persistence must be explicitly disabled: the default `PersistenceConfig`
/// is enabled with a cwd-relative "data" dir, so a stray snapshot would be
/// loaded on "restart" and skip the WAL replay under test.
fn crash_recovery_config(wal_dir: &Path) -> AletheiaDBConfig {
    AletheiaDBConfig::builder()
        .wal(
            WalConfigBuilder::new()
                .wal_dir(wal_dir.to_path_buf())
                .durability_mode(DurabilityMode::Synchronous)
                .build(),
        )
        .persistence(PersistenceConfig {
            enabled: false,
            ..Default::default()
        })
        .build()
}

/// Pre-crash snapshot of one historical version's observable state.
#[derive(Debug, Clone, PartialEq)]
struct VersionSnapshot {
    label: String,
    properties: PropertyMap,
    valid_start: Timestamp,
    valid_end: Timestamp,
    valid_empty: bool,
    valid_open: bool,
    tx_open: bool,
}

fn snapshot_history(history: &EntityHistory) -> Vec<VersionSnapshot> {
    history
        .versions
        .iter()
        .map(|v| VersionSnapshot {
            label: v.label.clone(),
            properties: v.properties.clone(),
            valid_start: v.temporal.valid_time().start(),
            valid_end: v.temporal.valid_time().end(),
            valid_empty: v.temporal.valid_time().is_empty(),
            valid_open: v.temporal.valid_time().is_current(),
            tx_open: v.temporal.transaction_time().is_current(),
        })
        .collect()
}

/// Assert a recovered history matches its pre-crash snapshot.
///
/// Valid-time bounds are compared exactly, with a single carve-out around
/// delete tombstones: a tombstone's (empty) valid interval — and therefore
/// the valid_to of the version it closes, which is stamped from the
/// tombstone's valid_from — is compared structurally rather than exactly.
/// Replay currently stamps delete tombstones with the entry timestamp
/// instead of the logged valid_from (see
/// `backdated_delete_valid_from_survives_replay` below), and even the
/// non-backdated tombstone valid_from differs by microseconds (transaction
/// start time vs WAL entry timestamp).
///
/// Transaction-time bounds are compared structurally (open/closed + chain
/// continuity), not exactly — see the module docs for why exact equality
/// cannot hold.
fn assert_history_matches(entity: &str, pre: &[VersionSnapshot], recovered: &EntityHistory) {
    let post = snapshot_history(recovered);
    assert_eq!(
        pre.len(),
        post.len(),
        "{entity}: version count must survive recovery"
    );
    for (i, (p, q)) in pre.iter().zip(post.iter()).enumerate() {
        assert_eq!(
            p.label, q.label,
            "{entity} v{i}: label must survive recovery"
        );
        assert_eq!(
            p.properties, q.properties,
            "{entity} v{i}: properties must survive recovery"
        );
        assert_eq!(
            p.valid_empty, q.valid_empty,
            "{entity} v{i}: tombstone-ness of the valid interval must survive recovery"
        );
        assert_eq!(
            p.valid_open, q.valid_open,
            "{entity} v{i}: valid-time openness must survive recovery"
        );
        let next_is_tombstone = pre.get(i + 1).is_some_and(|n| n.valid_empty);
        if !p.valid_empty {
            assert_eq!(
                p.valid_start, q.valid_start,
                "{entity} v{i}: valid_from must survive recovery exactly"
            );
            if !next_is_tombstone {
                assert_eq!(
                    p.valid_end, q.valid_end,
                    "{entity} v{i}: valid_to must survive recovery exactly"
                );
            }
            // else: this version was closed by a delete tombstone; its exact
            // valid_to inherits the tombstone valid_from divergence (openness
            // is asserted above, continuity is asserted below).
        }
        assert_eq!(
            p.tx_open, q.tx_open,
            "{entity} v{i}: transaction-time openness must survive recovery"
        );
    }

    // Chain continuity on the recovered history:
    // - each superseded version's tx-time closure must equal its successor's
    //   transaction start (both stamped from the same WAL entry);
    // - a version closed by a tombstone must have its valid_to equal to the
    //   tombstone's valid_from.
    for i in 0..recovered.versions.len().saturating_sub(1) {
        let closed_at = recovered.versions[i].temporal.transaction_time().end();
        let successor = &recovered.versions[i + 1];
        assert_eq!(
            closed_at,
            successor.temporal.transaction_time().start(),
            "{entity} v{i}: tx-time closure must equal the successor's tx start after recovery"
        );
        if successor.temporal.valid_time().is_empty() {
            assert_eq!(
                recovered.versions[i].temporal.valid_time().end(),
                successor.temporal.valid_time().start(),
                "{entity} v{i}: valid_to must equal the closing tombstone's valid_from"
            );
        }
    }
}

/// Issue #365: data written through the public API with a durable WAL must
/// survive a crash (drop without checkpoint) and reopen, with full temporal
/// history intact. Covers create/update/delete/retract for nodes and edges.
#[test]
fn test_wal_recovery_on_reopen() {
    let temp_dir = tempfile::tempdir().unwrap();
    let wal_dir = temp_dir.path().join("wal");

    let now = time::now().wallclock();
    let t3 = hours_ago(now, 3);
    let t2 = hours_ago(now, 2);
    let t1 = hours_ago(now, 1);

    // ---------- Phase 1: pre-crash writes + state capture ----------
    let (alice, bob, carol, dave, erin, e_knows, e_likes, e_mentors, e_respects);
    let (pre_node_count, pre_edge_count);
    let (alice_hist, bob_hist, carol_hist, dave_hist, erin_hist);
    let (e_knows_hist, e_likes_hist, e_mentors_hist, e_respects_hist);
    {
        let db = AletheiaDB::with_unified_config(crash_recovery_config(&wal_dir))
            .expect("initial open with a fresh WAL dir must succeed");

        // Creates, both backdated (Issue #3221). Bob is backdated to t3 —
        // the earliest valid_from of any edge touching him (e_mentors at t3;
        // e_knows / e_respects at t2) — so both endpoints of every edge are
        // valid whenever the edge itself is valid.
        alice = db
            .create_node_with_valid_time(
                "Person",
                PropertyMapBuilder::new().insert("name", "Alice").build(),
                Some(t3),
            )
            .unwrap();
        bob = db
            .create_node_with_valid_time(
                "Person",
                PropertyMapBuilder::new().insert("name", "Bob").build(),
                Some(t3),
            )
            .unwrap();

        // Backdated edge.
        e_knows = db
            .create_edge_with_valid_time(
                alice,
                bob,
                "KNOWS",
                PropertyMapBuilder::new().insert("since", 2020_i64).build(),
                Some(t2),
            )
            .unwrap();

        // Update alice (captures a superseded version).
        db.update_node_with_valid_time(
            alice,
            PropertyMapBuilder::new()
                .insert("name", "Alice")
                .insert("age", 31_i64)
                .build(),
            Some(t2),
        )
        .unwrap();

        // Delete an isolated node (plain delete).
        carol = db
            .create_node(
                "Person",
                PropertyMapBuilder::new().insert("name", "Carol").build(),
            )
            .unwrap();
        db.delete_node_with_valid_time(carol, None).unwrap();

        // Delete an edge (plain delete).
        e_likes = db
            .create_edge(bob, alice, "LIKES", PropertyMapBuilder::new().build())
            .unwrap();
        db.delete_edge_with_valid_time(e_likes, None).unwrap();

        // Retract a node with a connected edge (detach co-retracts the edge)
        // at a BACKDATED valid_to (Issue #3230).
        dave = db
            .create_node_with_valid_time(
                "Person",
                PropertyMapBuilder::new().insert("name", "Dave").build(),
                Some(t3),
            )
            .unwrap();
        e_mentors = db
            .create_edge_with_valid_time(
                dave,
                bob,
                "MENTORS",
                PropertyMapBuilder::new().build(),
                Some(t3),
            )
            .unwrap();
        let retraction = db.retract_node_detach(dave, t1).unwrap();
        assert_eq!(retraction.edges_retracted, 1);

        // Retract an edge directly at a backdated valid_to.
        e_respects = db
            .create_edge_with_valid_time(
                bob,
                alice,
                "RESPECTS",
                PropertyMapBuilder::new().build(),
                Some(t2),
            )
            .unwrap();
        let edge_retraction = db.retract_edge(e_respects, t1).unwrap();
        assert!(!edge_retraction.already_retracted);

        // A full version chain closed by a tombstone: create -> update ->
        // delete on the same (isolated, edge-free) node. This is the only
        // entity whose delete tombstone closes an already-superseded chain.
        erin = db
            .create_node_with_valid_time(
                "Person",
                PropertyMapBuilder::new().insert("name", "Erin").build(),
                Some(t3),
            )
            .unwrap();
        db.update_node_with_valid_time(
            erin,
            PropertyMapBuilder::new()
                .insert("name", "Erin")
                .insert("role", "admin")
                .build(),
            Some(t2),
        )
        .unwrap();
        db.delete_node_with_valid_time(erin, None).unwrap();

        // Capture current state.
        pre_node_count = db.node_count();
        pre_edge_count = db.edge_count();
        assert_eq!(
            pre_node_count, 2,
            "alice + bob live; carol + erin deleted, dave retracted"
        );
        assert_eq!(pre_edge_count, 1, "only KNOWS live");

        // Capture full bi-temporal histories.
        alice_hist = snapshot_history(&db.get_node_history(alice).unwrap());
        bob_hist = snapshot_history(&db.get_node_history(bob).unwrap());
        carol_hist = snapshot_history(&db.get_node_history(carol).unwrap());
        dave_hist = snapshot_history(&db.get_node_history(dave).unwrap());
        erin_hist = snapshot_history(&db.get_node_history(erin).unwrap());
        e_knows_hist = snapshot_history(&db.get_edge_history(e_knows).unwrap());
        e_likes_hist = snapshot_history(&db.get_edge_history(e_likes).unwrap());
        e_mentors_hist = snapshot_history(&db.get_edge_history(e_mentors).unwrap());
        e_respects_hist = snapshot_history(&db.get_edge_history(e_respects).unwrap());

        assert_eq!(alice_hist.len(), 2, "create + update");
        assert_eq!(bob_hist.len(), 1);
        assert_eq!(carol_hist.len(), 2, "create + delete tombstone");
        assert_eq!(dave_hist.len(), 2, "create + retraction");
        assert_eq!(erin_hist.len(), 3, "create + update + delete tombstone");
        assert_eq!(e_knows_hist.len(), 1);
        assert_eq!(e_likes_hist.len(), 2, "create + delete tombstone");
        assert_eq!(e_mentors_hist.len(), 2, "create + co-retraction");
        assert_eq!(e_respects_hist.len(), 2, "create + retraction");

        // ---------- Phase 2: simulated crash ----------
        // Drop WITHOUT a checkpoint: recovery must come from WAL replay alone.
    }

    // ---------- Phase 3: reopen (full WAL replay from LSN 0) ----------
    let db = AletheiaDB::with_unified_config(crash_recovery_config(&wal_dir))
        .expect("reopen after simulated crash must succeed (WAL replay)");

    // (d) No data corruption: counts identical.
    assert_eq!(
        db.node_count(),
        pre_node_count,
        "node count must survive recovery"
    );
    assert_eq!(
        db.edge_count(),
        pre_edge_count,
        "edge count must survive recovery"
    );

    // (a) Current-state reads identical.
    let alice_now = db.get_node(alice).unwrap();
    assert!(alice_now.has_label_str("Person"));
    assert!(
        matches!(alice_now.properties.get("name"), Some(PropertyValue::String(s)) if s.as_ref() == "Alice")
    );
    assert!(
        matches!(
            alice_now.properties.get("age"),
            Some(PropertyValue::Int(31))
        ),
        "alice must be recovered in her UPDATED state"
    );
    let bob_now = db.get_node(bob).unwrap();
    assert!(
        matches!(bob_now.properties.get("name"), Some(PropertyValue::String(s)) if s.as_ref() == "Bob")
    );
    let knows_now = db.get_edge(e_knows).unwrap();
    assert!(knows_now.has_label_str("KNOWS"));
    assert_eq!(knows_now.source, alice);
    assert_eq!(knows_now.target, bob);

    // Deleted / retracted entities absent from current state.
    assert!(
        db.get_node(carol).is_err(),
        "deleted node must stay deleted"
    );
    assert!(
        db.get_node(dave).is_err(),
        "retracted node must stay absent"
    );
    assert!(
        db.get_node(erin).is_err(),
        "deleted node (with an update chain) must stay deleted"
    );
    assert!(
        db.get_edge(e_likes).is_err(),
        "deleted edge must stay deleted"
    );
    assert!(
        db.get_edge(e_mentors).is_err(),
        "co-retracted edge must stay absent"
    );
    assert!(
        db.get_edge(e_respects).is_err(),
        "retracted edge must stay absent"
    );

    // (b) Every captured history matches.
    let alice_recovered = db.get_node_history(alice).unwrap();
    assert_history_matches("alice", &alice_hist, &alice_recovered);
    let bob_recovered = db.get_node_history(bob).unwrap();
    assert_history_matches("bob", &bob_hist, &bob_recovered);
    let carol_recovered = db.get_node_history(carol).unwrap();
    assert_history_matches("carol", &carol_hist, &carol_recovered);
    let dave_recovered = db.get_node_history(dave).unwrap();
    assert_history_matches("dave", &dave_hist, &dave_recovered);
    let erin_recovered = db.get_node_history(erin).unwrap();
    assert_history_matches("erin", &erin_hist, &erin_recovered);
    assert_history_matches(
        "e_knows",
        &e_knows_hist,
        &db.get_edge_history(e_knows).unwrap(),
    );
    let e_likes_recovered = db.get_edge_history(e_likes).unwrap();
    assert_history_matches("e_likes", &e_likes_hist, &e_likes_recovered);
    assert_history_matches(
        "e_mentors",
        &e_mentors_hist,
        &db.get_edge_history(e_mentors).unwrap(),
    );
    assert_history_matches(
        "e_respects",
        &e_respects_hist,
        &db.get_edge_history(e_respects).unwrap(),
    );

    // Spot-check the exact recovered valid-time bounds against the
    // backdates used at write time (not just against the pre-crash capture).
    assert_eq!(
        alice_recovered.versions[0].temporal.valid_time().start(),
        t3
    );
    assert_eq!(
        alice_recovered.versions[1].temporal.valid_time().start(),
        t2
    );
    assert_eq!(
        bob_recovered.versions[0].temporal.valid_time().start(),
        t3,
        "bob's backdated valid_from must survive replay exactly"
    );
    assert_eq!(dave_recovered.versions[1].temporal.valid_time().start(), t3);
    assert_eq!(
        dave_recovered.versions[1].temporal.valid_time().end(),
        t1,
        "retraction's backdated valid_to must survive replay exactly"
    );

    // (c) Bi-temporal point-in-time reads.
    //
    // Superseded state: anchor transaction time inside the create version's
    // recovered transaction interval to recall alice BEFORE the update.
    let alice_create_tx = alice_recovered.versions[0]
        .temporal
        .transaction_time()
        .start();
    assert!(
        alice_create_tx
            < alice_recovered.versions[1]
                .temporal
                .transaction_time()
                .start(),
        "premise: create/update WAL entry timestamps must be strictly ordered (clock anomaly?)"
    );
    let alice_old = db.get_node_at_time(alice, t3, alice_create_tx).unwrap();
    assert!(
        alice_old.properties.get("age").is_none(),
        "anchored before the update, alice must not yet have an age"
    );
    assert!(
        matches!(alice_old.properties.get("name"), Some(PropertyValue::String(s)) if s.as_ref() == "Alice")
    );

    // Deleted node: visible when anchoring BOTH dimensions before the
    // delete, gone at the current coordinate.
    let carol_create_tx = carol_recovered.versions[0]
        .temporal
        .transaction_time()
        .start();
    assert!(
        carol_create_tx
            < carol_recovered.versions[1]
                .temporal
                .transaction_time()
                .start(),
        "premise: create/delete WAL entry timestamps must be strictly ordered (clock anomaly?)"
    );
    let carol_valid_from = carol_recovered.versions[0].temporal.valid_time().start();
    assert!(
        db.get_node_at_time(carol, carol_valid_from, carol_create_tx)
            .is_ok(),
        "deleted node must remain recallable at pre-delete coordinates"
    );
    assert!(
        db.get_node_at_time(carol, time::now(), time::now())
            .is_err()
    );

    // Deleted edge: same contract.
    let e_likes_create_tx = e_likes_recovered.versions[0]
        .temporal
        .transaction_time()
        .start();
    assert!(
        e_likes_create_tx
            < e_likes_recovered.versions[1]
                .temporal
                .transaction_time()
                .start(),
        "premise: create/delete WAL entry timestamps must be strictly ordered (clock anomaly?)"
    );
    let e_likes_valid_from = e_likes_recovered.versions[0].temporal.valid_time().start();
    assert!(
        db.get_edge_at_time(e_likes, e_likes_valid_from, e_likes_create_tx)
            .is_ok()
    );
    assert!(
        db.get_edge_at_time(e_likes, time::now(), time::now())
            .is_err()
    );

    // Tombstone closing a superseded chain (erin: create -> update ->
    // delete). Point-in-time visibility matrix over the recovered chain:
    // the ORIGINAL state anchored at the creation coordinates, the UPDATED
    // state anchored at the update coordinates, gone at/after the delete.
    let erin_create_tx = erin_recovered.versions[0]
        .temporal
        .transaction_time()
        .start();
    let erin_update_tx = erin_recovered.versions[1]
        .temporal
        .transaction_time()
        .start();
    let erin_delete_tx = erin_recovered.versions[2]
        .temporal
        .transaction_time()
        .start();
    assert!(
        erin_create_tx < erin_update_tx && erin_update_tx < erin_delete_tx,
        "premise: create/update/delete WAL entry timestamps must be strictly ordered (clock anomaly?)"
    );
    let erin_original = db.get_node_at_time(erin, t3, erin_create_tx).unwrap();
    assert!(
        matches!(erin_original.properties.get("name"), Some(PropertyValue::String(s)) if s.as_ref() == "Erin")
    );
    assert!(
        erin_original.properties.get("role").is_none(),
        "anchored at the creation coordinates, erin must be in her ORIGINAL state"
    );
    let erin_updated = db.get_node_at_time(erin, t2, erin_update_tx).unwrap();
    assert!(
        matches!(erin_updated.properties.get("role"), Some(PropertyValue::String(s)) if s.as_ref() == "admin"),
        "anchored at the update coordinates, erin must be in her UPDATED state"
    );
    assert!(
        db.get_node_at_time(erin, t2, erin_delete_tx).is_err(),
        "erin must be gone when transaction time is anchored at the delete's commit"
    );
    assert!(
        db.get_node_at_time(erin, time::now(), time::now()).is_err(),
        "erin must be gone at the current bi-temporal coordinate"
    );

    // Retracted entities: valid-time semantics honored after replay —
    // visible strictly before the backdated valid_to, absent at/after.
    assert!(db.get_node_at_valid_time(dave, t2).is_ok());
    assert!(db.get_node_at_valid_time(dave, t1).is_err());
    assert!(db.get_node_at_valid_time(dave, time::now()).is_err());
    assert!(db.get_edge_at_valid_time(e_mentors, t2).is_ok());
    assert!(db.get_edge_at_valid_time(e_mentors, t1).is_err());
    assert!(db.get_edge_at_valid_time(e_respects, t2).is_ok());
    assert!(db.get_edge_at_valid_time(e_respects, t1).is_err());
}

/// Pins the known live-path/replay divergence for BACKDATED deletes
/// (Issues #3221 + #452 follow-up):
///
/// - the live write path logs the user-supplied `valid_from` for
///   `DeleteNode`/`DeleteEdge` (src/api/transaction/write_buffer.rs) and
///   stamps the tombstone with it (src/api/transaction/write/apply.rs,
///   `apply_node_delete`);
/// - WAL replay destructures the logged field as `valid_from: _` and
///   substitutes the entry timestamp (src/storage/recovery.rs, DeleteNode /
///   DeleteEdge arms).
///
/// So a backdated delete's tombstone `valid_from` (when the fact stopped
/// being true in reality) is silently replaced by the delete's commit time
/// after crash recovery. This test FAILS until replay honors the logged
/// value; run it with `--ignored` to observe the divergence.
#[test]
#[ignore = "known divergence (see issue #3400): WAL replay drops the logged valid_from for \
            DeleteNode/DeleteEdge (src/storage/recovery.rs destructures `valid_from: _` and \
            stamps the tombstone with entry.timestamp), while the live path honors the #3221 \
            backdated valid_time. Un-ignore once issue #3400 is fixed and replay honors the \
            logged delete valid_from."]
fn backdated_delete_valid_from_survives_replay() {
    let temp_dir = tempfile::tempdir().unwrap();
    let wal_dir = temp_dir.path().join("wal");

    let now = time::now().wallclock();
    let t_create = hours_ago(now, 3);
    let t_delete = hours_ago(now, 1); // backdated: fact stopped being true 1h ago

    let node_id;
    let pre_tombstone_valid_from;
    {
        let db = AletheiaDB::with_unified_config(crash_recovery_config(&wal_dir)).unwrap();
        node_id = db
            .create_node_with_valid_time(
                "Person",
                PropertyMapBuilder::new().insert("name", "Zoe").build(),
                Some(t_create),
            )
            .unwrap();
        db.delete_node_with_valid_time(node_id, Some(t_delete))
            .unwrap();

        let history = db.get_node_history(node_id).unwrap();
        assert_eq!(history.version_count(), 2);
        pre_tombstone_valid_from = history.versions[1].temporal.valid_time().start();
        assert_eq!(
            pre_tombstone_valid_from, t_delete,
            "live path stamps the tombstone with the backdated valid_from"
        );
        // Simulated crash: drop without a checkpoint.
    }

    let db = AletheiaDB::with_unified_config(crash_recovery_config(&wal_dir)).unwrap();
    let history = db.get_node_history(node_id).unwrap();
    assert_eq!(history.version_count(), 2);
    assert_eq!(
        history.versions[1].temporal.valid_time().start(),
        t_delete,
        "replay must honor the LOGGED backdated delete valid_from, not the entry timestamp"
    );
}
