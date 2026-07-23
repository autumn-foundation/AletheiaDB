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
        let wal = &self.primary.wal;

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
        if entries.len() > max_entries {
            entries.truncate(max_entries);
        }

        let primary_flushed_lsn = wal.max_flushed_lsn().map_or(0, |lsn| lsn.0);
        let primary_wallclock_micros = crate::core::temporal::time::now().wallclock();

        Ok(FetchOutcome::Entries {
            entries,
            primary_flushed_lsn,
            primary_wallclock_micros,
        })
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
