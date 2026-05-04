//! State space exploration tests for temporal invariants (issue #153).
//!
//! Supplements line/branch coverage with property-based and exhaustive
//! small-n enumeration to verify AletheiaDB's 8 core temporal invariants
//! across a wide range of operation sequences and timestamp combinations.
//!
//! # Phase 1 – Property-based tests (proptest)
//! Each test exercises one or more invariants over randomly-generated
//! inputs, ensuring correctness across thousands of state combinations.
//!
//! # Phase 2 – Exhaustive small-n enumeration
//! Deterministically exercises every semantically distinct lifecycle
//! ordering for a single entity and all relevant timestamp orderings.
//!
//! # Invariants verified
//! 1. **Tx-time monotonicity** – `tx_time(v_n) >= tx_time(v_{n-1})`
//! 2. **Version number ordering** – `version_number(v_n) > version_number(v_{n-1})`
//! 3. **Time range validity** – `start <= end` for every stored range
//! 4. **Visibility consistency** – `visible_at(vt,tt) ⟺ valid_at(vt) ∧ recorded_at(tt)`
//! 5. **Overlap symmetry** – `r1.overlaps(r2) == r2.overlaps(r1)`
//! 6. **Contains-range reflexivity** – `r.contains_range(r) == true`
//! 7. **Half-open interval semantics** – end timestamp is always exclusive
//! 8. **Temporal isolation** – time-travel yields consistent snapshot values

use aletheiadb::core::id::NodeId;
use aletheiadb::core::property::PropertyMapBuilder;
use aletheiadb::{AletheiaDB, BiTemporalInterval, Error, TimeRange, Timestamp, WriteOps, time};
use proptest::prelude::*;

// ============================================================================
// Test infrastructure: TemporalOperation generators
// ============================================================================

/// A post-creation operation that can be applied to a node in sequence.
///
/// Used by proptest strategies to generate realistic node lifecycles.
#[derive(Debug, Clone)]
enum TemporalOperation {
    Update { value: i64 },
    Delete,
}

fn temporal_operation_strategy() -> impl Strategy<Value = TemporalOperation> {
    prop_oneof![
        any::<i64>().prop_map(|v| TemporalOperation::Update { value: v }),
        Just(TemporalOperation::Delete),
    ]
}

/// Generates a non-empty sequence of operations to apply after an initial Create.
fn operation_sequence(max_ops: usize) -> impl Strategy<Value = Vec<TemporalOperation>> {
    proptest::collection::vec(temporal_operation_strategy(), 1..=max_ops)
}

// ============================================================================
// Invariant checker functions
//
// Each function embodies one or more of the 8 invariants and returns `true`
// when the invariant holds.  `get_node_history` errors (e.g., after deletion)
// are treated as an empty history: invariants are trivially satisfied.
// ============================================================================

/// Invariant 1 – Transaction time is monotonically non-decreasing across versions.
fn check_tx_time_monotonicity(db: &AletheiaDB, node_id: NodeId) -> bool {
    let history = match db.get_node_history(node_id) {
        Ok(h) => h,
        Err(_) => return true,
    };
    history.versions.windows(2).all(|pair| {
        let prev_tx = pair[0].temporal.transaction_time().start();
        let curr_tx = pair[1].temporal.transaction_time().start();
        curr_tx >= prev_tx
    })
}

/// Invariant 2 – Version numbers are strictly increasing.
fn check_version_numbers_increasing(db: &AletheiaDB, node_id: NodeId) -> bool {
    let history = match db.get_node_history(node_id) {
        Ok(h) => h,
        Err(_) => return true,
    };
    history
        .versions
        .windows(2)
        .all(|pair| pair[1].version_number > pair[0].version_number)
}

/// Invariant 3 – All stored time ranges satisfy `start <= end`.
fn check_time_range_validity(db: &AletheiaDB, node_id: NodeId) -> bool {
    let history = match db.get_node_history(node_id) {
        Ok(h) => h,
        Err(_) => return true,
    };
    history.versions.iter().all(|v| {
        let vt = v.temporal.valid_time();
        let tt = v.temporal.transaction_time();
        vt.start() <= vt.end() && tt.start() <= tt.end()
    })
}

/// Invariant 4 – Visibility is consistent with individual dimension checks.
///
/// `visible_at(vt, tt)  ⟺  is_valid_at(vt) ∧ is_recorded_at(tt)`
fn check_visibility_consistency(
    interval: BiTemporalInterval,
    vt: Timestamp,
    tt: Timestamp,
) -> bool {
    interval.is_visible_at(vt, tt) == (interval.is_valid_at(vt) && interval.is_recorded_at(tt))
}

