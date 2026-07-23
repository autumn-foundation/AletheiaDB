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
    /// Slice A scope: flips the role atomic back to `Primary`, so writes are
    /// accepted again from the next call onward. Idempotent — promoting an
    /// already-primary node is `Ok(())` and a no-op.
    ///
    /// # Seam for Slice B
    ///
    /// The full promotion procedure -- stopping the replica applier thread,
    /// seeding the local WAL's next LSN to `applied_lsn + 1` (so newly
    /// accepted writes can never collide with history this node already
    /// applied as a replica), and persisting indexes at the promotion point
    /// -- lands with the replication engine (Issue #3355 Slice B), which
    /// hooks into this method. The `&self -> Result<()>` signature already
    /// anticipates that: a failure partway through that fuller procedure
    /// (e.g. the applier not stopping within its shutdown timeout) can be
    /// reported without a breaking API change.
    pub fn promote_to_primary(&self) -> Result<()> {
        self.role.store(NodeRole::Primary.to_u8(), Ordering::SeqCst);
        Ok(())
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
