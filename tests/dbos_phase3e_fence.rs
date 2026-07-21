//! DBOS Phase 3e — safe multi-executor fencing primitive tests.
//!
//! Covers the three pieces of the fencing primitive extension
//! (docs/plans/2026-07-20-dbos-phase3e-cas-lease-fence.md):
//!
//! 1. server-side monotonic fence precondition on the claim,
//! 2. DB-side lease-deadline computation, and
//! 3. compare-and-set inside `apply_batch`.

use aletheiadb::api::transaction::WriteOps;
use aletheiadb::core::error::{Error, TransactionError};
use aletheiadb::core::hlc::HybridTimestamp;
use aletheiadb::core::property::PropertyValue;
use aletheiadb::core::temporal::{Timestamp, time};
use aletheiadb::{AletheiaDB, PropertyMapBuilder};

/// A `lease_until` timestamp `secs` seconds in the future of now.
fn future_ts(secs: i64) -> Timestamp {
    HybridTimestamp::new(time::now().wallclock() + secs * 1_000_000, 0).expect("ts")
}

/// A `lease_until` timestamp `secs` seconds in the past of now.
fn past_ts(secs: i64) -> Timestamp {
    HybridTimestamp::new(time::now().wallclock() - secs * 1_000_000, 0).expect("ts")
}

// -------------------------------------------------------------------------
// Piece 1 (RED→GREEN): the fenced claim makes the stale-fence steal
// collision IMPOSSIBLE.
//
// Scenario (design §5.3 / risk #16): two stealers B and C both read
// `old_fence = 7` from an expired-lease run and both compute `new_fence = 8`.
// B steals first (still-expired lease), then C steals on the lease-expired
// OR-branch with its STALE base version. On the plain primitive BOTH commit
// `fence = 8` (collision). The fenced primitive must REJECT C's steal because
// its `new_fence (8)` is not strictly greater than the now-stored `8`.
// -------------------------------------------------------------------------
#[test]
fn fenced_claim_prevents_stale_fence_collision() {
    let db = AletheiaDB::new().unwrap();
    // A run held by A with fence = 7 and an ALREADY-EXPIRED lease.
    let id = db
        .create_node(
            "WorkflowRun",
            PropertyMapBuilder::new()
                .insert("wf", "W1")
                .insert("fence", 7_i64)
                .insert("lease_owner", "A")
                .insert("lease_until", past_ts(60).wallclock())
                .build(),
        )
        .unwrap();
    let v1 = db.get_node(id).unwrap().current_version;

    // B steals: reads old_fence = 7 -> new_fence = 8, still-expired lease.
    // Version v1 matches, so this is admitted; committed fence becomes 8.
    db.claim_with_lease_fenced(
        id,
        v1,
        "lease_owner",
        "lease_until",
        "fence",
        PropertyValue::from("B"),
        std::time::Duration::from_secs(0), // lease expires immediately (still-expired)
        8,
        PropertyMapBuilder::new().insert("wf", "W1").build(),
    )
    .expect("B's fenced steal (8 > 7) must succeed");

    // C steals with the STALE v1 and the SAME stale-read fence 8. The lease is
    // expired so the OR-branch would admit it on the plain primitive; the fence
    // precondition (8 > stored 8 == false) must REJECT it.
    let c = db.claim_with_lease_fenced(
        id,
        v1,
        "lease_owner",
        "lease_until",
        "fence",
        PropertyValue::from("C"),
        std::time::Duration::from_secs(60),
        8,
        PropertyMapBuilder::new().insert("wf", "W1").build(),
    );
    assert!(
        c.is_err(),
        "second stale-fence steal MUST be rejected to prevent fence reuse; got {c:?}"
    );

    // The run is still owned by B at fence 8 — C never took it.
    let node = db.get_node(id).unwrap();
    assert_eq!(
        node.get_property("lease_owner")
            .and_then(|v| v.as_str().map(String::from)),
        Some("B".to_string()),
        "the fenced-out C must not have stolen the run"
    );
    assert_eq!(
        node.get_property("fence").and_then(|v| v.as_int()),
        Some(8),
        "fence stays at B's 8; no second claim reused it"
    );
    // The rejection is the FenceTooLow precondition failure, not a lost claim.
    match c.unwrap_err() {
        Error::Transaction(TransactionError::FenceTooLow {
            new_fence, stored, ..
        }) => {
            assert_eq!(new_fence, 8);
            assert_eq!(stored, 8);
        }
        other => panic!("expected FenceTooLow, got {other:?}"),
    }
}