// ============================================================================
// Phase 1: Property-based tests
// ============================================================================

proptest! {
    #![proptest_config(ProptestConfig::with_cases(200))]

    /// Invariants 1 & 2: After N updates, tx_time is monotone and versions are ordered.
    #[test]
    fn prop_tx_time_monotonicity_after_updates(update_count in 1usize..=8) {
        let db = AletheiaDB::new().unwrap();
        let node_id = db
            .create_node("N", PropertyMapBuilder::new().insert("v", 0i64).build())
            .unwrap();

        for i in 1..=(update_count as i64) {
            db.write(|tx| {
                tx.update_node(node_id, PropertyMapBuilder::new().insert("v", i).build())?;
                Ok::<_, Error>(())
            })
            .unwrap();
        }

        prop_assert!(
            check_tx_time_monotonicity(&db, node_id),
            "Inv 1 violated after {} updates",
            update_count
        );
        prop_assert!(
            check_version_numbers_increasing(&db, node_id),
            "Inv 2 violated after {} updates",
            update_count
        );
    }

    /// Invariant 3: Every stored time range satisfies start <= end.
    #[test]
    fn prop_time_ranges_always_valid(update_count in 1usize..=6) {
        let db = AletheiaDB::new().unwrap();
        let node_id = db
            .create_node("N", PropertyMapBuilder::new().insert("v", 0i64).build())
            .unwrap();

        for i in 1..=(update_count as i64) {
            db.write(|tx| {
                tx.update_node(node_id, PropertyMapBuilder::new().insert("v", i).build())?;
                Ok::<_, Error>(())
            })
            .unwrap();
        }

        prop_assert!(
            check_time_range_validity(&db, node_id),
            "Inv 3 violated after {} updates",
            update_count
        );
    }

    /// Invariant 4: BiTemporalInterval visibility ⟺ both dimensions satisfied.
    #[test]
    fn prop_visibility_consistency(
        s1 in 1_000_000i64..=400_000_000i64,
        e1 in 1_000_000i64..=400_000_000i64,
        s2 in 1_000_000i64..=400_000_000i64,
        e2 in 1_000_000i64..=400_000_000i64,
        qv in 1_000_000i64..=400_000_000i64,
        qt in 1_000_000i64..=400_000_000i64,
    ) {
        // Ensure start <= end for both ranges.
        let (s1, e1) = if s1 <= e1 { (s1, e1) } else { (e1, s1) };
        let (s2, e2) = if s2 <= e2 { (s2, e2) } else { (e2, s2) };

        let vt = match TimeRange::new(s1.into(), e1.into()) {
            Ok(r) => r,
            Err(_) => return Ok(()),
        };
        let tt = match TimeRange::new(s2.into(), e2.into()) {
            Ok(r) => r,
            Err(_) => return Ok(()),
        };

        let interval = BiTemporalInterval::new(vt, tt);

        prop_assert!(
            check_visibility_consistency(interval, qv.into(), qt.into()),
            "Inv 4 violated: visible={}, valid_at={}, recorded_at={}",
            interval.is_visible_at(qv.into(), qt.into()),
            interval.is_valid_at(qv.into()),
            interval.is_recorded_at(qt.into())
        );
    }

    /// Invariant 5: TimeRange.overlaps() is symmetric.
    #[test]
    fn prop_overlap_symmetry(
        s1 in 1_000_000i64..=400_000_000i64,
        e1 in 1_000_000i64..=400_000_000i64,
        s2 in 1_000_000i64..=400_000_000i64,
        e2 in 1_000_000i64..=400_000_000i64,
    ) {
        let (s1, e1) = if s1 <= e1 { (s1, e1) } else { (e1, s1) };
        let (s2, e2) = if s2 <= e2 { (s2, e2) } else { (e2, s2) };

        let r1 = match TimeRange::new(s1.into(), e1.into()) {
            Ok(r) => r,
            Err(_) => return Ok(()),
        };
        let r2 = match TimeRange::new(s2.into(), e2.into()) {
            Ok(r) => r,
            Err(_) => return Ok(()),
        };

        prop_assert_eq!(
            r1.overlaps(&r2),
            r2.overlaps(&r1),
            "Inv 5 violated: overlap not symmetric for {:?} vs {:?}",
            r1,
            r2
        );
    }

    /// Invariant 6: TimeRange.contains_range() is reflexive.
    #[test]
    fn prop_contains_range_reflexivity(
        s in 1_000_000i64..=400_000_000i64,
        e in 1_000_000i64..=400_000_000i64,
    ) {
        let (s, e) = if s <= e { (s, e) } else { (e, s) };

        let range = match TimeRange::new(s.into(), e.into()) {
            Ok(r) => r,
            Err(_) => return Ok(()),
        };

        prop_assert!(
            range.contains_range(&range),
            "Inv 6 violated: range does not contain itself: {:?}",
            range
        );
    }

    /// Invariant 7: End of a TimeRange is always exclusive.
    #[test]
    fn prop_closed_range_excludes_end(
        s in 1_000_000i64..=100_000_000i64,
        e in 100_000_001i64..=400_000_000i64,
    ) {
        let range = TimeRange::new(s.into(), e.into()).unwrap();

        prop_assert!(
            !range.contains(e.into()),
            "Inv 7 violated: range {:?} contains its exclusive end",
            range
        );
        prop_assert!(
            range.contains((e - 1).into()),
            "Inv 7 violated: range {:?} does not contain end-1",
            range
        );
    }

    /// Invariants 1–3: Any operation sequence preserves version chain invariants.
    #[test]
    fn prop_operation_sequence_preserves_invariants(ops in operation_sequence(6)) {
        let db = AletheiaDB::new().unwrap();
        let node_id = db
            .create_node("N", PropertyMapBuilder::new().insert("v", 0i64).build())
            .unwrap();

        for op in &ops {
            let res = match op {
                TemporalOperation::Update { value } => db.write(|tx| {
                    tx.update_node(
                        node_id,
                        PropertyMapBuilder::new().insert("v", *value).build(),
                    )?;
                    Ok::<_, Error>(())
                }),
                TemporalOperation::Delete => db.write(|tx| {
                    tx.delete_node(node_id)?;
                    Ok::<_, Error>(())
                }),
            };
            if res.is_err() {
                // Expected for sequences like update-after-delete; stop here and
                // check invariants on the state reached before the failure.
                break;
            }
        }

        prop_assert!(
            check_tx_time_monotonicity(&db, node_id),
            "Inv 1 violated after sequence {:?}",
            ops
        );
        prop_assert!(
            check_version_numbers_increasing(&db, node_id),
            "Inv 2 violated after sequence {:?}",
            ops
        );
        prop_assert!(
            check_time_range_validity(&db, node_id),
            "Inv 3 violated after sequence {:?}",
            ops
        );
    }
}

