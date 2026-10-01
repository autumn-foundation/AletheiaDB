//! Compare-and-set (CAS) + lease/claim primitive tests (Issue #3577).
//!
//! Covers the conditional-write kernel: version-keyed CAS on nodes and edges
//! (full-replace semantics), the non-retriable `CasMismatch` abort, the
//! under-guard first-committer-wins behavior under real concurrency, and the
//! lease/claim convention layered on top (claim iff version matches OR the
//! lease is expired at the commit timestamp).

use aletheiadb::api::transaction::WriteOps;
use aletheiadb::core::error::{Error, TransactionError};
use aletheiadb::core::hlc::HybridTimestamp;
use aletheiadb::core::id::VersionId;
use aletheiadb::core::property::PropertyValue;
use aletheiadb::core::temporal::{Timestamp, time};
use aletheiadb::{AletheiaDB, PropertyMapBuilder};
use std::sync::{Arc, Barrier};

/// A `lease_until` timestamp `secs` seconds in the future of now.
fn future_ts(secs: i64) -> Timestamp {
    HybridTimestamp::new(time::now().wallclock() + secs * 1_000_000, 0).expect("ts")
}

/// A `lease_until` timestamp `secs` seconds in the past of now.
fn past_ts(secs: i64) -> Timestamp {
    HybridTimestamp::new(time::now().wallclock() - secs * 1_000_000, 0).expect("ts")
}

/// Assert an error is a non-retriable `CasMismatch` with the expected `actual`.
fn assert_cas_mismatch(err: &Error, expect_actual_some: Option<bool>) {
    match err {
        Error::Transaction(TransactionError::CasMismatch { actual, .. }) => {
            if let Some(some) = expect_actual_some {
                assert_eq!(
                    actual.is_some(),
                    some,
                    "CasMismatch.actual presence mismatch: {actual:?}"
                );
            }
        }
        other => panic!("expected TransactionError::CasMismatch, got {other:?}"),
    }
}

// -------------------------------------------------------------------------
// 1. CAS with matching expected_version succeeds and replaces the map.
// -------------------------------------------------------------------------
#[test]
fn cas_node_matching_version_succeeds_and_replaces() {
    let db = AletheiaDB::new().unwrap();
    let id = db
        .create_node(
            "Doc",
            PropertyMapBuilder::new()
                .insert("title", "v1")
                .insert("stale_key", "keep?")
                .build(),
        )
        .unwrap();
    let v1 = db.get_node(id).unwrap().current_version;

    // Full-replace map: no `stale_key`.
    let new_version = db
        .compare_and_set_node(
            id,
            v1,
            PropertyMapBuilder::new().insert("title", "v2").build(),
        )
        .expect("CAS with matching version must succeed");

    let node = db.get_node(id).unwrap();
    assert_eq!(node.current_version, new_version, "returned == new head");
    assert_ne!(new_version, v1, "new version differs from expected");
    assert_eq!(
        node.get_property("title")
            .and_then(|v| v.as_str().map(String::from)),
        Some("v2".to_string())
    );
    assert!(
        node.get_property("stale_key").is_none(),
        "CAS is a full REPLACE: stale_key must be gone"
    );
}

// -------------------------------------------------------------------------
// 2. CAS with a stale expected_version fails; entity unchanged.
// -------------------------------------------------------------------------
#[test]
fn cas_node_stale_version_fails_entity_unchanged() {
    let db = AletheiaDB::new().unwrap();
    let id = db
        .create_node(
            "Doc",
            PropertyMapBuilder::new().insert("title", "v1").build(),
        )
        .unwrap();
    let v1 = db.get_node(id).unwrap().current_version;

    // Advance the head to v2 via a normal update so v1 is now stale.
    db.update_node_with_valid_time(
        id,
        PropertyMapBuilder::new().insert("title", "v2").build(),
        None,
    )
    .unwrap();
    let v2 = db.get_node(id).unwrap().current_version;
    assert_ne!(v1, v2);

    let err = db
        .compare_and_set_node(
            id,
            v1,
            PropertyMapBuilder::new().insert("title", "vX").build(),
        )
        .expect_err("stale CAS must fail");
    assert_cas_mismatch(&err, Some(true));

    // Entity is UNCHANGED: head still v2, title still v2, no new version.
    let node = db.get_node(id).unwrap();
    assert_eq!(
        node.current_version, v2,
        "head must be unchanged by failed CAS"
    );
    assert_eq!(
        node.get_property("title")
            .and_then(|v| v.as_str().map(String::from)),
        Some("v2".to_string()),
        "title must be unchanged (vX never written)"
    );
}

