//! Node replication role: writable primary vs. read-only replica.
//!
//! This module is the always-compiled enforcement core for asynchronous
//! replication (Issue #3355, Slice A). It defines [`NodeRole`], the atomic
//! role cell carried by every [`AletheiaDB`], and the shared rejection used
//! at both write-transaction construction seams
//! ([`AletheiaDB::write_transaction`]/`write_transaction_with_options`) and
//! the commit-time promotion-race recheck
//! (`WriteTransaction::commit_with_timestamp_inner`). The MCP and HTTP
//! surfaces build their own structured errors (see `src/mcp/auth.rs` and
//! `src/http/error.rs`) but consult [`AletheiaDB::is_replica`] to decide
//! whether to.
//!
//! The networked replication engine (feed, applier, TCP transport) that
//! actually keeps a replica's role/history in sync lands separately behind
//! the `replication` feature (Slice B onward); nothing here depends on it.

use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};

#[cfg(not(target_arch = "wasm32"))]
use crate::core::error::{Error, StorageError};
use crate::core::error::{Result, TransactionError};
use crate::db::AletheiaDB;

/// Encodes [`NodeRole::Primary`] in the atomic role cell.
const ROLE_PRIMARY: u8 = 0;
/// Encodes [`NodeRole::Replica`] in the atomic role cell.
const ROLE_REPLICA: u8 = 1;

/// Whether a database instance accepts writes (`Primary`) or is a read-only
/// follower of another node's write-ahead log (`Replica`).
///
/// A standalone database — the overwhelming majority of embedded use — is
/// trivially "the primary with zero replicas", so every [`AletheiaDB`]
/// starts as `Primary` and nothing changes until
/// [`AletheiaDB::enter_replica_mode`] is called.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeRole {
    /// Accepts writes; the source of truth other nodes replicate from.
    Primary,
    /// Read-only: every write surface (Rust API, MCP, HTTP) rejects with
    /// [`TransactionError::ReadOnlyReplica`] (or that surface's structured
    /// rendering of it). Reads are entirely unaffected.
    Replica,
}

impl NodeRole {
    /// The stable lowercase wire token (`"primary"` / `"replica"`), used by
    /// [`Display`](std::fmt::Display), the optional `serde` impl, and
    /// [`DatabaseStats`](crate::db::DatabaseStats)'s `replication.role` field.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            NodeRole::Primary => "primary",
            NodeRole::Replica => "replica",
        }
    }

    fn from_u8(value: u8) -> Self {
        match value {
            ROLE_REPLICA => NodeRole::Replica,
            _ => NodeRole::Primary,
        }
    }

    fn to_u8(self) -> u8 {
        match self {
            NodeRole::Primary => ROLE_PRIMARY,
            NodeRole::Replica => ROLE_REPLICA,
        }
    }
}

/// Outcome of [`AletheiaDB::promote_to_primary`] (Issue #3355, Slice B).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PromotionReport {
    /// Whether a running replication applier was stopped as part of this
    /// promotion. `false` when the node was already `Primary`, or was a
    /// `Replica` on which `start_replication`/`bootstrap_replica` was never
    /// called (nothing to stop).
    pub applier_stopped: bool,
    /// The replica's last fully-applied primary LSN at the moment of
    /// promotion, when an applier was running. `None` otherwise.
    pub last_applied_lsn: Option<u64>,
    /// `Some(error)` when the promotion-point index persist failed (or was
    /// attempted with persistence configured but errored). Replicated history
    /// is NEVER in the promoted node's local WAL -- the applier applies
    /// without appending -- so until the next successful persist, everything
    /// applied since the last periodic persist exists in memory only, and a
    /// crash before then loses it with no former primary left to re-stream
    /// from. An operator seeing this should retry persistence (or back up)
    /// before trusting the promoted node's durability. `None` when the
    /// persist succeeded or no persistence is configured.
    pub index_persist_error: Option<String>,
}

impl std::fmt::Display for NodeRole {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

// Always the same lowercase token `as_str()`/`Display` already produce, kept
// as a distinct impl (rather than a `derive`) so the wire representation
// cannot silently drift from `Display` and so this compiles out entirely
// when `serde` is not enabled (this module carries no feature flag of its
// own -- Slice A is always compiled).
#[cfg(feature = "serde")]
impl serde::Serialize for NodeRole {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

/// Construct the initial role cell for a freshly-created [`AletheiaDB`]:
/// every database starts as a writable primary.
pub(crate) fn new_role_cell() -> Arc<AtomicU8> {
    Arc::new(AtomicU8::new(ROLE_PRIMARY))
}

/// Return [`TransactionError::ReadOnlyReplica`] if `role` currently reads
/// `Replica`. Shared by the write-transaction construction seams
/// (`AletheiaDB::write_transaction`/`write_transaction_with_options`) and the
/// commit-time promotion-race recheck in
/// `WriteTransaction::commit_with_timestamp_inner`.
pub(crate) fn reject_if_replica(role: &AtomicU8) -> Result<()> {
    if NodeRole::from_u8(role.load(Ordering::SeqCst)) == NodeRole::Replica {
        return Err(TransactionError::ReadOnlyReplica.into());
    }
    Ok(())
}

impl AletheiaDB {
    /// This node's current replication role.
    ///
    /// `Primary` (the default) unless [`enter_replica_mode`](Self::enter_replica_mode)
    /// has been called with no subsequent [`promote_to_primary`](Self::promote_to_primary).
    #[must_use]
    pub fn node_role(&self) -> NodeRole {
        NodeRole::from_u8(self.role.load(Ordering::SeqCst))
    }