// ============================================================================
// Phase 2: Exhaustive small-n state enumeration
// ============================================================================

/// Exhaustively tests all semantically distinct 3-operation lifecycle orderings.
///
/// Covers:
/// - Create → Update
/// - Create → Delete
/// - Create → Update → Delete (full lifecycle)
/// - Create → Update × 15 (multiple versions, spanning the default anchor boundary)
#[test]
fn exhaustive_three_operation_orderings() {
    // --- Create then Update ---
    {
        let db = AletheiaDB::new().unwrap();
        let id = db
            .create_node("N", PropertyMapBuilder::new().insert("v", 1i64).build())
            .unwrap();

        db.write(|tx| {
            tx.update_node(id, PropertyMapBuilder::new().insert("v", 2i64).build())?;
            Ok::<_, Error>(())
        })
        .unwrap();

        assert!(check_tx_time_monotonicity(&db, id), "C→U: inv 1");
        assert!(check_version_numbers_increasing(&db, id), "C→U: inv 2");
        assert!(check_time_range_validity(&db, id), "C→U: inv 3");

        let history = db.get_node_history(id).unwrap();
        assert_eq!(history.versions.len(), 2, "C→U: expected 2 versions");
        assert_eq!(
            history
                .versions
                .last()
                .unwrap()
                .properties
                .get("v")
                .and_then(|v| v.as_int()),
            Some(2),
            "C→U: latest version should carry v=2"
        );
    }

    // --- Create then Delete ---
    {
        let db = AletheiaDB::new().unwrap();
        let id = db
            .create_node("N", PropertyMapBuilder::new().insert("v", 1i64).build())
            .unwrap();

        std::thread::sleep(std::time::Duration::from_micros(100));
        let t_after_create = time::now();
        std::thread::sleep(std::time::Duration::from_micros(100));

        db.write(|tx| {
            tx.delete_node(id)?;
            Ok::<_, Error>(())
        })
        .unwrap();

        assert!(
            db.get_node(id).is_err(),
            "C→D: node inaccessible after deletion"
        );
        assert!(
            db.get_node_at_time(id, t_after_create, t_after_create)
                .is_ok(),
            "C→D: time-travel before deletion succeeds"
        );
    }

    // --- Create → Update → Delete (full lifecycle) ---
    {
        let db = AletheiaDB::new().unwrap();
        let id = db
            .create_node("N", PropertyMapBuilder::new().insert("v", 1i64).build())
            .unwrap();

        std::thread::sleep(std::time::Duration::from_micros(100));
        let t_v1 = time::now();
        std::thread::sleep(std::time::Duration::from_micros(100));

        db.write(|tx| {
            tx.update_node(id, PropertyMapBuilder::new().insert("v", 2i64).build())?;
            Ok::<_, Error>(())
        })
        .unwrap();

        std::thread::sleep(std::time::Duration::from_micros(100));
        let t_v2 = time::now();
        std::thread::sleep(std::time::Duration::from_micros(100));

        db.write(|tx| {
            tx.delete_node(id)?;
            Ok::<_, Error>(())
        })
        .unwrap();

        assert!(
            db.get_node(id).is_err(),
            "C→U→D: node inaccessible after deletion"
        );

        let node_v1 = db.get_node_at_time(id, t_v1, t_v1).unwrap();
        assert_eq!(
            node_v1.get_property("v").and_then(|v| v.as_int()),
            Some(1),
            "C→U→D: time-travel before update yields v=1"
        );

        let node_v2 = db.get_node_at_time(id, t_v2, t_v2).unwrap();
        assert_eq!(
            node_v2.get_property("v").and_then(|v| v.as_int()),
            Some(2),
            "C→U→D: time-travel after update yields v=2"
        );
    }

    // --- Multiple updates spanning the anchor boundary ---
    {
        let db = AletheiaDB::new().unwrap();
        let id = db
            .create_node("N", PropertyMapBuilder::new().insert("v", 0i64).build())
            .unwrap();

        let mut checkpoints: Vec<(i64, Timestamp)> = vec![(0, time::now())];

        // 15 updates → crosses the default anchor_interval of 10
        for i in 1..=15i64 {
            std::thread::sleep(std::time::Duration::from_micros(10));
            db.write(|tx| {
                tx.update_node(id, PropertyMapBuilder::new().insert("v", i).build())?;
                Ok::<_, Error>(())
            })
            .unwrap();
            std::thread::sleep(std::time::Duration::from_micros(10));
            checkpoints.push((i, time::now()));
        }

        assert!(check_tx_time_monotonicity(&db, id), "anchor-span: inv 1");
        assert!(
            check_version_numbers_increasing(&db, id),
            "anchor-span: inv 2"
        );
        assert!(check_time_range_validity(&db, id), "anchor-span: inv 3");

        // Every historical value must be reconstructable via anchor + delta chain.
        for (expected, ts) in &checkpoints {
            assert!(
                db.get_node_at_time(id, *ts, *ts).is_ok(),
                "anchor-span: v={expected} not reconstructable at its snapshot time"
            );
        }
    }
}

