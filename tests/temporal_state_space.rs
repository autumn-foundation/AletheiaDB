//! State space exploration tests for temporal invariants (issue #153).
//!
//! Implements property-based testing and exhaustive small-n enumeration to
//! verify the 8 core temporal invariants of AletheiaDB beyond line coverage.
//!
//! # TDD Structure
//! - **RED phase** (this commit): invariant checkers are stubs that `todo!()`,
//!   causing every test to fail.
//! - **GREEN phase** (next commit): invariant checkers fully implemented;
//!   all tests pass.
//! - **REFACTOR phase**: code organisation and edge-case expansion.
//!
//! # Invariants under test
//! 1. **Transaction Time Monotonicity** – `tx_time(v_n) >= tx_time(v_{n-1})`
//! 2. **Version Number Ordering** – `version_number(v_n) > version_number(v_{n-1})`
//! 3. **Time Range Validity** – `start <= end` for every stored range
//! 4. **Visibility Consistency** – `visible_at(vt, tt) ⟺ valid_at(vt) ∧ recorded_at(tt)`
//! 5. **Overlap Symmetry** – `r1.overlaps(r2) == r2.overlaps(r1)`
//! 6. **Contains-Range Reflexivity** – `r.contains_range(r) == true`
//! 7. **Half-Open Interval Semantics** – end is exclusive
//! 8. **Temporal Isolation** – time-travel sees consistent snapshot values

use aletheiadb::core::id::NodeId;
use aletheiadb::core::property::PropertyMapBuilder;
use aletheiadb::{AletheiaDB, BiTemporalInterval, Error, TimeRange, Timestamp, WriteOps, time};
use proptest::prelude::*;

// ============================================================================
// Test infrastructure: TemporalOperation generators
// ============================================================================

/// A post-creation operation that can be applied to a node in sequence.
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
///
/// The sequence always starts with at least one Update, and may optionally end
/// with a Delete.  This mirrors the realistic node lifecycle.
fn operation_sequence(max_ops: usize) -> impl Strategy<Value = Vec<TemporalOperation>> {
    proptest::collection::vec(temporal_operation_strategy(), 1..=max_ops)
}

// ============================================================================
// Invariant checkers
// RED phase: each function calls `todo!()` so tests fail until implemented.
// ============================================================================

/// Invariant 1 – Transaction time is monotonically non-decreasing across versions.
fn check_tx_time_monotonicity(_db: &AletheiaDB, _node_id: NodeId) -> bool {
    todo!("RED – tx_time monotonicity checker not yet implemented")
}

/// Invariant 2 – Version numbers are strictly increasing.
fn check_version_numbers_increasing(_db: &AletheiaDB, _node_id: NodeId) -> bool {
    todo!("RED – version number ordering checker not yet implemented")
}

/// Invariant 3 – All stored time ranges satisfy start <= end.
fn check_time_range_validity(_db: &AletheiaDB, _node_id: NodeId) -> bool {
    todo!("RED – time range validity checker not yet implemented")
}

/// Invariant 4 – Visibility is consistent with individual dimension checks.
///
/// `visible_at(vt, tt)  ⟺  is_valid_at(vt) AND is_recorded_at(tt)`
fn check_visibility_consistency(
    _interval: BiTemporalInterval,
    _vt: Timestamp,
    _tt: Timestamp,
) -> bool {
    todo!("RED – visibility consistency checker not yet implemented")
}

// ============================================================================
// Phase 1: Property-based tests
// ============================================================================