// -------------------------------------------------------------------------
// 3. Concurrent CAS: exactly one commits, the other gets CasMismatch.
//    This proves the AUTHORITATIVE under-guard re-check (not a pre-lock-only
//    check): both threads buffer with expected == v1 on the same base, then
//    race to commit.
// -------------------------------------------------------------------------
#[test]
fn concurrent_cas_first_committer_wins() {
    let db = Arc::new(AletheiaDB::new().unwrap());
    let id = db
        .create_node(
            "Claim",
            PropertyMapBuilder::new().insert("state", "free").build(),
        )
        .unwrap();
    let v1 = db.get_node(id).unwrap().current_version;

    let barrier = Arc::new(Barrier::new(2));
    let mut handles = Vec::new();
    for owner in ["A", "B"] {
        let db = Arc::clone(&db);
        let barrier = Arc::clone(&barrier);
        handles.push(std::thread::spawn(move || {
            // Open a tx and buffer the CAS BEFORE the barrier, so both hold
            // expected == v1 against the same committed base.
            let mut tx = db.write_transaction().unwrap();
            tx.compare_and_set_node(
                id,
                v1,
                PropertyMapBuilder::new().insert("owner", owner).build(),
            )
            .unwrap();
            barrier.wait();
            // Race to commit; the timestamp + historical guard serialize this.
            tx.commit()
        }));
    }

    let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
    let successes = results.iter().filter(|r| r.is_ok()).count();
    let mismatches = results
        .iter()
        .filter(|r| matches!(r, Err(e) if matches!(e, Error::Transaction(TransactionError::CasMismatch { .. }))))
        .count();

    assert_eq!(successes, 1, "exactly one CAS must win: {results:?}");
    assert_eq!(
        mismatches, 1,
        "the loser must get CasMismatch (non-retriable): {results:?}"
    );

    // Exactly one version was appended beyond v1.
    let node = db.get_node(id).unwrap();
    assert_ne!(node.current_version, v1);
    assert!(
        node.get_property("owner").is_some(),
        "winner stamped an owner"
    );
}

// -------------------------------------------------------------------------
// 4. CAS on a nonexistent / deleted node -> CasMismatch { actual: None }.
// -------------------------------------------------------------------------
#[test]
fn cas_nonexistent_node_is_cas_mismatch_none() {
    let db = AletheiaDB::new().unwrap();
    // Never-created id.
    let ghost = aletheiadb::core::NodeId::new(999_999).unwrap();
    let err = db
        .compare_and_set_node(
            ghost,
            VersionId::new(1).unwrap(),
            PropertyMapBuilder::new().insert("x", 1_i64).build(),
        )
        .expect_err("CAS on nonexistent node must fail, not panic");
    assert_cas_mismatch(&err, Some(false));

    // Deleted node.
    let id = db
        .create_node(
            "Doc",
            PropertyMapBuilder::new().insert("title", "v1").build(),
        )
        .unwrap();
    let v1 = db.get_node(id).unwrap().current_version;
    db.delete_node_with_valid_time(id, None).unwrap();
    let err = db
        .compare_and_set_node(
            id,
            v1,
            PropertyMapBuilder::new().insert("title", "v2").build(),
        )
        .expect_err("CAS on deleted node must fail");
    // A deleted node is absent from the write path's read -> actual None.
    assert_cas_mismatch(&err, Some(false));
}

// -------------------------------------------------------------------------
// 5. Lease: claim when unclaimed (no lease props, version matches) succeeds.
// -------------------------------------------------------------------------
#[test]
fn claim_unclaimed_node_stamps_owner_and_lease() {
    let db = AletheiaDB::new().unwrap();
    let id = db
        .create_node(
            "Workflow",
            PropertyMapBuilder::new().insert("wf", "W1").build(),
        )
        .unwrap();
    let v1 = db.get_node(id).unwrap().current_version;

    let lease_until = future_ts(60);
    let new_version = db
        .claim_with_lease(
            id,
            v1,
            "lease_owner",
            "lease_until",
            PropertyValue::from("worker-a"),
            lease_until,
            PropertyMapBuilder::new().insert("wf", "W1").build(),
        )
        .expect("claim of unclaimed node must succeed");

    let node = db.get_node(id).unwrap();
    assert_eq!(node.current_version, new_version);
    assert_eq!(
        node.get_property("lease_owner")
            .and_then(|v| v.as_str().map(String::from)),
        Some("worker-a".to_string())
    );
    assert_eq!(
        node.get_property("lease_until").and_then(|v| v.as_int()),
        Some(lease_until.wallclock())
    );
}