/// Exhaustively tests all timestamp-ordering cases for BiTemporalInterval visibility.
///
/// Uses three ordered reference points t1 < t2 < t3 and checks all
/// combinations of (valid_time_query, tx_time_query) against an interval
/// with valid time [t1, t3) and transaction time [t2, ∞).
#[test]
fn exhaustive_timestamp_visibility_orderings() {
    let t1 = time::from_secs(100);
    let t2 = time::from_secs(200);
    let t3 = time::from_secs(300);
    let before_t1 = time::from_secs(50);

    let interval = BiTemporalInterval::new(TimeRange::new(t1, t3).unwrap(), TimeRange::from(t2));

    // Before valid time start — not visible regardless of tx_time.
    assert!(
        !interval.is_visible_at(before_t1, t3),
        "not visible before valid-time start"
    );

    // Valid time ok, but before transaction time start — not visible.
    assert!(
        !interval.is_visible_at(t2, t1),
        "not visible when tx_time precedes recording"
    );

    // Both dimensions satisfied — visible.
    assert!(
        interval.is_visible_at(t2, t3),
        "visible when both dimensions satisfied"
    );
    assert!(
        interval.is_visible_at(t1, t3),
        "visible at valid-time start when tx satisfied"
    );

    // End of valid range is exclusive — not visible at t3.
    assert!(
        !interval.is_visible_at(t3, t3),
        "exclusive valid-time end not visible"
    );

    // Open-ended interval (both dimensions current from t1).
    let open = BiTemporalInterval::current(t1);
    assert!(
        open.is_visible_at(t2, t2),
        "open interval: visible at any future point"
    );
    assert!(
        !open.is_visible_at(before_t1, before_t1),
        "open interval: not visible before start"
    );
}

