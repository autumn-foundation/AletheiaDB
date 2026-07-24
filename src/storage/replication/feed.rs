//! Primary-side WAL feed (Issue #3355, Slice B).
//!
//! [`ReplicationFeed`] is constructed from a primary [`AletheiaDB`] and serves
//! durable (flushed-to-segment) WAL entries from a requested LSN, or a
//! structured [`FetchOutcome::ResyncRequired`] when that LSN has fallen off
//! the primary's retained segments. It also exposes a point-in-time snapshot
//! (delegating to the existing `.albk` backup artifact) for initial replica
//! bootstrap.
//!
//! Used in-process today by [`super::source::InProcessSource`]; the same feed
//! is the seam Slice C's TCP server wraps to serve remote replicas without any
//! change here.

use std::path::Path;
use std::sync::Arc;

use crate::core::error::Result;
use crate::db::AletheiaDB;
use crate::storage::backup::BackupSummary;
use crate::storage::wal::concurrent_system::ConcurrentWalSystem;
use crate::storage::wal::{LSN, WalEntry};

/// Result of a single [`ReplicationFeed::fetch_entries`] call.
#[derive(Debug, Clone)]
pub enum FetchOutcome {
    /// A (possibly empty) batch of durable WAL entries, plus the primary's
    /// current flushed-LSN and wallclock — both used for replica lag
    /// observability, not correctness.
    Entries {
        /// Entries with `lsn >= from_lsn`, in LSN order, truncated to at most
        /// the requested `max_entries`. May legitimately be empty (the
        /// primary is caught up, or the WAL directory has no segments yet).
        entries: Vec<WalEntry>,
        /// The primary's best-known max LSN actually written to disk at the
        /// time of this fetch (see
        /// [`crate::storage::wal::concurrent_system::ConcurrentWalSystem::max_flushed_lsn`]).
        primary_flushed_lsn: u64,
        /// The primary's wallclock (microseconds since epoch) at the time of
        /// this fetch, used to derive an approximate replication-lag metric.
        primary_wallclock_micros: i64,
    },
    /// The requested `from_lsn` has fallen behind the primary's minimum
    /// available (retained) WAL segment LSN -- the entries the replica needs
    /// have been truncated away (e.g. by tiered-storage `truncate_to_lsn` or a
    /// retention sweep). The replica must NOT skip ahead; it should surface
    /// this state and stop applying until re-bootstrapped from a fresh
    /// snapshot.
    ResyncRequired {
        /// The primary's current minimum available (retained) LSN.
        min_available_lsn: u64,
    },
}

/// Primary-side WAL feed over an [`AletheiaDB`]'s durable state.
pub struct ReplicationFeed {
    primary: Arc<AletheiaDB>,
}

impl ReplicationFeed {
    /// Construct a feed over `primary`.
    #[must_use]
    pub fn new(primary: Arc<AletheiaDB>) -> Self {
        Self { primary }
    }

    /// Fetch durable WAL entries starting at `from_lsn` (inclusive), capped at
    /// `max_entries`.
    ///
    /// Reads only entries the primary has actually flushed to segment files on
    /// disk (`ConcurrentWalSystem::read_from`) -- an un-durable, ring-buffer-only
    /// entry is never shipped, so a replica can never apply data the primary
    /// itself could lose on crash.
    ///
    /// Returns [`FetchOutcome::ResyncRequired`] instead of a (possibly gapped)
    /// entry batch when `from_lsn` lies below the primary's minimum retained
    /// segment LSN -- i.e. the primary has already discarded WAL history the
    /// replica still needs.
    ///
    /// # Errors
    ///
    /// Propagates any I/O/decode error from reading the WAL segment directory.
    pub fn fetch_entries(&self, from_lsn: u64, max_entries: usize) -> Result<FetchOutcome> {
        fetch_entries_from_wal(&self.primary.wal, from_lsn, max_entries)
    }

    /// Take a consistent point-in-time snapshot of the primary at `path`,
    /// suitable for bootstrapping a fresh replica. Delegates to
    /// [`AletheiaDB::backup`]; the returned summary's `source_lsn` is the
    /// replica's replay start coordinate (see [`BackupSummary::source_lsn`]).
    ///
    /// # Errors
    ///
    /// Propagates any error from [`AletheiaDB::backup`].
    pub fn snapshot_to(&self, path: &Path) -> Result<BackupSummary> {
        self.primary.backup(path)
    }
}