// -------------------------------------------------------------------------
// Documenting test: the PLAIN primitive still admits the collision (proves the
// hole survives and that Phase 3e did NOT change plain `claim_with_lease`
// semantics). Mirrors design §8 risk #16.
// -------------------------------------------------------------------------
#[test]
fn plain_claim_still_admits_stale_fence_collision() {
    let db = AletheiaDB::new().unwrap();
    let id = db
        .create_node(
            "WorkflowRun",
            PropertyMapBuilder::new()
                .insert("fence", 7_i64)
                .insert("lease_owner", "A")
                .insert("lease_until", past_ts(60).wallclock())
                .build(),
        )
        .unwrap();
    let v1 = db.get_node(id).unwrap().current_version;

    // B steals via the plain primitive, still-expired lease, fence 8.
    db.claim_with_lease(
        id,
        v1,
        "lease_owner",
        "lease_until",
        PropertyValue::from("B"),
        past_ts(1),
        PropertyMapBuilder::new().insert("fence", 8_i64).build(),
    )
    .unwrap();

    // C steals with stale v1 on the lease-expired branch, reusing fence 8 —
    // the plain primitive has no fence check, so this SUCCEEDS (the collision).
    let c = db.claim_with_lease(
        id,
        v1,
        "lease_owner",
        "lease_until",
        PropertyValue::from("C"),
        future_ts(60),
        PropertyMapBuilder::new().insert("fence", 8_i64).build(),
    );
    assert!(
        c.is_ok(),
        "plain claim_with_lease still admits the collision (unchanged semantics): {c:?}"
    );
}

// -------------------------------------------------------------------------
// Piece 1: first fenced claim on a never-claimed run (no stored fence) succeeds.
// -------------------------------------------------------------------------
#[test]
fn fenced_claim_first_claim_absent_fence_succeeds() {
    let db = AletheiaDB::new().unwrap();
    let id = db
        .create_node(
            "WorkflowRun",
            PropertyMapBuilder::new().insert("wf", "W1").build(),
        )
        .unwrap();
    let v1 = db.get_node(id).unwrap().current_version;

    // No `fence` property exists yet -> stored fence is treated as i64::MIN, so
    // any non-negative new_fence beats it.
    db.claim_with_lease_fenced(
        id,
        v1,
        "lease_owner",
        "lease_until",
        "fence",
        PropertyValue::from("A"),
        std::time::Duration::from_secs(60),
        1,
        PropertyMapBuilder::new().insert("wf", "W1").build(),
    )
    .expect("first fenced claim on an unfenced run must succeed");

    let node = db.get_node(id).unwrap();
    assert_eq!(node.get_property("fence").and_then(|v| v.as_int()), Some(1));
    assert_eq!(
        node.get_property("lease_owner")
            .and_then(|v| v.as_str().map(String::from)),
        Some("A".to_string())
    );
}

// -------------------------------------------------------------------------
// Piece 1: the fence gate is AND-composed with the claim gate — a held,
// unexpired lease + stale version is still refused (CasMismatch), even with a
// strictly-higher fence.
// -------------------------------------------------------------------------
#[test]
fn fenced_claim_still_refused_when_lease_held() {
    let db = AletheiaDB::new().unwrap();
    let id = db
        .create_node(
            "WorkflowRun",
            PropertyMapBuilder::new().insert("fence", 5_i64).build(),
        )
        .unwrap();
    let v1 = db.get_node(id).unwrap().current_version;

    // A claims with a long DB-side lease and fence 6.
    db.claim_with_lease_fenced(
        id,
        v1,
        "lease_owner",
        "lease_until",
        "fence",
        PropertyValue::from("A"),
        std::time::Duration::from_secs(3600),
        6,
        PropertyMapBuilder::new().build(),
    )
    .unwrap();

    // B tries the stale v1 with an even-higher fence 7; the lease is held and
    // unexpired, so the claim gate rejects it with CasMismatch (the fence gate
    // is not even reached — a higher fence cannot bypass a live lease).
    let err = db
        .claim_with_lease_fenced(
            id,
            v1,
            "lease_owner",
            "lease_until",
            "fence",
            PropertyValue::from("B"),
            std::time::Duration::from_secs(3600),
            7,
            PropertyMapBuilder::new().build(),
        )
        .expect_err("held lease must refuse the claim regardless of fence");
    match err {
        Error::Transaction(TransactionError::CasMismatch { .. }) => {}
        other => panic!("expected CasMismatch (claim gate), got {other:?}"),
    }
    assert_eq!(
        db.get_node(id)
            .unwrap()
            .get_property("lease_owner")
            .and_then(|v| v.as_str().map(String::from)),
        Some("A".to_string()),
        "A keeps the run"
    );
}