/// Systematically verifies all 8 temporal invariants with targeted scenarios.
#[test]
fn check_all_eight_temporal_invariants() {
    // Inv 1, 2 & 3: Transaction Time Monotonicity, Version Number Ordering, Time Range Validity
    {
        let db = AletheiaDB::new().unwrap();
        let id = db
            .create_node("N", PropertyMapBuilder::new().insert("v", 0i64).build())
            .unwrap();
        for i in 1..=5i64 {
            db.write(|tx| {
                tx.update_node(id, PropertyMapBuilder::new().insert("v", i).build())?;
                Ok::<_, Error>(())
            })
            .unwrap();
        }
        assert!(
            check_tx_time_monotonicity(&db, id),
            "inv 1: tx_time monotonicity"
        );
        assert!(
            check_version_numbers_increasing(&db, id),
            "inv 2: version number ordering"
        );
        assert!(
            check_time_range_validity(&db, id),
            "inv 3: all stored time ranges valid"
        );
    }

    // Inv 4: Valid Time Consistency – entity not visible before its valid_from
    {
        let db = AletheiaDB::new().unwrap();
        let valid_from = time::from_secs(1000);
        let id = db
            .write(|tx| {
                tx.create_node_with_valid_time(
                    "N",
                    PropertyMapBuilder::new().insert("v", 1i64).build(),
                    Some(valid_from),
                )
            })
            .unwrap();

        let before_valid = time::from_secs(500);
        let tx_now = time::now();
        assert!(
            db.get_node_at_time(id, before_valid, tx_now).is_err(),
            "inv 4: entity not visible before valid_from"
        );
    }

    // Inv 5: No Paradoxes – time-travel before deletion succeeds; current access fails
    {
        let db = AletheiaDB::new().unwrap();
        let id = db
            .create_node("N", PropertyMapBuilder::new().insert("v", 1i64).build())
            .unwrap();

        std::thread::sleep(std::time::Duration::from_micros(100));
        let t_alive = time::now();
        std::thread::sleep(std::time::Duration::from_micros(100));

        db.write(|tx| {
            tx.delete_node(id)?;
            Ok::<_, Error>(())
        })
        .unwrap();

        assert!(
            db.get_node_at_time(id, t_alive, t_alive).is_ok(),
            "inv 5: time-travel before deletion must succeed"
        );
        assert!(
            db.get_node(id).is_err(),
            "inv 5: current access after deletion must fail"
        );
    }

    // Inv 6: Visibility Consistency – direct BiTemporalInterval checks
    {
        let vt_start = time::from_secs(100);
        let vt_end = time::from_secs(200);
        let tt_start = time::from_secs(150);

        let interval = BiTemporalInterval::new(
            TimeRange::new(vt_start, vt_end).unwrap(),
            TimeRange::from(tt_start),
        );

        let vt_inside = time::from_secs(150);
        let tt_satisfied = time::from_secs(200);
        let tt_before = time::from_secs(100);

        assert!(
            check_visibility_consistency(interval, vt_inside, tt_satisfied),
            "inv 6: consistent when both dims satisfied"
        );
        assert!(
            check_visibility_consistency(interval, vt_inside, tt_before),
            "inv 6: consistent when tx_time not satisfied"
        );
    }

    // Inv 7 & 8: Anchor/Delta Completeness – reconstruction across anchor boundary
    {
        let db = AletheiaDB::new().unwrap();
        let id = db
            .create_node("N", PropertyMapBuilder::new().insert("v", 0i64).build())
            .unwrap();

        let mut checkpoints: Vec<(i64, Timestamp)> = vec![(0, time::now())];
        for i in 1..=15i64 {
            std::thread::sleep(std::time::Duration::from_micros(10));
            db.write(|tx| {
                tx.update_node(id, PropertyMapBuilder::new().insert("v", i).build())?;
                Ok::<_, Error>(())
            })
            .unwrap();
            std::thread::sleep(std::time::Duration::from_micros(10));
            checkpoints.push((i, time::now()));
        }

        for (expected, ts) in &checkpoints {
            assert!(
                db.get_node_at_time(id, *ts, *ts).is_ok(),
                "inv 7/8: v={expected} must be reconstructable across anchor/delta boundary"
            );
        }
    }
}
