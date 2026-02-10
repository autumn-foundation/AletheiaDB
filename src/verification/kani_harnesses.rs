//! Initial Kani harnesses for bounded model checking.
//!
//! These are intentionally small and focus on high-value invariants that are easy
//! to model-check exhaustively.

use crate::core::id::NodeId;
use crate::index::vector::temporal::snapshot::SnapshotMetadata;
use crate::storage::wal::LSN;
use crate::storage::wal::WalOperation;
use crate::storage::wal::concurrent::restore_global_lsn_order;
use crate::storage::wal::concurrent::{ConcurrentWal, ConcurrentWalConfig};
use crate::storage::wal::lsn_allocator::LsnAllocator;
use crate::storage::wal::ring_buffer::PendingEntry;

/// Prove single-step monotonicity of the LSN allocator.
#[kani::proof]
fn kani_lsn_allocator_single_step_monotonic() {
    let alloc = LsnAllocator::starting_at(LSN(100));
    let a = alloc.allocate();
    let b = alloc.allocate();
    assert!(a.0 < b.0);
}

/// Prove batch allocation range size and ordering for bounded counts.
#[kani::proof]
fn kani_lsn_allocator_batch_range_size() {
    let alloc = LsnAllocator::starting_at(LSN(1));
    let count: u64 = kani::any();
    kani::assume(count > 0);
    kani::assume(count <= 64);

    let (first, last) = alloc.allocate_batch(count);
    assert_eq!(last.0 - first.0 + 1, count);

    // Next allocation must be strictly after the batch range.
    let next = alloc.allocate();
    assert!(next.0 > last.0);
}

/// Prove ordering helper returns non-decreasing LSN order.
#[kani::proof]
fn kani_restore_global_lsn_order_sorts() {
    let a: u64 = kani::any();
    let b: u64 = kani::any();
    let c: u64 = kani::any();

    // Keep data payloads tiny; only LSN ordering matters for this harness.
    let mut entries = vec![
        PendingEntry::new_async(LSN(a), vec![0]),
        PendingEntry::new_async(LSN(b), vec![0]),
        PendingEntry::new_async(LSN(c), vec![0]),
    ];

    restore_global_lsn_order(&mut entries);

    assert!(entries[0].lsn.0 <= entries[1].lsn.0);
    assert!(entries[1].lsn.0 <= entries[2].lsn.0);
}

/// Bounded WAL lifecycle model over real `ConcurrentWal` APIs:
/// open -> appending -> draining(flushing) -> closed.
#[kani::proof]
fn kani_wal_bounded_state_machine_transitions() {
    let config = ConcurrentWalConfig::new("target/kani-wal")
        .with_num_stripes(1)
        .with_stripe_capacity(8);
    let wal = match ConcurrentWal::new(config) {
        Ok(wal) => wal,
        Err(_) => unreachable!("bounded harness requires valid WAL construction"),
    };

    let steps: u8 = kani::any();
    kani::assume(steps <= 6);

    let mut successful_appends = 0_u64;
    let mut pending_entries = 0_u64;
    let mut last_success_lsn = 0_u64;
    let mut saw_close = false;

    for _ in 0..steps {
        let action: u8 = kani::any();
        kani::assume(action <= 3);

        match action {
            // append
            0 => {
                let was_closed = wal.is_closed();
                let prev_current = wal.current_lsn().0;
                let op = WalOperation::Checkpoint {
                    lsn: LSN(0),
                    timestamp: 0_i64.into(),
                };
                let result = wal.append_async(op);

                // LSN allocation happens before ring-buffer append attempt.
                assert_eq!(wal.current_lsn().0, prev_current + 1);

                if was_closed {
                    assert!(result.is_err());
                } else {
                    let lsn = match result {
                        Ok(lsn) => lsn,
                        Err(_) => unreachable!("append should succeed while open in bounded model"),
                    };
                    assert!(lsn.0 > last_success_lsn);
                    last_success_lsn = lsn.0;
                    successful_appends += 1;
                    pending_entries += 1;
                }
            }
            // drain/flush
            1 => {
                let drained = wal.drain_all();
                assert!((drained.len() as u64) <= pending_entries);
                pending_entries -= drained.len() as u64;

                for i in 1..drained.len() {
                    assert!(drained[i - 1].lsn.0 <= drained[i].lsn.0);
                }
            }
            // close
            2 => {
                wal.close();
                saw_close = true;
                assert!(wal.is_closed());
            }
            // no-op
            3 => {}
            _ => unreachable!("action constrained to 0..=3"),
        }
    }

    assert_eq!(wal.total_appends(), successful_appends);
    if saw_close {
        assert!(wal.is_closed());
    }
}