proptest! {
    #![proptest_config(ProptestConfig::with_cases(200))]

    // Invariant 1 & 2 – after N updates, tx_time is monotone and versions are ordered
    #[test]
    fn prop_tx_time_monotonicity_after_updates(update_count in 1usize..=8) {
        let db = AletheiaDB::new().unwrap();

        let props = PropertyMapBuilder::new().insert("v", 0i64).build();
        let node_id = db.create_node("TestNode", props).unwrap();

        for i in 1..=(update_count as i64) {
            let p = PropertyMapBuilder::new().insert("v", i).build();
            db.write(|tx| {
                tx.update_node(node_id, p)?;
                Ok::<_, Error>(())
            })
            .unwrap();
        }

        prop_assert!(
            check_tx_time_monotonicity(&db, node_id),
            "Invariant 1 violated after {} updates",
            update_count
        );
        prop_assert!(
            check_version_numbers_increasing(&db, node_id),
            "Invariant 2 violated after {} updates",
            update_count
        );
    }

    // Invariant 3 – time ranges stored by the database are always valid
    #[test]
    fn prop_time_ranges_always_valid(update_count in 1usize..=6) {
        let db = AletheiaDB::new().unwrap();

        let props = PropertyMapBuilder::new().insert("v", 0i64).build();
        let node_id = db.create_node("N", props).unwrap();

        for i in 1..=(update_count as i64) {
            let p = PropertyMapBuilder::new().insert("v", i).build();
            db.write(|tx| {
                tx.update_node(node_id, p)?;
                Ok::<_, Error>(())
            })
            .unwrap();
        }

        prop_assert!(
            check_time_range_validity(&db, node_id),
            "Invariant 3 violated after {} updates",
            update_count
        );
    }

    // Invariant 4 – BiTemporalInterval visibility ⟺ both dimensions satisfied
    #[test]
    fn prop_visibility_consistency(
        s1 in 1_000_000i64..=400_000_000i64,
        e1 in 1_000_000i64..=400_000_000i64,
        s2 in 1_000_000i64..=400_000_000i64,
        e2 in 1_000_000i64..=400_000_000i64,
        qv in 1_000_000i64..=400_000_000i64,
        qt in 1_000_000i64..=400_000_000i64,
    ) {
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
            "Invariant 4 violated: visible={}, valid_at={}, recorded_at={}",
            interval.is_visible_at(qv.into(), qt.into()),
            interval.is_valid_at(qv.into()),
            interval.is_recorded_at(qt.into())
        );
    }

    // Invariant 5 – TimeRange.overlaps() is symmetric
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
            "Invariant 5 violated: overlap not symmetric for {:?} vs {:?}",
            r1,
            r2
        );
    }

    // Invariant 6 – TimeRange.contains_range() is reflexive
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
            "Invariant 6 violated: range does not contain itself: {:?}",
            range
        );
    }

    // Invariant 7 – Half-open interval: end is exclusive, end-1 is inclusive
    #[test]
    fn prop_closed_range_excludes_end(
        s in 1_000_000i64..=100_000_000i64,
        e in 100_000_001i64..=400_000_000i64,
    ) {
        let range = TimeRange::new(s.into(), e.into()).unwrap();

        prop_assert!(
            !range.contains(e.into()),
            "Invariant 7 violated: range {:?} contains its exclusive end {}",
            range, e
        );
        prop_assert!(
            range.contains((e - 1).into()),
            "Invariant 7 violated: range {:?} does not contain end-1={}",
            range,
            e - 1
        );
    }

    // Invariant 8 – Temporal isolation: operation sequences preserve invariants
    #[test]
    fn prop_operation_sequence_preserves_invariants(
        ops in operation_sequence(6)
    ) {
        let db = AletheiaDB::new().unwrap();

        let props = PropertyMapBuilder::new().insert("v", 0i64).build();
        let node_id = db.create_node("N", props).unwrap();

        for op in &ops {
            match op {
                TemporalOperation::Update { value } => {
                    let p = PropertyMapBuilder::new().insert("v", *value).build();
                    let _ = db.write(|tx| {
                        tx.update_node(node_id, p)?;
                        Ok::<_, Error>(())
                    });
                }
                TemporalOperation::Delete => {
                    let _ = db.write(|tx| {
                        tx.delete_node(node_id)?;
                        Ok::<_, Error>(())
                    });
                }
            }
        }

        // After any combination of operations, the version chain invariants must hold.
        prop_assert!(
            check_tx_time_monotonicity(&db, node_id),
            "Invariant 1 violated after sequence {:?}",
            ops
        );
        prop_assert!(
            check_version_numbers_increasing(&db, node_id),
            "Invariant 2 violated after sequence {:?}",
            ops
        );
        prop_assert!(
            check_time_range_validity(&db, node_id),
            "Invariant 3 violated after sequence {:?}",
            ops
        );
    }
}