// -------------------------------------------------------------------------
// 6. Lease held & NOT expired + stale version -> refused (CasMismatch).
// -------------------------------------------------------------------------
#[test]
fn claim_refused_when_lease_held_and_version_stale() {
    let db = AletheiaDB::new().unwrap();
    let id = db
        .create_node(
            "Workflow",
            PropertyMapBuilder::new().insert("wf", "W1").build(),
        )
        .unwrap();
    let v1 = db.get_node(id).unwrap().current_version;

    // Owner A claims with a lease far in the future.
    db.claim_with_lease(
        id,
        v1,
        "lease_owner",
        "lease_until",
        PropertyValue::from("A"),
        future_ts(3600),
        PropertyMapBuilder::new().insert("wf", "W1").build(),
    )
    .unwrap();

    // Owner B tries with the now-stale v1; lease is held & unexpired -> refused.
    let err = db
        .claim_with_lease(
            id,
            v1,
            "lease_owner",
            "lease_until",
            PropertyValue::from("B"),
            future_ts(3600),
            PropertyMapBuilder::new().insert("wf", "W1").build(),
        )
        .expect_err("claim must be refused while lease held");
    assert_cas_mismatch(&err, Some(true));

    // Still owned by A.
    let node = db.get_node(id).unwrap();
    assert_eq!(
        node.get_property("lease_owner")
            .and_then(|v| v.as_str().map(String::from)),
        Some("A".to_string())
    );
}

// -------------------------------------------------------------------------
// 7 + 8. Lease EXPIRED + stale version -> claim succeeds (OR branch), and
//    expiry is judged at the commit timestamp.
// -------------------------------------------------------------------------
#[test]
fn claim_succeeds_when_lease_expired_despite_stale_version() {
    let db = AletheiaDB::new().unwrap();
    let id = db
        .create_node(
            "Workflow",
            PropertyMapBuilder::new().insert("wf", "W1").build(),
        )
        .unwrap();
    let v1 = db.get_node(id).unwrap().current_version;

    // Owner A claims but with a lease that is ALREADY in the past (expired
    // relative to any commit timestamp that follows).
    db.claim_with_lease(
        id,
        v1,
        "lease_owner",
        "lease_until",
        PropertyValue::from("A"),
        past_ts(60),
        PropertyMapBuilder::new().insert("wf", "W1").build(),
    )
    .unwrap();
    // Head is now v2; v1 is stale.
    let v2 = db.get_node(id).unwrap().current_version;
    assert_ne!(v1, v2);

    // Owner B claims with the STALE v1. Version does not match, but the lease
    // is expired at commit time -> the OR branch admits the claim.
    let new_version = db
        .claim_with_lease(
            id,
            v1,
            "lease_owner",
            "lease_until",
            PropertyValue::from("B"),
            future_ts(60),
            PropertyMapBuilder::new().insert("wf", "W1").build(),
        )
        .expect("claim must succeed when existing lease is expired");

    let node = db.get_node(id).unwrap();
    assert_eq!(node.current_version, new_version);
    assert_eq!(
        node.get_property("lease_owner")
            .and_then(|v| v.as_str().map(String::from)),
        Some("B".to_string()),
        "B took over the expired lease"
    );
}