/// Bounded metadata state-machine for snapshot rollover/reset invariants.
#[kani::proof]
fn kani_snapshot_metadata_rollover_bounded() {
    let mut meta = SnapshotMetadata::new(100_i64.into());
    let mut expected_total_snapshots = 0_usize;
    let mut expected_snapshots_since_full = 0_usize;

    let steps: u8 = kani::any();
    kani::assume(steps <= 8);

    for _ in 0..steps {
        let action: u8 = kani::any();
        kani::assume(action <= 2);

        match action {
            // record_transaction
            0 => {
                let before = meta.transactions_since_snapshot;
                meta.record_transaction();
                assert_eq!(meta.transactions_since_snapshot, before + 1);
            }
            // record_change
            1 => {
                let id = match NodeId::new(1) {
                    Ok(id) => id,
                    Err(_) => unreachable!("NodeId(1) must be valid"),
                };
                meta.record_change(id);
                assert!(meta.vectors_changed_since_snapshot.contains(&id));
                assert!(meta.changes_accumulated.contains(&id));
            }
            // reset with bounded timestamp + full/delta choice
            2 => {
                let ts: i64 = kani::any();
                kani::assume(ts >= 0);
                kani::assume(ts <= 1_000_000);
                let is_full: bool = kani::any();

                meta.reset(ts.into(), is_full);
                expected_total_snapshots += 1;
                assert_eq!(meta.total_snapshots, expected_total_snapshots);
                assert_eq!(meta.transactions_since_snapshot, 0);
                assert!(meta.vectors_changed_since_snapshot.is_empty());

                if is_full {
                    expected_snapshots_since_full = 0;
                    assert_eq!(meta.snapshots_since_full, 0);
                    assert_eq!(meta.last_full_snapshot_time, ts.into());
                    assert!(meta.changes_accumulated.is_empty());
                } else {
                    expected_snapshots_since_full += 1;
                    assert_eq!(meta.snapshots_since_full, expected_snapshots_since_full);
                }
            }
            _ => unreachable!("action constrained to 0..=2"),
        }
    }
}

/// Mixed allocate/allocate_batch bounded sequence preserves strict monotonicity.
#[kani::proof]
fn kani_lsn_allocator_bounded_sequence_monotonic() {
    let alloc = LsnAllocator::starting_at(LSN(10));
    let steps: u8 = kani::any();
    kani::assume(steps <= 8);

    let mut last_issued = 9_u64;

    for _ in 0..steps {
        let action: u8 = kani::any();
        kani::assume(action <= 1);

        if action == 0 {
            let lsn = alloc.allocate();
            assert!(lsn.0 > last_issued);
            last_issued = lsn.0;
        } else {
            let count: u64 = kani::any();
            kani::assume(count >= 1);
            kani::assume(count <= 4);

            let (first, last) = alloc.allocate_batch(count);
            assert!(first.0 > last_issued);
            assert!(last.0 >= first.0);
            assert_eq!(last.0 - first.0 + 1, count);
            last_issued = last.0;
        }
    }

    assert_eq!(alloc.current().0, last_issued + 1);
}

/// Single allocation followed by batch must never overlap.
#[kani::proof]
fn kani_lsn_single_then_batch_non_overlapping() {
    let alloc = LsnAllocator::starting_at(LSN(1));
    let first = alloc.allocate();

    let count: u64 = kani::any();
    kani::assume((1..=8).contains(&count));

    let (batch_first, batch_last) = alloc.allocate_batch(count);
    assert!(batch_first.0 > first.0);
    assert!(batch_last.0 >= batch_first.0);
}

/// Batch allocation followed by single allocation must never overlap.
#[kani::proof]
fn kani_lsn_batch_then_single_non_overlapping() {
    let alloc = LsnAllocator::starting_at(LSN(1));
    let count: u64 = kani::any();
    kani::assume((1..=8).contains(&count));

    let (_first, last) = alloc.allocate_batch(count);
    let next = alloc.allocate();
    assert!(next.0 > last.0);
}

/// Close is sticky and append attempts after close fail while LSN still advances.
#[kani::proof]
fn kani_wal_close_is_sticky_and_append_fails_after_close() {
    let config = ConcurrentWalConfig::new("target/kani-wal-close")
        .with_num_stripes(1)
        .with_stripe_capacity(8);
    let wal = match ConcurrentWal::new(config) {
        Ok(wal) => wal,
        Err(_) => unreachable!("bounded harness requires valid WAL construction"),
    };

    wal.close();
    assert!(wal.is_closed());

    let before = wal.current_lsn().0;
    let result = wal.append_async(WalOperation::Checkpoint {
        lsn: LSN(0),
        timestamp: 0_i64.into(),
    });
    assert!(result.is_err());
    assert!(wal.is_closed());
    assert_eq!(wal.current_lsn().0, before + 1);
}

/// Full reset clears accumulated changes after arbitrary bounded change history.
#[kani::proof]
fn kani_snapshot_full_reset_clears_accumulated_changes() {
    let mut meta = SnapshotMetadata::new(0_i64.into());

    let n: u8 = kani::any();
    kani::assume(n <= 6);
    for _ in 0..n {
        let id = match NodeId::new(1) {
            Ok(id) => id,
            Err(_) => unreachable!("NodeId(1) must be valid"),
        };
        meta.record_change(id);
    }

    meta.reset(10_i64.into(), true);
    assert!(meta.changes_accumulated.is_empty());
    assert_eq!(meta.snapshots_since_full, 0);
    assert_eq!(meta.total_snapshots, 1);
}