    /// Shorthand for `self.node_role() == NodeRole::Replica`.
    #[must_use]
    pub fn is_replica(&self) -> bool {
        self.node_role() == NodeRole::Replica
    }

    /// Enter read-only replica mode.
    ///
    /// Used by the replication subsystem (Slice B) — or an embedded replica
    /// applier — to flip this node into a read-only follower: from the next
    /// call onward every write surface (Rust `write_transaction`, the MCP
    /// write/admin tools, HTTP write-class `/query` requests) rejects with
    /// [`TransactionError::ReadOnlyReplica`]. Reads are entirely unaffected.
    /// Idempotent.
    pub fn enter_replica_mode(&self) {
        self.role.store(NodeRole::Replica.to_u8(), Ordering::SeqCst);
    }

    /// Promote this node back to a writable primary.
    ///
    /// If a replication applier is running (started via
    /// [`start_replication`](AletheiaDB::start_replication) /
    /// [`bootstrap_replica`](AletheiaDB::bootstrap_replica)), this:
    ///
    /// 1. Stops and joins the applier thread (no more replicated writes can
    ///    land after this point).
    /// 2. Seeds the local WAL's next LSN to `applied_lsn + 1` (never
    ///    backwards) so a newly-accepted write's LSN can never collide with
    ///    history this node already applied as a replica.
    /// 3. Persists indexes at the promotion point, if index persistence is
    ///    configured, so a subsequent restart resumes from here rather than
    ///    needing to re-replicate from the (now former) primary.
    /// 4. Flips the role atomic to `Primary`.
    ///
    /// On a node with no running applier (already `Primary`, or a `Replica`
    /// on which replication was never started), only step 4 runs. Idempotent
    /// — promoting an already-primary node is `Ok(_)` and a no-op.
    pub fn promote_to_primary(&self) -> Result<PromotionReport> {
        let mut report = PromotionReport::default();

        #[cfg(not(target_arch = "wasm32"))]
        {
            let applier = self
                .replication
                .lock()
                .map_err(|_| {
                    Error::Storage(StorageError::LockPoisoned {
                        resource: "replication".to_string(),
                    })
                })?
                .take();

            if let Some(applier) = applier {
                // Keep the progress handle across the join: the applier can
                // legitimately complete one more batch between any
                // pre-join read and the thread actually stopping, and
                // seeding the allocator from a stale value would hand out
                // LSNs already materialized in state (primary-space
                // collision). Only the post-join read is authoritative.
                let progress = std::sync::Arc::clone(&applier.progress);
                report.applier_stopped = true;

                // Stops + joins the background thread.
                drop(applier);

                // Issue #3788: the applier is joined, so no publish can still
                // be in flight -- disarming here restores the ungated
                // current-state read fast path for the promoted primary.
                self.current.disarm_apply_gate();

                let applied_lsn = progress.last_applied_lsn();
                report.last_applied_lsn = Some(applied_lsn);

                // LSN continuity (mirrors the Issue #3420 recovery-seeding
                // contract): only ever move the allocator forward.
                let next = crate::storage::wal::LSN(applied_lsn.saturating_add(1));
                if next > self.wal.current_lsn() {
                    self.wal.set_next_lsn(next);
                }

                if let (Some(manager), Some(tracker)) =
                    (&self.persistence_manager, &self.persistence_tracker)
                {
                    // Surface (never swallow) a persist failure: replicated
                    // history is not in the local WAL, so a crash before the
                    // next successful persist would lose it silently. The
                    // promotion itself still completes -- the node must
                    // become writable -- but the report tells the operator
                    // durability is not yet re-established.
                    if let Err(e) =
                        crate::storage::index_persistence::operations::persist_all_indexes_at_lsn(
                            &self.current,
                            &self.historical,
                            &self.temporal_indexes,
                            manager,
                            tracker,
                            applied_lsn,
                        )
                    {
                        report.index_persist_error = Some(e.to_string());
                    }
                }
            }
        }

        self.role.store(NodeRole::Primary.to_u8(), Ordering::SeqCst);
        Ok(report)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_role_is_primary() {
        let db = AletheiaDB::new().expect("create db");
        assert_eq!(db.node_role(), NodeRole::Primary);
        assert!(!db.is_replica());
    }

    #[test]
    fn enter_replica_mode_flips_role() {
        let db = AletheiaDB::new().expect("create db");
        db.enter_replica_mode();
        assert_eq!(db.node_role(), NodeRole::Replica);
        assert!(db.is_replica());
    }

    #[test]
    fn promote_to_primary_flips_back_and_is_idempotent() {
        let db = AletheiaDB::new().expect("create db");
        db.enter_replica_mode();
        assert!(db.is_replica());
        db.promote_to_primary().expect("promote");
        assert_eq!(db.node_role(), NodeRole::Primary);
        // Promoting an already-primary node is a no-op success.
        db.promote_to_primary().expect("promote again");
        assert_eq!(db.node_role(), NodeRole::Primary);
    }

    #[test]
    fn node_role_display_and_as_str_are_lowercase() {
        assert_eq!(NodeRole::Primary.as_str(), "primary");
        assert_eq!(NodeRole::Replica.as_str(), "replica");
        assert_eq!(NodeRole::Primary.to_string(), "primary");
        assert_eq!(NodeRole::Replica.to_string(), "replica");
    }
}