// -------------------------------------------------------------------------
// 8b. Concurrent claim of an EXPIRED lease: exactly one claimant wins.
//     Both threads open on the same base (an expired lease + stale v1) and
//     race to commit a FUTURE lease. The lease-expired OR-branch admits both
//     pre-lock (lease claims are excluded from the pure-CAS fast-path), so the
//     AUTHORITATIVE under-guard re-check is what enforces "exactly one winner":
//     the loser observes the winner's fresh (future) lease and aborts.
// -------------------------------------------------------------------------
#[test]
fn concurrent_expired_lease_claim_exactly_one_wins() {
    let db = Arc::new(AletheiaDB::new().unwrap());
    let id = db
        .create_node("Job", PropertyMapBuilder::new().insert("j", "J1").build())
        .unwrap();
    let v1 = db.get_node(id).unwrap().current_version;

    // Owner A claims with a lease ALREADY in the past -> head advances to v2 and
    // the lease is expired relative to any later commit timestamp.
    db.claim_with_lease(
        id,
        v1,
        "lease_owner",
        "lease_until",
        PropertyValue::from("A"),
        past_ts(60),
        PropertyMapBuilder::new().insert("j", "J1").build(),
    )
    .unwrap();

    // Two claimants B and C, both opened on the SAME base (v1 is stale, lease
    // expired), each writing a FUTURE lease. Exactly one may win.
    let barrier = Arc::new(Barrier::new(2));
    let mut handles = Vec::new();
    for owner in ["B", "C"] {
        let db = Arc::clone(&db);
        let barrier = Arc::clone(&barrier);
        handles.push(std::thread::spawn(move || {
            let mut tx = db.write_transaction().unwrap();
            tx.claim_with_lease(
                id,
                v1,
                "lease_owner",
                "lease_until",
                PropertyValue::from(owner),
                future_ts(3600),
                PropertyMapBuilder::new().insert("j", "J1").build(),
            )
            .unwrap();
            barrier.wait();
            tx.commit()
        }));
    }

    let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
    let successes = results.iter().filter(|r| r.is_ok()).count();
    let mismatches = results
        .iter()
        .filter(|r| matches!(r, Err(e) if matches!(e, Error::Transaction(TransactionError::CasMismatch { .. }))))
        .count();

    assert_eq!(
        successes, 1,
        "exactly one expired-lease claim wins: {results:?}"
    );
    assert_eq!(
        mismatches, 1,
        "the loser must get CasMismatch (lease now held by the winner): {results:?}"
    );

    // The winner holds a future lease.
    let node = db.get_node(id).unwrap();
    let owner = node
        .get_property("lease_owner")
        .and_then(|v| v.as_str().map(String::from));
    assert!(
        owner == Some("B".to_string()) || owner == Some("C".to_string()),
        "winner is one of the two claimants, got {owner:?}"
    );
    assert!(
        node.get_property("lease_until")
            .and_then(|v| v.as_int())
            .map(|until| until > time::now().wallclock())
            .unwrap_or(false),
        "winner installed a future lease_until"
    );
}

// -------------------------------------------------------------------------
// 8c. claim_with_lease on a nonexistent node -> CasMismatch { actual: None }
//     (no panic, mirrors the pure-CAS nonexistent-node contract).
// -------------------------------------------------------------------------
#[test]
fn claim_with_lease_nonexistent_node_is_cas_mismatch_none() {
    let db = AletheiaDB::new().unwrap();
    let ghost = aletheiadb::core::NodeId::new(888_888).unwrap();
    let err = db
        .claim_with_lease(
            ghost,
            VersionId::new(1).unwrap(),
            "lease_owner",
            "lease_until",
            PropertyValue::from("worker"),
            future_ts(60),
            PropertyMapBuilder::new().insert("j", "J1").build(),
        )
        .expect_err("claim of a nonexistent node must fail, not panic");
    assert_cas_mismatch(&err, Some(false));
}

// -------------------------------------------------------------------------
// 8d. Non-integer `lease_until` is treated as UNCLAIMED (expired): a claim on
//     a stale version whose current lease_until is a String is admitted. This
//     documents the `as_int() -> None => unclaimed` behavior (a malformed lease
//     is not a valid hold). Behavior is intentional; asserted, not changed.
// -------------------------------------------------------------------------
#[test]
fn non_integer_lease_until_is_treated_as_unclaimed() {
    let db = AletheiaDB::new().unwrap();
    // Node carries a STRING lease_until (not the integer-microseconds convention).
    let id = db
        .create_node(
            "Job",
            PropertyMapBuilder::new()
                .insert("j", "J1")
                .insert("lease_until", "whenever")
                .build(),
        )
        .unwrap();
    let v1 = db.get_node(id).unwrap().current_version;

    // Advance the head (PATCH update preserves the String lease_until) so v1 is
    // stale. The version branch will NOT match at claim time.
    db.update_node_with_valid_time(
        id,
        PropertyMapBuilder::new().insert("j", "J1b").build(),
        None,
    )
    .unwrap();
    assert_ne!(db.get_node(id).unwrap().current_version, v1);

    // Claim with the STALE v1. Version does not match, but the current
    // lease_until is a non-integer String -> treated as unclaimed -> admitted.
    let new_version = db
        .claim_with_lease(
            id,
            v1,
            "lease_owner",
            "lease_until",
            PropertyValue::from("taker"),
            future_ts(60),
            PropertyMapBuilder::new().insert("j", "J1b").build(),
        )
        .expect("non-integer lease_until must be treated as unclaimed, admitting the claim");

    let node = db.get_node(id).unwrap();
    assert_eq!(node.current_version, new_version);
    assert_eq!(
        node.get_property("lease_owner")
            .and_then(|v| v.as_str().map(String::from)),
        Some("taker".to_string())
    );
    // The claim installed the integer-microseconds convention going forward.
    assert!(
        node.get_property("lease_until")
            .and_then(|v| v.as_int())
            .is_some(),
        "claim overwrote the malformed lease_until with an integer"
    );
}

