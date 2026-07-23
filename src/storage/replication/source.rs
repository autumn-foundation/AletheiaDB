//! Replica-side transport abstraction (Issue #3355, Slice B).
//!
//! [`ReplicationSource`] is deliberately transport-agnostic: [`InProcessSource`]
//! wraps another in-process [`AletheiaDB`] (the deterministic transport used by
//! tests and, later, chaos harnesses), while Slice C adds a TCP client
//! implementation with the exact same trait -- [`applier::ReplicaApplierHandle`]
//! and everything above it never needs to change.
//!
//! [`applier::ReplicaApplierHandle`]: super::applier::ReplicaApplierHandle

use std::path::Path;
use std::sync::Arc;

use crate::core::error::Result;
use crate::db::AletheiaDB;

use super::feed::{FetchOutcome, ReplicationFeed};

/// A pull source of primary WAL entries + snapshots that a replica applier
/// drives.
///
/// `Send + 'static` because a `Box<dyn ReplicationSource>` is owned by the
/// background applier thread ([`super::applier::ReplicaApplierHandle`]).
pub trait ReplicationSource: Send + 'static {
    /// Fetch durable WAL entries starting at `from_lsn` (inclusive), capped at
    /// `max_entries`. See [`ReplicationFeed::fetch_entries`] for the exact
    /// contract this must honor (durable-only, resync-on-truncation).
    ///
    /// # Errors
    ///
    /// Any transport/decode failure. The applier treats an error as transient
    /// (retries with backoff), distinct from a structured
    /// [`FetchOutcome::ResyncRequired`].
    fn fetch_entries(&mut self, from_lsn: u64, max_entries: usize) -> Result<FetchOutcome>;

    /// Fetch a full point-in-time snapshot of the primary into `dest`,
    /// returning the snapshot's `source_lsn` -- the LSN a replica bootstrapped
    /// from this snapshot should resume streaming from (inclusive).
    ///
    /// # Errors
    ///
    /// Any transport/I/O failure while producing or transferring the
    /// snapshot artifact.
    fn fetch_snapshot(&mut self, dest: &Path) -> Result<u64>;
}

/// In-process [`ReplicationSource`]: wraps another [`AletheiaDB`] (the
/// "primary") via a [`ReplicationFeed`]. This is the deterministic transport
/// used by tests and chaos harnesses; Slice C's TCP client implements the same
/// trait for real deployments.
pub struct InProcessSource {
    feed: ReplicationFeed,
}

impl InProcessSource {
    /// Wrap `primary` as an in-process replication source.
    #[must_use]
    pub fn new(primary: Arc<AletheiaDB>) -> Self {
        Self {
            feed: ReplicationFeed::new(primary),
        }
    }
}

impl ReplicationSource for InProcessSource {
    fn fetch_entries(&mut self, from_lsn: u64, max_entries: usize) -> Result<FetchOutcome> {
        self.feed.fetch_entries(from_lsn, max_entries)
    }

    fn fetch_snapshot(&mut self, dest: &Path) -> Result<u64> {
        Ok(self.feed.snapshot_to(dest)?.source_lsn)
    }
}
