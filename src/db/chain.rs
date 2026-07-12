//! Provenance hash chain integration on [`AletheiaDB`] (Issue #3351).
//!
//! This module wires the opt-in tamper-evident chain into the database:
//! - capturing the version refs a committed transaction produced (hot path),
//! - rebuilding the unsealed tail from replayed history at startup, and
//! - the public verification / anchor-export API.
//!
//! When the chain is disabled (the default), none of this runs and the verify
//! API returns a clear "not enabled" error, so behavior is byte-identical to a
//! database without the feature.

use std::collections::BTreeMap;
use std::sync::Arc;

use crate::api::transaction::{BufferedWrite, WriteTransaction};
use crate::core::error::{Error, Result};
use crate::core::id::{EdgeId, NodeId};
use crate::core::temporal::Timestamp;
use crate::db::AletheiaDB;
use crate::provenance_chain::{
    ChainHead, ChainVerification, EntityKind, PendingTx, ProvenanceChain,
};

/// A single version reference: `(kind, entity_id, version_id)`.
type VersionRef = (EntityKind, u64, u64);

/// Restored versions grouped by their exact commit timestamp `(wallclock,
/// logical)` — one group per recovered transaction, keyed for ascending order.
type CommitGroups = BTreeMap<(i64, u32), Vec<VersionRef>>;

/// Version refs captured from a transaction's write buffer *before* commit
/// consumes it. Create/update refs are exact (their version ids live in the
/// buffer); delete/retract entity ids are resolved to their closing version id
/// after commit via [`AletheiaDB::finalize_chain_capture`].
pub(crate) struct ChainCapture {
    tx_id: u64,
    refs: Vec<(EntityKind, u64, u64)>,
    closing_nodes: Vec<u64>,
    closing_edges: Vec<u64>,
}

impl AletheiaDB {
    /// Capture the version refs a transaction will commit, from its write
    /// buffer. Called on the hot path only when the chain is enabled.
    pub(crate) fn precapture_chain(&self, tx: &WriteTransaction) -> ChainCapture {
        let mut refs = Vec::new();
        let mut closing_nodes = Vec::new();
        let mut closing_edges = Vec::new();
        for op in tx.buffer.operations() {
            match op {
                BufferedWrite::CreateNode {
                    node_id,
                    version_id,
                    ..
                }
                | BufferedWrite::UpdateNode {
                    node_id,
                    version_id,
                    ..
                } => refs.push((EntityKind::Node, node_id.as_u64(), version_id.as_u64())),
                BufferedWrite::CreateEdge {
                    edge_id,
                    version_id,
                    ..
                }
                | BufferedWrite::UpdateEdge {
                    edge_id,
                    version_id,
                    ..
                } => refs.push((EntityKind::Edge, edge_id.as_u64(), version_id.as_u64())),
                BufferedWrite::DeleteNode { node_id, .. }
                | BufferedWrite::RetractNode { node_id, .. } => {
                    closing_nodes.push(node_id.as_u64())
                }
                BufferedWrite::DeleteEdge { edge_id, .. }
                | BufferedWrite::RetractEdge { edge_id, .. } => {
                    closing_edges.push(edge_id.as_u64())
                }
            }
        }
        ChainCapture {
            tx_id: tx.tx_id.as_u64(),
            refs,
            closing_nodes,
            closing_edges,
        }
    }

    /// Resolve any delete/retract closing versions (their ids are allocated at
    /// apply time, not in the buffer) and produce the [`PendingTx`] to enqueue.
    ///
    /// v1 caveat: the closing version id is read as the entity's *current* head
    /// right after commit. Under a concurrent supersession of the same entity
    /// between this commit and the head read, a later immutable version could be
    /// attributed to this transaction — a fidelity edge case that never breaks
    /// verification (seal and verify both fetch the same version id).
    pub(crate) fn finalize_chain_capture(
        &self,
        mut cap: ChainCapture,
        commit_ts: Timestamp,
        anchor_lsn: u64,
    ) -> PendingTx {
        if !cap.closing_nodes.is_empty() || !cap.closing_edges.is_empty() {
            let historical = self.historical.read();
            for nid in cap.closing_nodes.drain(..) {
                if let Ok(node_id) = NodeId::new(nid)
                    && let Some(vid) = historical.get_current_node_version(node_id)
                {
                    cap.refs.push((EntityKind::Node, nid, vid.as_u64()));
                }
            }
            for eid in cap.closing_edges.drain(..) {
                if let Ok(edge_id) = EdgeId::new(eid)
                    && let Some(vid) = historical.get_current_edge_version(edge_id)
                {
                    cap.refs.push((EntityKind::Edge, eid, vid.as_u64()));
                }
            }
        }
        PendingTx {
            commit_ts_micros: commit_ts.wallclock(),
            tx_id: cap.tx_id,
            anchor_lsn,
            entity_refs: cap.refs,
        }
    }