// -------------------------------------------------------------------------
// 10. Returned versions chain: CAS(expected=v2) ok, CAS(expected=v1) fails.
// -------------------------------------------------------------------------
#[test]
fn cas_returned_version_chains() {
    let db = AletheiaDB::new().unwrap();
    let id = db
        .create_node("Doc", PropertyMapBuilder::new().insert("n", 0_i64).build())
        .unwrap();
    let v1 = db.get_node(id).unwrap().current_version;

    let v2 = db
        .compare_and_set_node(id, v1, PropertyMapBuilder::new().insert("n", 1_i64).build())
        .unwrap();

    // CAS with the freshly returned v2 succeeds.
    let v3 = db
        .compare_and_set_node(id, v2, PropertyMapBuilder::new().insert("n", 2_i64).build())
        .expect("CAS with the returned new version must succeed");
    assert_ne!(v2, v3);

    // CAS with the now-stale v1 fails.
    let err = db
        .compare_and_set_node(
            id,
            v1,
            PropertyMapBuilder::new().insert("n", 99_i64).build(),
        )
        .expect_err("CAS with an old version must fail");
    assert_cas_mismatch(&err, Some(true));
}

// -------------------------------------------------------------------------
// 11. valid_time / provenance options honored on CAS (parity with update).
// -------------------------------------------------------------------------
#[test]
fn cas_honors_valid_time_and_provenance_options() {
    use aletheiadb::api::transaction::WriteRequestOptions;
    use aletheiadb::core::provenance::Provenance;

    let db = AletheiaDB::new().unwrap();
    // Create the node backdated 2 minutes so a CAS backdated 30s is still AFTER
    // creation (valid_from-before-creation is rejected, matching update parity).
    let id = db
        .create_node_with_valid_time(
            "Doc",
            PropertyMapBuilder::new().insert("title", "v1").build(),
            Some(past_ts(120)),
        )
        .unwrap();
    let v1 = db.get_node(id).unwrap().current_version;

    let valid_from = past_ts(30);
    let provenance = Provenance::builder()
        .source("cas-test")
        .confidence(0.9)
        .build()
        .unwrap();
    let options = WriteRequestOptions::new()
        .with_valid_from(valid_from)
        .with_provenance(provenance);

    let new_version = db
        .compare_and_set_node_with_options(
            id,
            v1,
            PropertyMapBuilder::new().insert("title", "v2").build(),
            options,
        )
        .expect("CAS with options must succeed");

    let node = db.get_node(id).unwrap();
    assert_eq!(node.current_version, new_version);
    // Provenance recorded on the new version.
    let prov = db.get_node_provenance(id).unwrap();
    assert!(
        prov.map(|p| p.source() == Some("cas-test"))
            .unwrap_or(false),
        "CAS must honor write-time provenance"
    );
}

// -------------------------------------------------------------------------
// 12. Edge CAS: endpoints/type immutable; mismatch aborts.
// -------------------------------------------------------------------------
#[test]
fn cas_edge_replaces_props_and_mismatch_aborts() {
    let db = AletheiaDB::new().unwrap();
    let a = db
        .create_node("P", PropertyMapBuilder::new().insert("n", "a").build())
        .unwrap();
    let b = db
        .create_node("P", PropertyMapBuilder::new().insert("n", "b").build())
        .unwrap();
    let e = db
        .create_edge(
            a,
            b,
            "KNOWS",
            PropertyMapBuilder::new()
                .insert("since", 2020_i64)
                .insert("weight", 5_i64)
                .build(),
        )
        .unwrap();
    let ev1 = db.get_edge(e).unwrap().current_version;

    // Matching CAS: full-replace props (drop weight); endpoints/type preserved.
    let ev2 = db
        .compare_and_set_edge(
            e,
            ev1,
            PropertyMapBuilder::new().insert("since", 2024_i64).build(),
        )
        .expect("edge CAS with matching version must succeed");
    let edge = db.get_edge(e).unwrap();
    assert_eq!(edge.current_version, ev2);
    assert_eq!(edge.source, a, "source immutable");
    assert_eq!(edge.target, b, "target immutable");
    assert!(edge.has_label_str("KNOWS"), "type immutable");
    assert_eq!(
        edge.get_property("since").and_then(|v| v.as_int()),
        Some(2024)
    );
    assert!(
        edge.get_property("weight").is_none(),
        "full replace dropped weight"
    );

    // Stale CAS aborts.
    let err = db
        .compare_and_set_edge(
            e,
            ev1,
            PropertyMapBuilder::new().insert("since", 1999_i64).build(),
        )
        .expect_err("stale edge CAS must fail");
    assert_cas_mismatch(&err, Some(true));
    // Unchanged.
    assert_eq!(
        db.get_edge(e)
            .unwrap()
            .get_property("since")
            .and_then(|v| v.as_int()),
        Some(2024)
    );
}