/// The durable-truncation-aware `fetch_entries` logic, factored out over a
/// bare `&ConcurrentWalSystem` (Issue #3355, Slice C).
///
/// [`ReplicationFeed::fetch_entries`] is a thin wrapper over this. It exists
/// standalone so [`super::tcp`]'s config-auto-wired listen server (which is
/// spawned from inside `AletheiaDB::with_unified_config` -- before the
/// database being constructed can be wrapped in the `Arc<AletheiaDB>` a full
/// [`ReplicationFeed`] requires) can serve `FetchEntries` from just the WAL
/// handle it already owns an `Arc` to, with no self-referential-`Arc`
/// trickery. Only snapshot bootstrap (`FetchSnapshot`, which needs
/// [`AletheiaDB::backup`]) requires the fuller [`ReplicationFeed`]; see
/// `crate::storage::replication::tcp`'s module docs for the exact contract
/// this splits.
pub(crate) fn fetch_entries_from_wal(
    wal: &ConcurrentWalSystem,
    from_lsn: u64,
    max_entries: usize,
) -> Result<FetchOutcome> {
    // Resync detection: `min_available_lsn() == None` means no segment
    // carries metadata yet (a fresh/empty WAL directory) -- nothing has
    // ever been truncated, so every requested LSN is still satisfiable.
    // `min_available_lsn.0 == 1` means retention hasn't removed anything
    // (the very first segment is still present), so a caller starting
    // from LSN 1 (a fresh replica) is never spuriously told to resync.
    if let Some(min_lsn) = wal.min_available_lsn()
        && from_lsn < min_lsn.0
        && min_lsn.0 > 1
    {
        return Ok(FetchOutcome::ResyncRequired {
            min_available_lsn: min_lsn.0,
        });
    }

    let start = LSN(from_lsn.max(1));
    let mut entries = wal.read_from(start)?;

    // The durable stream is not guaranteed dense at the read frontier: the
    // striped ring buffers flush independently, so later LSNs can be on disk
    // while earlier ones (even of the same commit frame) are still buffered
    // in another stripe, and serialize failures leave permanent allocator
    // holes (`concurrent.rs`). Shipping past a gap would let a replica
    // silently skip committed transactions -- or tear a frame whose
    // `BeginTx` sits inside the gap. Withhold everything at/after the first
    // discontinuity: a transient gap heals on a later poll once the missing
    // stripe flushes; a permanent hole surfaces as growing, observable lag
    // (`entries_behind`) instead of silent data loss.
    truncate_at_first_gap(&mut entries, start.0);
    if entries.len() > max_entries {
        entries.truncate(max_entries);
    }

    // Re-check retention AFTER the read: `truncate_to_lsn` racing between
    // the check above and `read_from` could have removed `from_lsn`'s
    // segment mid-read, making the (now possibly empty or gapped) batch
    // indistinguishable from "caught up". Prefer the explicit resync signal.
    if let Some(min_lsn) = wal.min_available_lsn()
        && from_lsn < min_lsn.0
        && min_lsn.0 > 1
    {
        return Ok(FetchOutcome::ResyncRequired {
            min_available_lsn: min_lsn.0,
        });
    }

    let primary_flushed_lsn = wal.max_flushed_lsn().map_or(0, |lsn| lsn.0);
    let primary_wallclock_micros = crate::core::temporal::time::now().wallclock();

    Ok(FetchOutcome::Entries {
        entries,
        primary_flushed_lsn,
        primary_wallclock_micros,
    })
}

/// Keep only the longest dense prefix of `entries` starting exactly at
/// `from_lsn` (entries are already LSN-sorted by `read_from`). An empty
/// result means the entry at `from_lsn` is not durable yet -- the caller
/// reports "no new entries" and the replica retries, rather than being
/// handed a batch with a silent hole.
fn truncate_at_first_gap(entries: &mut Vec<crate::storage::wal::WalEntry>, from_lsn: u64) {
    let mut expected = from_lsn;
    let mut keep = 0usize;
    for entry in entries.iter() {
        if entry.lsn.0 != expected {
            break;
        }
        keep += 1;
        expected = expected.wrapping_add(1);
    }
    entries.truncate(keep);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::hlc::HybridTimestamp;
    use crate::storage::wal::WalOperation;

    fn entry(lsn: u64) -> WalEntry {
        WalEntry {
            lsn: LSN(lsn),
            timestamp: HybridTimestamp::new_unchecked(1, 0),
            operation: WalOperation::BeginTx { tx_id: 1 },
            checksum: 0,
            framed: true,
            segment_version: None,
        }
    }

    fn lsns(entries: &[WalEntry]) -> Vec<u64> {
        entries.iter().map(|e| e.lsn.0).collect()
    }

    /// Review finding #1 (Issue #3355): a batch must never ship past an LSN
    /// discontinuity -- later-durable stripes and permanent allocator holes
    /// would otherwise let a replica silently skip committed transactions.
    #[test]
    fn dense_batch_is_untouched() {
        let mut e = vec![entry(5), entry(6), entry(7)];
        truncate_at_first_gap(&mut e, 5);
        assert_eq!(lsns(&e), vec![5, 6, 7]);
    }

    #[test]
    fn head_gap_withholds_everything() {
        // Requested LSN 5 but the first durable entry is 8 (5-7 still in an
        // unflushed stripe): withhold the whole batch.
        let mut e = vec![entry(8), entry(9)];
        truncate_at_first_gap(&mut e, 5);
        assert!(e.is_empty());
    }

    #[test]
    fn mid_gap_truncates_at_discontinuity() {
        // 5,6 durable; 7 missing; 8,9 durable: only the dense prefix ships.
        let mut e = vec![entry(5), entry(6), entry(8), entry(9)];
        truncate_at_first_gap(&mut e, 5);
        assert_eq!(lsns(&e), vec![5, 6]);
    }

    #[test]
    fn empty_batch_is_fine() {
        let mut e: Vec<WalEntry> = Vec::new();
        truncate_at_first_gap(&mut e, 1);
        assert!(e.is_empty());
    }
}