// ============================================================================
// Phase 2: Exhaustive small-n state enumeration
// ============================================================================

/// Exhaustively tests all semantically distinct 3-operation lifecycle orderings.
///
/// For a single entity, the possible sequences that matter are:
///   Create → Update           (normal update)
///   Create → Delete           (immediate delete)
///   Create → Update → Delete  (full lifecycle)
///   Create → Update × N       (multiple versions, spanning anchor boundary)
#[test]
fn exhaustive_three_operation_orderings() {
    // --- Create then Update ---
    {
        let db = AletheiaDB::new().unwrap();
        let id = db
            .create_node(
                "N",
                PropertyMapBuilder::new().insert("v", 1i64).build(),
            )
            .unwrap();

        db.write(|tx| {
            tx.update_node(
                id,
                PropertyMapBuilder::new().insert("v", 2i64).build(),
            )?;
            Ok::<_, Error>(())
        })
        .unwrap();

        assert!(
            check_tx_time_monotonicity(&db, id),
            "Create→Update: tx_time monotonicity (inv 1)"
        );
        assert!(
            check_version_numbers_increasing(&db, id),
            "Create→Update: version numbers increasing (inv 2)"
        );
        assert!(
            check_time_range_validity(&db, id),
            "Create→Update: time range validity (inv 3)"
        );

        let history = db.get_node_history(id).unwrap();
        assert_eq!(history.versions.len(), 2, "Create→Update: expected 2 versions");
        assert_eq!(
            history
                .versions
                .last()
                .unwrap()
                .properties
                .get("v")
                .and_then(|v| v.as_int()),
            Some(2),
            "Create→Update: latest version should carry v=2"
        );
    }

    // --- Create then Delete ---
    {
        let db = AletheiaDB::new().unwrap();
        let id = db
            .create_node(
                "N",
                PropertyMapBuilder::new().insert("v", 1i64).build(),
            )
            .unwrap();

        std::thread::sleep(std::time::Duration::from_micros(100));
        let t_after_create = time::now();
        std::thread::sleep(std::time::Duration::from_micros(100));

        db.write(|tx| {
            tx.delete_node(id)?;
            Ok::<_, Error>(())
        })
        .unwrap();

        // Current query must fail after deletion.
        assert!(
            db.get_node(id).is_err(),
            "Create→Delete: node should not be accessible after deletion"
        );
        // Time-travel to before deletion must succeed.
        assert!(
            db.get_node_at_time(id, t_after_create, t_after_create).is_ok(),
            "Create→Delete: time-travel before deletion should succeed"
        );
    }

    // --- Create → Update → Delete (full lifecycle) ---
    {
        let db = AletheiaDB::new().unwrap();
        let id = db
            .create_node(
                "N",
                PropertyMapBuilder::new().insert("v", 1i64).build(),
            )
            .unwrap();

        std::thread::sleep(std::time::Duration::from_micros(100));
        let t_v1 = time::now();
        std::thread::sleep(std::time::Duration::from_micros(100));

        db.write(|tx| {
            tx.update_node(
                id,
                PropertyMapBuilder::new().insert("v", 2i64).build(),
            )?;
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

        // Current query fails.
        assert!(
            db.get_node(id).is_err(),
            "C→U→D: node should not exist after deletion"
        );
        // Time-travel to after create (before update) → v=1.
        let node_v1 = db.get_node_at_time(id, t_v1, t_v1).unwrap();
        assert_eq!(
            node_v1.get_property("v").and_then(|v| v.as_int()),
            Some(1),
            "C→U→D: time-travel before update should give v=1"
        );
        // Time-travel to after update (before delete) → v=2.
        let node_v2 = db.get_node_at_time(id, t_v2, t_v2).unwrap();
        assert_eq!(
            node_v2.get_property("v").and_then(|v| v.as_int()),
            Some(2),
            "C→U→D: time-travel after update should give v=2"
        );
    }

    // --- Multiple updates spanning the anchor boundary ---
    {
        let db = AletheiaDB::new().unwrap();
        let id = db
            .create_node(
                "N",
                PropertyMapBuilder::new().insert("v", 0i64).build(),
            )
            .unwrap();

        let mut checkpoints: Vec<(i64, Timestamp)> = vec![(0, time::now())];

        // 15 updates → crosses the default anchor_interval of 10
        for i in 1..=15i64 {
            std::thread::sleep(std::time::Duration::from_micros(10));
            let p = PropertyMapBuilder::new().insert("v", i).build();
            db.write(|tx| {
                tx.update_node(id, p)?;
                Ok::<_, Error>(())
            })
            .unwrap();
            std::thread::sleep(std::time::Duration::from_micros(10));
            checkpoints.push((i, time::now()));
        }

        assert!(
            check_tx_time_monotonicity(&db, id),
            "Multi-update: tx_time monotonicity across anchor boundary (inv 1)"
        );
        assert!(
            check_version_numbers_increasing(&db, id),
            "Multi-update: version numbers increasing (inv 2)"
        );
        assert!(
            check_time_range_validity(&db, id),
            "Multi-update: time range validity (inv 3)"
        );

        // Verify reconstruction of every version via anchor + delta chain.
        for (expected, ts) in &checkpoints {
            let node = db.get_node_at_time(id, *ts, *ts);
            assert!(
                node.is_ok(),
                "Multi-update: version v={} not reconstructable at its snapshot time",
                expected
            );
        }
    }
}

/// Exhaustively tests all timestamp-ordering cases for BiTemporalInterval visibility.
#[test]
fn exhaustive_timestamp_visibility_orderings() {
    // Reference timestamps: t1 < t2 < t3 (seconds since epoch)
    let t1 = time::from_secs(100);
    let t2 = time::from_secs(200);
    let t3 = time::from_secs(300);
    let before_t1 = time::from_secs(50);

    // Interval valid from t1..t3, recorded from t2..current
    let interval = BiTemporalInterval::new(
        TimeRange::new(t1, t3).unwrap(),
        TimeRange::from(t2),
    );

    // Before valid time start → not visible regardless of tx_time
    assert!(
        !interval.is_visible_at(before_t1, t3),
        "Not visible before valid time start"
    );
    // Valid time ok, but before transaction time start → not visible
    assert!(
        !interval.is_visible_at(t2, t1),
        "Not visible when tx_time before recording start"
    );
    // Both dimensions satisfied → visible
    assert!(
        interval.is_visible_at(t2, t3),
        "Visible when both dimensions satisfied"
    );
    assert!(
        interval.is_visible_at(t1, t3),
        "Visible at valid time start when tx_time satisfied"
    );
    // End of valid range is exclusive → not visible
    assert!(
        !interval.is_visible_at(t3, t3),
        "Not visible at exclusive valid-time end"
    );

    // Open interval (current in both dimensions)
    let open = BiTemporalInterval::current(t1);
    assert!(
        open.is_visible_at(t2, t2),
        "Open interval: visible at any future point"
    );
    assert!(
        !open.is_visible_at(before_t1, before_t1),
        "Open interval: not visible before start"
    );
}

/// Systematically verifies all 8 temporal invariants with concrete scenarios.
#[test]
fn check_all_eight_temporal_invariants() {
    // Inv 1: Transaction Time Monotonicity
    {
        let db = AletheiaDB::new().unwrap();
        let id = db
            .create_node(
                "N",
                PropertyMapBuilder::new().insert("v", 0i64).build(),
            )
            .unwrap();
        for i in 1..=5i64 {
            db.write(|tx| {
                tx.update_node(
                    id,
                    PropertyMapBuilder::new().insert("v", i).build(),
                )?;
                Ok::<_, Error>(())
            })
            .unwrap();
        }
        assert!(
            check_tx_time_monotonicity(&db, id),
            "Inv 1: tx_time monotonicity"
        );
    }

    // Inv 2: Version Number Ordering
    {
        let db = AletheiaDB::new().unwrap();
        let id = db
            .create_node(
                "N",
                PropertyMapBuilder::new().insert("v", 0i64).build(),
            )
            .unwrap();
        for i in 1..=5i64 {
            db.write(|tx| {
                tx.update_node(
                    id,
                    PropertyMapBuilder::new().insert("v", i).build(),
                )?;
                Ok::<_, Error>(())
            })
            .unwrap();
        }
        assert!(
            check_version_numbers_increasing(&db, id),
            "Inv 2: version number ordering"
        );
    }

    // Inv 3: Time Range Validity
    {
        let db = AletheiaDB::new().unwrap();
        let id = db
            .create_node(
                "N",
                PropertyMapBuilder::new().insert("v", 0i64).build(),
            )
            .unwrap();
        for i in 1..=5i64 {
            db.write(|tx| {
                tx.update_node(
                    id,
                    PropertyMapBuilder::new().insert("v", i).build(),
                )?;
                Ok::<_, Error>(())
            })
            .unwrap();
        }
        assert!(
            check_time_range_validity(&db, id),
            "Inv 3: all stored time ranges valid"
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
            "Inv 4: entity not visible before valid_from"
        );
    }

    // Inv 5: No Paradoxes – time-travel before deletion succeeds; current access fails
    {
        let db = AletheiaDB::new().unwrap();
        let id = db
            .create_node(
                "N",
                PropertyMapBuilder::new().insert("v", 1i64).build(),
            )
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
            "Inv 5: time-travel before deletion must succeed"
        );
        assert!(
            db.get_node(id).is_err(),
            "Inv 5: current access after deletion must fail"
        );
    }

    // Inv 6: Visibility Consistency (direct BiTemporalInterval checks)
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
            "Inv 6: visibility consistent when both dims satisfied"
        );
        assert!(
            check_visibility_consistency(interval, vt_inside, tt_before),
            "Inv 6: visibility consistent when tx_time not satisfied"
        );
    }

    // Inv 7 & 8: Anchor/Delta Completeness – reconstruction across anchor boundary
    {
        let db = AletheiaDB::new().unwrap();
        let id = db
            .create_node(
                "N",
                PropertyMapBuilder::new().insert("v", 0i64).build(),
            )
            .unwrap();

        let mut checkpoints: Vec<(i64, Timestamp)> = vec![(0, time::now())];
        for i in 1..=15i64 {
            std::thread::sleep(std::time::Duration::from_micros(10));
            db.write(|tx| {
                tx.update_node(
                    id,
                    PropertyMapBuilder::new().insert("v", i).build(),
                )?;
                Ok::<_, Error>(())
            })
            .unwrap();
            std::thread::sleep(std::time::Duration::from_micros(10));
            checkpoints.push((i, time::now()));
        }

        for (expected, ts) in &checkpoints {
            let node = db.get_node_at_time(id, *ts, *ts);
            assert!(
                node.is_ok(),
                "Inv 7/8: version v={} should be reconstructable across anchor/delta boundary",
                expected
            );
        }
    }
}