// -------------------------------------------------------------------------
// 13. `CasMismatch` Display formats both the `actual: Some` and `actual: None`
//     arms (Issue #3577). Exercises `format_cas_mismatch` directly so the
//     Display arm of the error variant is covered for both shapes.
// -------------------------------------------------------------------------
#[test]
fn cas_mismatch_display_formats_some_and_none() {
    let expected = VersionId::new(5).unwrap();

    // actual: Some(current) -> mentions the current version.
    let some_err = TransactionError::CasMismatch {
        expected,
        actual: Some(VersionId::new(9).unwrap()),
    };
    let s = format!("{some_err}");
    assert!(
        s.contains("compare-and-set failed") && s.contains('5') && s.contains('9'),
        "Some-arm Display must name expected + current versions, got: {s}"
    );
    assert!(
        s.contains("current version"),
        "Some-arm Display must reference the current version, got: {s}"
    );

    // actual: None -> "does not exist".
    let none_err = TransactionError::CasMismatch {
        expected,
        actual: None,
    };
    let s = format!("{none_err}");
    assert!(
        s.contains("compare-and-set failed") && s.contains('5') && s.contains("does not exist"),
        "None-arm Display must name expected version + absence, got: {s}"
    );
}

// -------------------------------------------------------------------------
// 14. Top-level `AletheiaDB::compare_and_set_edge_with_options` wrapper:
//     success (honors provenance) AND stale-version Err arm. Covers the
//     convenience wrapper surface (Issue #3577).
// -------------------------------------------------------------------------
#[test]
fn cas_edge_with_options_succeeds_and_stale_fails() {
    use aletheiadb::api::transaction::WriteRequestOptions;
    use aletheiadb::core::provenance::Provenance;

    let db = AletheiaDB::new().unwrap();
    let a = db
        .create_node("P", PropertyMapBuilder::new().insert("n", "a").build())
        .unwrap();
    let b = db
        .create_node("P", PropertyMapBuilder::new().insert("n", "b").build())
        .unwrap();
    let e = db
        .create_edge(
            a,
            b,
            "KNOWS",
            PropertyMapBuilder::new().insert("since", 2020_i64).build(),
        )
        .unwrap();
    let ev1 = db.get_edge(e).unwrap().current_version;

    let provenance = Provenance::builder()
        .source("cas-edge-opts")
        .confidence(0.75)
        .build()
        .unwrap();
    let options = WriteRequestOptions::new().with_provenance(provenance);

    // Ok arm through the with_options wrapper.
    let ev2 = db
        .compare_and_set_edge_with_options(
            e,
            ev1,
            PropertyMapBuilder::new().insert("since", 2024_i64).build(),
            options,
        )
        .expect("edge CAS with options + matching version must succeed");
    assert_ne!(ev1, ev2);
    let edge = db.get_edge(e).unwrap();
    assert_eq!(edge.current_version, ev2);
    assert_eq!(
        edge.get_property("since").and_then(|v| v.as_int()),
        Some(2024)
    );
    let prov = db.get_edge_provenance(e).unwrap();
    assert!(
        prov.map(|p| p.source() == Some("cas-edge-opts"))
            .unwrap_or(false),
        "edge CAS must honor write-time provenance"
    );

    // Err arm through the with_options wrapper: stale ev1 now fails.
    let err = db
        .compare_and_set_edge_with_options(
            e,
            ev1,
            PropertyMapBuilder::new().insert("since", 1999_i64).build(),
            WriteRequestOptions::new(),
        )
        .expect_err("stale edge CAS with options must fail");
    assert_cas_mismatch(&err, Some(true));
    assert_eq!(
        db.get_edge(e)
            .unwrap()
            .get_property("since")
            .and_then(|v| v.as_int()),
        Some(2024),
        "failed edge CAS must not mutate the edge"
    );
}