    /// Finalize a pre-commit capture and enqueue it to the sealer. A no-op when
    /// the chain is disabled or no capture was taken (the hot path calls this
    /// unconditionally; the `Option`s collapse to nothing when disabled).
    pub(crate) fn enqueue_chain_commit(&self, capture: Option<ChainCapture>, commit_ts: Timestamp) {
        if let (Some(chain), Some(cap)) = (self.chain.as_ref(), capture) {
            let anchor_lsn = self.wal.current_lsn().0;
            let pending = self.finalize_chain_capture(cap, commit_ts, anchor_lsn);
            chain.enqueue_commit(pending);
        }
    }

    /// Rebuild the unsealed tail of the chain from replayed history (Issue #3351
    /// AC6). Groups every restored node/edge version by its exact commit
    /// timestamp (a transaction's versions share one HLC stamp) and, for each
    /// group beyond the loaded chain head, seals it synchronously in ascending
    /// commit order so the chain covers the full recovered prefix.
    ///
    /// Called once at startup after WAL replay and before the sealer thread
    /// starts. Idempotent against an already-sealed prefix: only transactions
    /// with a commit timestamp strictly beyond the head are folded in.
    pub(crate) fn rebuild_chain_tail(&self, chain: &Arc<ProvenanceChain>) {
        let head = chain.head();
        let head_ts = head.commit_ts;

        // Group version refs by their full commit Timestamp so two transactions
        // sharing a wallclock (different logical) stay distinct.
        let mut groups: CommitGroups = BTreeMap::new();
        {
            let historical = self.historical.read();
            for (vid, v) in historical.get_node_versions() {
                let ts = v.temporal.transaction_time().start();
                groups
                    .entry((ts.wallclock(), ts.logical()))
                    .or_default()
                    .push((EntityKind::Node, v.node_id.as_u64(), vid.as_u64()));
            }
            for (vid, v) in historical.get_edge_versions() {
                let ts = v.temporal.transaction_time().start();
                groups
                    .entry((ts.wallclock(), ts.logical()))
                    .or_default()
                    .push((EntityKind::Edge, v.edge_id.as_u64(), vid.as_u64()));
            }
        }

        for ((wallclock, _logical), refs) in groups {
            // Only seal transactions beyond the loaded head. When the head is
            // genesis (seq 0) every restored transaction is unsealed; otherwise
            // skip anything at or before the head's commit timestamp (already
            // sealed). v1 limitation: two transactions sharing a wallclock where
            // one is the head are not distinguished (rare HLC collision).
            if head.seq != 0 && wallclock <= head_ts {
                continue;
            }
            let pending = PendingTx {
                commit_ts_micros: wallclock,
                // Synthetic, deterministic tx id for rebuilt records: the WAL
                // does not carry the original tx id, and these transactions were
                // never live-sealed, so any stable value keeps the record
                // internally consistent (verify recomputes from stored fields).
                tx_id: wallclock as u64,
                anchor_lsn: head.anchor_lsn,
                entity_refs: refs,
            };
            let _ = chain.seal_pending_sync(pending);
        }
        let _ = chain.checkpoint();
    }

    /// Verify the entire provenance hash chain against stored history
    /// (Issue #3351 AC2). Returns the earliest broken sequence number on tamper.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Other`] when the provenance hash chain is not enabled on
    /// this database (see [`ChainConfig`](crate::provenance_chain::ChainConfig)).
    pub fn verify_chain(&self) -> Result<ChainVerification> {
        Ok(self.require_chain()?.verify_full())
    }

    /// Verify only one entity's contribution to the chain (Issue #3351 AC2),
    /// recomputing just that entity's leaves.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Other`] when the chain is not enabled.
    pub fn verify_entity_chain(&self, kind: EntityKind, id: u64) -> Result<ChainVerification> {
        Ok(self.require_chain()?.verify_entity(kind, id))
    }

    /// Export the current chain head as an external anchor (Issue #3351 AC4).
    ///
    /// # Errors
    ///
    /// Returns [`Error::Other`] when the chain is not enabled.
    pub fn export_chain_head(&self) -> Result<ChainHead> {
        Ok(self.require_chain()?.export_head())
    }

    /// Prove the current chain append-only-extends a previously exported anchor
    /// (Issue #3351 AC4); detects rollback (truncation) and fork (divergence).
    ///
    /// # Errors
    ///
    /// Returns [`Error::Other`] when the chain is not enabled.
    pub fn verify_chain_against(&self, anchor: &ChainHead) -> Result<ChainVerification> {
        Ok(self.require_chain()?.verify_against_anchor(anchor))
    }

    fn require_chain(&self) -> Result<&Arc<ProvenanceChain>> {
        self.chain.as_ref().ok_or_else(|| {
            Error::Other(
                "provenance hash chain is not enabled (set ChainConfig::enabled = true)"
                    .to_string(),
            )
        })
    }
}