// -------------------------------------------------------------------------
// Piece 1: concurrent fenced steal of an expired lease — exactly one wins, the
// loser is fenced out (`FenceTooLow`, not merely a lost version race), and the
// winner's committed fence strictly exceeds the pre-steal fence.
//
// Both threads request `lease_ttl = 0`, so the winner's DB-computed lease is
// already expired for the later-committing loser. That routes the loser through
// the lease-expired OR-branch of the CLAIM gate (its stale base version no
// longer matches), so it reaches — and is stopped by — the FENCE gate. This is
// the whole point: with a long ttl the loser would abort on the version/lease
// claim gate (`CasMismatch`) and never exercise the fence gate at all, so the
// test would pass even if Piece 1's fence gate were deleted. Asserting the
// loser's error is specifically `FenceTooLow` (and that both stealers computed
// the SAME new_fence = 11) makes this test FAIL-IF-FENCE-REMOVED: without the
// fence gate the loser's lease-expired steal would COMMIT (`successes == 2`,
// tripping the exactly-one-winner assertion) instead of being rejected.
// -------------------------------------------------------------------------
#[test]
fn concurrent_fenced_steal_one_winner_distinct_fence() {
    use std::sync::{Arc, Barrier};

    let db = Arc::new(AletheiaDB::new().unwrap());
    let id = db
        .create_node(
            "WorkflowRun",
            PropertyMapBuilder::new()
                .insert("fence", 10_i64)
                .insert("lease_owner", "A")
                .insert("lease_until", past_ts(60).wallclock())
                .build(),
        )
        .unwrap();
    let v1 = db.get_node(id).unwrap().current_version;

    // B and C both open on the same base (expired lease, fence 10) and race to
    // commit a fenced steal with new_fence = 11 and a zero-length lease (so the
    // winner's lease is already expired for the loser's later commit).
    let barrier = Arc::new(Barrier::new(2));
    let mut handles = Vec::new();
    for owner in ["B", "C"] {
        let db = Arc::clone(&db);
        let barrier = Arc::clone(&barrier);
        handles.push(std::thread::spawn(move || {
            let mut tx = db.write_transaction().unwrap();
            tx.claim_with_lease_fenced(
                id,
                v1,
                "lease_owner",
                "lease_until",
                "fence",
                PropertyValue::from(owner),
                std::time::Duration::from_secs(0), // lease expires immediately
                11,
                PropertyMapBuilder::new().build(),
            )
            .unwrap();
            barrier.wait();
            tx.commit()
        }));
    }
    let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
    let successes = results.iter().filter(|r| r.is_ok()).count();
    assert_eq!(successes, 1, "exactly one fenced steal wins: {results:?}");

    // The loser must be fenced out (its stale-read fence 11 does not beat the
    // winner's now-committed 11), NOT merely a version/lease race loss. This is
    // the assertion that exercises — and depends on — Piece 1's fence gate.
    let loser_err = results
        .into_iter()
        .find_map(|r| r.err())
        .expect("exactly one commit must fail");
    match loser_err {
        Error::Transaction(TransactionError::FenceTooLow {
            new_fence, stored, ..
        }) => {
            assert_eq!(
                new_fence, 11,
                "the loser tried to install the stale-read 11"
            );
            assert_eq!(stored, 11, "the winner had already committed fence 11");
        }
        other => panic!("expected the loser to be fenced out (FenceTooLow), got {other:?}"),
    }

    let node = db.get_node(id).unwrap();
    assert_eq!(
        node.get_property("fence").and_then(|v| v.as_int()),
        Some(11),
        "winner installed fence 11 (strictly above the prior 10)"
    );
}

// -------------------------------------------------------------------------
// Piece 2: the DB computes `lease_until` from `engine_now + ttl`, ignoring the
// caller's clock entirely. Even with a huge TTL the deadline is bounded near
// now + ttl (not some far-future caller-supplied absolute).
// -------------------------------------------------------------------------
#[test]
fn fenced_claim_lease_until_is_db_computed() {
    let db = AletheiaDB::new().unwrap();
    let id = db
        .create_node("WorkflowRun", PropertyMapBuilder::new().build())
        .unwrap();
    let v1 = db.get_node(id).unwrap().current_version;

    let before = time::now().wallclock();
    db.claim_with_lease_fenced(
        id,
        v1,
        "lease_owner",
        "lease_until",
        "fence",
        PropertyValue::from("A"),
        std::time::Duration::from_secs(30),
        1,
        PropertyMapBuilder::new().build(),
    )
    .unwrap();
    let after = time::now().wallclock();

    let lease_until = db
        .get_node(id)
        .unwrap()
        .get_property("lease_until")
        .and_then(|v| v.as_int())
        .expect("lease_until stamped as integer micros");

    let ttl_us = 30 * 1_000_000_i64;
    // The DB-computed deadline is now + ttl, bracketed by the two clock reads.
    assert!(
        lease_until >= before + ttl_us && lease_until <= after + ttl_us,
        "lease_until ({lease_until}) must be engine_now + 30s, within [{}, {}]",
        before + ttl_us,
        after + ttl_us
    );
}