// -------------------------------------------------------------------------
// 15. Top-level `AletheiaDB::claim_with_lease_with_options` wrapper: success
//     (backdated valid_from + provenance) AND the stale-version-held-lease Err
//     arm. Covers the convenience wrapper surface (Issue #3577).
// -------------------------------------------------------------------------
#[test]
fn claim_with_lease_with_options_succeeds_and_stale_fails() {
    use aletheiadb::api::transaction::WriteRequestOptions;
    use aletheiadb::core::provenance::Provenance;

    let db = AletheiaDB::new().unwrap();
    // Backdate creation so a backdated claim valid_from is still after creation.
    let id = db
        .create_node_with_valid_time(
            "Workflow",
            PropertyMapBuilder::new().insert("wf", "W1").build(),
            Some(past_ts(120)),
        )
        .unwrap();
    let v1 = db.get_node(id).unwrap().current_version;

    let provenance = Provenance::builder()
        .source("claim-opts")
        .confidence(0.8)
        .build()
        .unwrap();
    let options = WriteRequestOptions::new()
        .with_valid_from(past_ts(30))
        .with_provenance(provenance);

    // Ok arm: claim the unclaimed node with a FUTURE lease, via with_options.
    let new_version = db
        .claim_with_lease_with_options(
            id,
            v1,
            "lease_owner",
            "lease_until",
            PropertyValue::from("worker-a"),
            future_ts(3600),
            PropertyMapBuilder::new().insert("wf", "W1").build(),
            options,
        )
        .expect("claim with options must succeed");
    let node = db.get_node(id).unwrap();
    assert_eq!(node.current_version, new_version);
    assert_eq!(
        node.get_property("lease_owner")
            .and_then(|v| v.as_str().map(String::from)),
        Some("worker-a".to_string())
    );
    let prov = db.get_node_provenance(id).unwrap();
    assert!(
        prov.map(|p| p.source() == Some("claim-opts"))
            .unwrap_or(false),
        "claim must honor write-time provenance"
    );

    // Err arm: a second claimant with the now-stale v1 and the lease still held
    // (future expiry) is refused.
    let err = db
        .claim_with_lease_with_options(
            id,
            v1,
            "lease_owner",
            "lease_until",
            PropertyValue::from("worker-b"),
            future_ts(3600),
            PropertyMapBuilder::new().insert("wf", "W1").build(),
            WriteRequestOptions::new(),
        )
        .expect_err("stale claim against a held lease must fail");
    assert_cas_mismatch(&err, Some(true));
    assert_eq!(
        db.get_node(id)
            .unwrap()
            .get_property("lease_owner")
            .and_then(|v| v.as_str().map(String::from)),
        Some("worker-a".to_string()),
        "failed claim must not steal the lease"
    );
}

// -------------------------------------------------------------------------
// 16. CAS on a nonexistent EDGE -> CasMismatch { actual: None } (no panic),
//     mirroring the pure-CAS nonexistent-node contract for edges (Issue #3577).
// -------------------------------------------------------------------------
#[test]
fn cas_nonexistent_edge_is_cas_mismatch_none() {
    let db = AletheiaDB::new().unwrap();
    let ghost = aletheiadb::core::EdgeId::new(777_777).unwrap();
    let err = db
        .compare_and_set_edge(
            ghost,
            VersionId::new(1).unwrap(),
            PropertyMapBuilder::new().insert("k", "v").build(),
        )
        .expect_err("edge CAS on a nonexistent edge must fail, not panic");
    assert_cas_mismatch(&err, Some(false));
}

