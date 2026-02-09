//! Verus proofs that mirror concrete invariants already checked in code/tests.
//!
//! Current links to runtime code/tests:
//! - Temporal prune predicates:
//!   `src/index/vector/temporal/mod.rs:keep_n_should_prune`
//!   `src/index/vector/temporal/mod.rs:keep_duration_should_prune`
//!   `src/index/vector/temporal/mod.rs:prune_snapshots`
//! - WAL ordering helper:
//!   `src/storage/wal/concurrent.rs:restore_global_lsn_order`
//!   `tests/loom_wal.rs:loom_wal_multi_stripe_drain_merge_preserves_global_lsn_order`
//!
//! These proofs are still "spec-level mirrors" (not direct proof of Rust bodies),
//! but they now encode the same policy decisions and invariants.

#![allow(unused_imports)]

use vstd::prelude::*;

verus! {

/// Closed interval over logical timestamps.
pub struct TimeRange {
    pub start: int,
    pub end: int,
}

impl TimeRange {
    pub open spec fn well_formed(self) -> bool {
        self.start <= self.end
    }
}

/// Core temporal safety predicate: no paradoxical range.
pub proof fn lemma_no_temporal_paradox(r: TimeRange)
    requires
        r.well_formed(),
    ensures
        !(r.end < r.start),
{
}

/// Monotonic update rule for transaction time.
pub proof fn lemma_tx_time_monotonic(prev: int, next: int)
    requires
        prev <= next,
    ensures
        next >= prev,
{
}

/// KeepN prune predicate from `keep_n_should_prune`.
/// Remove oldest `total - keep_n` entries.
pub open spec fn keep_n_should_prune(total_snapshots: int, keep_n: int, ordinal_from_oldest: int) -> bool {
    ordinal_from_oldest < total_snapshots - keep_n
}

/// Under KeepN with keep_n >= 1, newest entry (ordinal total-1) is retained.
pub proof fn lemma_keep_n_preserves_newest(total_snapshots: int, keep_n: int)
    requires
        total_snapshots > 0,
        keep_n >= 1,
        keep_n <= total_snapshots,
    ensures
        !keep_n_should_prune(total_snapshots, keep_n, total_snapshots - 1),
{
}

/// KeepDuration prune predicate from `keep_duration_should_prune`:
/// strictly older than cutoff.
pub open spec fn keep_duration_should_prune(candidate: int, cutoff: int) -> bool {
    candidate < cutoff
}

/// Candidate at cutoff boundary is retained (strict inequality).
pub proof fn lemma_keep_duration_cutoff_boundary(cutoff: int)
    ensures
        !keep_duration_should_prune(cutoff, cutoff),
{
}

/// If candidate is strictly older, it is prune-eligible.
pub proof fn lemma_keep_duration_strictly_older_prunes(candidate: int, cutoff: int)
    requires
        candidate < cutoff,
    ensures
        keep_duration_should_prune(candidate, cutoff),
{
}

/// KeepN prunes exactly the first `total-keep_n` ordinals.
pub proof fn lemma_keep_n_partition(total_snapshots: int, keep_n: int, ordinal: int)
    requires
        total_snapshots >= 0,
        keep_n >= 0,
        keep_n <= total_snapshots,
        0 <= ordinal,
        ordinal < total_snapshots,
    ensures
        keep_n_should_prune(total_snapshots, keep_n, ordinal)
            <==> ordinal < total_snapshots - keep_n,
{
}

/// Monotonicity: lowering keep_n never decreases prune set.
pub proof fn lemma_keep_n_monotone(total_snapshots: int, keep_a: int, keep_b: int, ordinal: int)
    requires
        total_snapshots >= 0,
        0 <= keep_a,
        keep_a <= keep_b,
        keep_b <= total_snapshots,
        0 <= ordinal,
        ordinal < total_snapshots,
        keep_n_should_prune(total_snapshots, keep_b, ordinal),
    ensures
        keep_n_should_prune(total_snapshots, keep_a, ordinal),
{
}

/// Abstract WAL merge-order predicate for three entries.
///
/// This mirrors the post-condition expected from `restore_global_lsn_order`
/// and from the Loom flush merge model.
pub open spec fn merged_lsn_ordered(a: int, b: int, c: int) -> bool {
    a <= b && b <= c
}

/// Adjacent order implies global order for 3-entry merge outputs.
pub proof fn lemma_adjacent_order_implies_merged_order(a: int, b: int, c: int)
    requires
        a <= b,
        b <= c,
    ensures
        merged_lsn_ordered(a, b, c),
{
}

} // verus!

fn main() {}