// -------------------------------------------------------------------------
// 17. A CAS whose `valid_from` predates the entity's creation is rejected
//     (parity with the update path's temporal invariant), for both a node and
//     an edge. Nothing is written on rejection (Issue #3577).
// -------------------------------------------------------------------------
#[test]
fn cas_valid_from_before_creation_rejected() {
    use aletheiadb::api::transaction::WriteRequestOptions;

    let db = AletheiaDB::new().unwrap();

    // NODE: created "now"; a CAS backdated 60s is before creation -> rejected.
    let n = db
        .create_node("Doc", PropertyMapBuilder::new().insert("t", "v1").build())
        .unwrap();
    let nv1 = db.get_node(n).unwrap().current_version;
    let err = db
        .compare_and_set_node_with_options(
            n,
            nv1,
            PropertyMapBuilder::new().insert("t", "v2").build(),
            WriteRequestOptions::new().with_valid_from(past_ts(60)),
        )
        .expect_err("node CAS with valid_from before creation must be rejected");
    match &err {
        Error::Temporal(_) => {}
        other => panic!("expected a Temporal error for valid_from-before-creation, got {other:?}"),
    }
    // Unchanged: head still nv1, title still v1.
    let node = db.get_node(n).unwrap();
    assert_eq!(node.current_version, nv1, "rejected node CAS wrote nothing");
    assert_eq!(
        node.get_property("t")
            .and_then(|v| v.as_str().map(String::from)),
        Some("v1".to_string())
    );

    // EDGE: created "now"; an edge CAS backdated 60s is before creation.
    let a = db
        .create_node("P", PropertyMapBuilder::new().insert("n", "a").build())
        .unwrap();
    let b = db
        .create_node("P", PropertyMapBuilder::new().insert("n", "b").build())
        .unwrap();
    let e = db
        .create_edge(
            a,
            b,
            "KNOWS",
            PropertyMapBuilder::new().insert("since", 2020_i64).build(),
        )
        .unwrap();
    let ev1 = db.get_edge(e).unwrap().current_version;
    let err = db
        .compare_and_set_edge_with_options(
            e,
            ev1,
            PropertyMapBuilder::new().insert("since", 2024_i64).build(),
            WriteRequestOptions::new().with_valid_from(past_ts(60)),
        )
        .expect_err("edge CAS with valid_from before creation must be rejected");
    match &err {
        Error::Temporal(_) => {}
        other => panic!("expected a Temporal error for valid_from-before-creation, got {other:?}"),
    }
    assert_eq!(
        db.get_edge(e).unwrap().current_version,
        ev1,
        "rejected edge CAS wrote nothing"
    );
}

// -------------------------------------------------------------------------
// 18. Concurrent EDGE CAS: exactly one commits, the other gets CasMismatch.
//     The edge analog of `concurrent_cas_first_committer_wins` -- both txns
//     buffer expected == ev1 on the same base, then race to commit, so the
//     loser is caught by the AUTHORITATIVE under-guard edge re-check rather
//     than only the pre-lock fast-path (Issue #3577).
// -------------------------------------------------------------------------
#[test]
fn concurrent_edge_cas_first_committer_wins() {
    let db = Arc::new(AletheiaDB::new().unwrap());
    let a = db
        .create_node("P", PropertyMapBuilder::new().insert("n", "a").build())
        .unwrap();
    let b = db
        .create_node("P", PropertyMapBuilder::new().insert("n", "b").build())
        .unwrap();
    let e = db
        .create_edge(
            a,
            b,
            "LINK",
            PropertyMapBuilder::new().insert("v", 0_i64).build(),
        )
        .unwrap();
    let ev1 = db.get_edge(e).unwrap().current_version;

    let barrier = Arc::new(Barrier::new(2));
    let mut handles = Vec::new();
    for tag in [1_i64, 2_i64] {
        let db = Arc::clone(&db);
        let barrier = Arc::clone(&barrier);
        handles.push(std::thread::spawn(move || {
            // Buffer the edge CAS BEFORE the barrier so both hold expected == ev1
            // against the same committed base.
            let mut tx = db.write_transaction().unwrap();
            tx.compare_and_set_edge(e, ev1, PropertyMapBuilder::new().insert("v", tag).build())
                .unwrap();
            barrier.wait();
            tx.commit()
        }));
    }

    let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
    let successes = results.iter().filter(|r| r.is_ok()).count();
    let mismatches = results
        .iter()
        .filter(|r| matches!(r, Err(e) if matches!(e, Error::Transaction(TransactionError::CasMismatch { .. }))))
        .count();

    assert_eq!(successes, 1, "exactly one edge CAS must win: {results:?}");
    assert_eq!(
        mismatches, 1,
        "the loser must get CasMismatch (non-retriable): {results:?}"
    );

    // Exactly one version was appended beyond ev1.
    let edge = db.get_edge(e).unwrap();
    assert_ne!(edge.current_version, ev1);
    assert!(
        edge.get_property("v").and_then(|v| v.as_int()).is_some(),
        "winner stamped a value"
    );
}
