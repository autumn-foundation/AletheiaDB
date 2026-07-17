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
            commit_ts_logical: commit_ts.logical(),
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

        for ((wallclock, logical), refs) in groups {
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
                // The full HLC commit timestamp (wallclock + logical) is what the
                // digest binds — identical in the live-seal and rebuild paths, so
                // a rebuilt chain reproduces the live head (Issue #3351 finding 4).
                commit_ts_logical: logical,
                // Synthetic tx id for rebuilt records: the WAL does not carry the
                // original tx id, and it is NOT folded into the digest — it is
                // record metadata only, so any stable value is fine.
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
    /// Returns [`Error::FailedPrecondition`] when the provenance hash chain is not enabled on
    /// this database (see [`ChainConfig`](crate::provenance_chain::ChainConfig)).
    pub fn verify_chain(&self) -> Result<ChainVerification> {
        Ok(self.require_chain()?.verify_full())
    }

    /// Verify only one entity's contribution to the chain (Issue #3351 AC2),
    /// recomputing just that entity's leaves.
    ///
    /// # Errors
    ///
    /// Returns [`Error::FailedPrecondition`] when the chain is not enabled.
    pub fn verify_entity_chain(&self, kind: EntityKind, id: u64) -> Result<ChainVerification> {
        Ok(self.require_chain()?.verify_entity(kind, id))
    }

    /// Export the current chain head as an external anchor (Issue #3351 AC4).
    ///
    /// # Errors
    ///
    /// Returns [`Error::FailedPrecondition`] when the chain is not enabled.
    pub fn export_chain_head(&self) -> Result<ChainHead> {
        Ok(self.require_chain()?.export_head())
    }

    /// Prove the current chain append-only-extends a previously exported anchor
    /// (Issue #3351 AC4); detects rollback (truncation) and fork (divergence).
    ///
    /// # Errors
    ///
    /// Returns [`Error::FailedPrecondition`] when the chain is not enabled.
    pub fn verify_chain_against(&self, anchor: &ChainHead) -> Result<ChainVerification> {
        Ok(self.require_chain()?.verify_against_anchor(anchor))
    }

    fn require_chain(&self) -> Result<&Arc<ProvenanceChain>> {
        self.chain.as_ref().ok_or_else(|| {
            Error::FailedPrecondition(
                "provenance hash chain is not enabled (set ChainConfig::enabled = true)"
                    .to_string(),
            )
        })
    }
}

#[cfg(test)]
mod tamper_tests {
    //! Real historical-version tamper tests (Issue #3351 finding 6, exercising
    //! findings 1 & 2). Unlike the AC3 integration test (which tampers the sealed
    //! `chain.log`), these mutate a **stored data version**'s content in place via
    //! the `pub(crate)` `insert_restored_*_version` storage API — the same path
    //! index-persistence restore uses — then assert `verify_chain` recomputes the
    //! leaf from the tampered version, finds it disagrees with the sealed leaf,
    //! and localizes the break. This is the dual of AC3 and the only in-crate way
    //! to reach the stored version without touching off-limits storage internals.

    use std::time::Duration;

    use crate::PropertyMapBuilder;
    use crate::config::{AletheiaDBConfig, WalConfigBuilder};
    use crate::core::interning::GLOBAL_INTERNER;
    use crate::core::temporal::{BiTemporalInterval, time};
    use crate::db::AletheiaDB;
    use crate::provenance_chain::{ChainConfig, ChainFsyncMode};
    use crate::storage::index_persistence::PersistenceConfig;
    use crate::storage::wal::DurabilityMode;

    fn enabled_db(dir: &std::path::Path) -> AletheiaDB {
        let config = AletheiaDBConfig::builder()
            .wal(
                WalConfigBuilder::new()
                    .wal_dir(dir.join("wal"))
                    .durability_mode(DurabilityMode::Synchronous)
                    .build(),
            )
            .persistence(PersistenceConfig {
                enabled: true,
                data_dir: dir.join("indexes"),
                load_on_startup: true,
                ..Default::default()
            })
            .chain(ChainConfig {
                enabled: true,
                fsync: ChainFsyncMode::PerTransaction,
                dir: None,
            })
            .build();
        AletheiaDB::with_unified_config(config).unwrap()
    }

    fn props(name: &str) -> crate::PropertyMap {
        PropertyMapBuilder::new().insert("name", name).build()
    }

    fn wait_seq(db: &AletheiaDB, expected: u64) {
        for _ in 0..300 {
            if db.export_chain_head().unwrap().seq >= expected {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!(
            "chain head stuck at {}",
            db.export_chain_head().unwrap().seq
        );
    }

    /// Finding 1: relabeling a stored node version is caught (label is now bound
    /// into the leaf).
    #[test]
    fn tampering_stored_node_label_is_detected() {
        let dir = tempfile::tempdir().unwrap();
        let db = enabled_db(dir.path());
        let alice = db.create_node("Person", props("Alice")).unwrap();
        wait_seq(&db, 1);
        assert!(db.verify_chain().unwrap().passed);

        // Relabel the stored version in place.
        {
            let mut hist = db.historical.write();
            let vid = hist.get_current_node_version(alice).unwrap();
            let mut v = hist.get_node_version(vid).unwrap().clone();
            v.label = GLOBAL_INTERNER.intern("Robot").unwrap();
            hist.insert_restored_node_version(v).unwrap();
        }

        let v = db.verify_chain().unwrap();
        assert!(!v.passed, "a relabel must be detected");
        assert!(v.earliest_broken_seq.is_some(), "tamper is localized");
    }

    /// Finding 1: rewiring a stored edge version's target is caught (endpoints are
    /// now bound into the leaf).
    #[test]
    fn tampering_stored_edge_endpoint_is_detected() {
        let dir = tempfile::tempdir().unwrap();
        let db = enabled_db(dir.path());
        let a = db.create_node("Person", props("Alice")).unwrap();
        let b = db.create_node("Person", props("Bob")).unwrap();
        let c = db.create_node("Person", props("Carol")).unwrap();
        let edge = db.create_edge(a, b, "KNOWS", props("")).unwrap();
        wait_seq(&db, 4);
        assert!(db.verify_chain().unwrap().passed);

        // Rewire the stored edge's target from b to c.
        {
            let mut hist = db.historical.write();
            let vid = hist.get_current_edge_version(edge).unwrap();
            let mut v = hist.get_edge_version(vid).unwrap().clone();
            v.target = c;
            hist.insert_restored_edge_version(v).unwrap();
        }

        let v = db.verify_chain().unwrap();
        assert!(!v.passed, "an edge endpoint rewire must be detected");
        assert!(v.earliest_broken_seq.is_some());
    }

    /// Finding 2: extending a retraction version's `valid_to` (re-validating an
    /// ended fact) is caught — the born-closed terminal `valid_to` is bound.
    #[test]
    fn tampering_retraction_valid_to_is_detected() {
        let dir = tempfile::tempdir().unwrap();
        let db = enabled_db(dir.path());
        let alice = db.create_node("Person", props("Alice")).unwrap();
        // Retract at "now": closes valid time. Seals as a second transaction.
        db.retract_node(alice, time::now()).unwrap();
        wait_seq(&db, 2);
        assert!(db.verify_chain().unwrap().passed);

        // Extend the stored retraction's valid_to far into the future.
        {
            let mut hist = db.historical.write();
            let vid = hist.get_current_node_version(alice).unwrap();
            let mut v = hist.get_node_version(vid).unwrap().clone();
            let vf = v.temporal.valid_time().start();
            let extended = crate::core::hlc::HybridTimestamp::new_unchecked(
                vf.wallclock() + 10_000_000_000,
                0,
            );
            v.temporal = v.temporal.close_valid_time(extended).unwrap();
            hist.insert_restored_node_version(v).unwrap();
        }

        let v = db.verify_chain().unwrap();
        assert!(
            !v.passed,
            "extending a retraction valid_to must be detected"
        );
        assert!(v.earliest_broken_seq.is_some());
    }

    /// Finding 2: re-opening a delete tombstone's interval (un-deleting) is caught
    /// — the tombstone flag is bound into the leaf.
    #[test]
    fn tampering_tombstone_reopen_is_detected() {
        let dir = tempfile::tempdir().unwrap();
        let db = enabled_db(dir.path());
        let alice = db.create_node("Person", props("Alice")).unwrap();
        // Delete (no connected edges): creates a tombstone, sealed as tx 2.
        db.delete_node_with_valid_time(alice, None).unwrap();
        wait_seq(&db, 2);
        assert!(db.verify_chain().unwrap().passed);

        // Re-open the tombstone's empty valid interval into an open one.
        {
            let mut hist = db.historical.write();
            let vid = hist.get_current_node_version(alice).unwrap();
            let mut v = hist.get_node_version(vid).unwrap().clone();
            let vf = v.temporal.valid_time().start();
            let tx_from = v.temporal.transaction_time().start();
            v.temporal = BiTemporalInterval::with_valid_time(vf, tx_from); // open valid
            hist.insert_restored_node_version(v).unwrap();
        }

        let v = db.verify_chain().unwrap();
        assert!(!v.passed, "re-opening a tombstone must be detected");
        assert!(v.earliest_broken_seq.is_some());
    }
}

#[cfg(all(test, feature = "audit-export"))]
mod crypto_shred_ac4_tests {
    //! AC4 (Issue #3359, GDPR crypto-shred slice 2): the provenance hash chain
    //! stays verifiable **after** a subject is crypto-shredded, and a tamper of a
    //! sealed value is still caught.
    //!
    //! This holds **by construction** as of the seal-before-apply write path
    //! (#3689): a designated property is sealed to a `PropertyValue::Bytes(SUBJ…)`
    //! envelope in the write buffer *before* `apply_changes`, so historical
    //! storage records ciphertext. The chain leaf is computed *post-commit* by the
    //! sealer from `DbVersionSource`, which reconstructs from the **raw**
    //! `reconstruct_*_properties` storage path (crypto-shred-unaware) — so the leaf
    //! binds the stored ciphertext, never plaintext. `erase_subject` destroys only
    //! the subject key; the stored envelope bytes survive, so `verify_chain`
    //! recomputes an identical leaf and the chain stays valid. Mutating the stored
    //! envelope, by contrast, changes the recomputed leaf and is detected.
    //!
    //! These tests combine the `chain` + `encryption` + designation harnesses
    //! (neither `tamper_tests::enabled_db` nor `crypto_shred::integration_tests`
    //! configures both) and are the regression guard for that property.

    use std::sync::Arc;
    use std::time::Duration;

    use crate::PropertyMapBuilder;
    use crate::config::{AletheiaDBConfig, WalConfigBuilder};
    use crate::core::property::PropertyValue;
    use crate::core::version::VersionData;
    use crate::db::AletheiaDB;
    use crate::db::crypto_shred::DesignationTarget;
    use crate::db::crypto_shred::envelope::is_envelope;
    use crate::encryption::FileKeyProvider;
    use crate::encryption::config::EncryptionConfig;
    use crate::provenance_chain::{ChainConfig, ChainFsyncMode};
    use crate::storage::index_persistence::PersistenceConfig;
    use crate::storage::wal::DurabilityMode;

    const SEALED: &str = "AC4_SEALED_PLAINTEXT_that_will_be_crypto_shredded_c1f2";

    /// A combined config: WAL + index persistence + provenance chain + encryption,
    /// so a single database exercises seal-at-write together with the chain
    /// sealer. `enabled_db` (chain only) and the crypto-shred integration harness
    /// (encryption only) each configure just one half; AC4 needs both.
    fn enc_chain_db(dir: &std::path::Path) -> AletheiaDB {
        let key_file = dir.join("mek.key");
        if !key_file.exists() {
            FileKeyProvider::generate_key_file(&key_file).unwrap();
        }
        let config = AletheiaDBConfig::builder()
            .wal(
                WalConfigBuilder::new()
                    .wal_dir(dir.join("wal"))
                    .durability_mode(DurabilityMode::Synchronous)
                    .build(),
            )
            .persistence(PersistenceConfig {
                enabled: true,
                data_dir: dir.join("indexes"),
                load_on_startup: true,
                ..Default::default()
            })
            .chain(ChainConfig {
                enabled: true,
                fsync: ChainFsyncMode::PerTransaction,
                dir: None,
            })
            .encryption(EncryptionConfig::file_based(&key_file))
            .build();
        AletheiaDB::with_unified_config(config).unwrap()
    }

    fn wait_seq(db: &AletheiaDB, expected: u64) {
        for _ in 0..300 {
            if db.export_chain_head().unwrap().seq >= expected {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!(
            "chain head stuck at {}",
            db.export_chain_head().unwrap().seq
        );
    }

    /// Read the raw (stored) value of a node property through the *same* storage
    /// reconstruction path the chain source uses — NOT the unsealing read
    /// boundary — so the returned value is the on-disk sealed envelope.
    fn stored_node_property(
        db: &AletheiaDB,
        node: crate::core::id::NodeId,
        key: &str,
    ) -> PropertyValue {
        let hist = db.historical.read();
        let vid = hist.get_current_node_version(node).unwrap();
        let props = hist.reconstruct_node_properties(vid).unwrap();
        props.get(key).unwrap().clone()
    }

    /// (a) The provenance chain still verifies after a **node** subject is
    /// crypto-shredded: the leaf bound the ciphertext, which survives erasure.
    #[test]
    fn verify_chain_passes_after_crypto_shred_node() {
        let dir = tempfile::tempdir().unwrap();
        let db = enc_chain_db(dir.path());

        // Placeholder (plaintext) so we capture the id, THEN designate + replace so
        // the sealed version carries the designated value.
        let alice = db
            .create_node(
                "Person",
                PropertyMapBuilder::new()
                    .insert("stage", "placeholder")
                    .build(),
            )
            .unwrap();
        db.designate_subject(
            "subject-A",
            vec![DesignationTarget::WholeNode(alice.as_u64())],
        )
        .unwrap();
        db.replace_node(
            alice,
            "Person",
            PropertyMapBuilder::new().insert("email", SEALED).build(),
        )
        .unwrap();
        // A second designated update, to exercise a multi-version sealed chain.
        db.replace_node(
            alice,
            "Person",
            PropertyMapBuilder::new()
                .insert("email", SEALED)
                .insert("v", 2_i64)
                .build(),
        )
        .unwrap();
        wait_seq(&db, 3);

        // Guardrail: the stored value is the sealed SUBJ envelope (ciphertext),
        // not the plaintext — this is exactly what the chain leaf binds.
        match &stored_node_property(&db, alice, "email") {
            PropertyValue::Bytes(b) => {
                assert!(is_envelope(b), "designated value must be sealed at rest");
                assert_ne!(
                    b.as_ref(),
                    SEALED.as_bytes(),
                    "leaf must not bind plaintext"
                );
            }
            other => panic!("expected sealed Bytes envelope at rest, got {other:?}"),
        }

        // Chain verifies while the subject is active.
        assert!(
            db.verify_chain().unwrap().passed,
            "chain must verify before erasure"
        );

        // Crypto-shred the subject: destroys the key only, not the ciphertext.
        db.erase_subject("subject-A").unwrap();

        // The stored value is still the same opaque SUBJ envelope (only the key is
        // gone) — so the recomputed leaf is unchanged.
        match &stored_node_property(&db, alice, "email") {
            PropertyValue::Bytes(b) => {
                assert!(is_envelope(b), "erased value stays a SUBJ envelope");
                assert_ne!(
                    b.as_ref(),
                    SEALED.as_bytes(),
                    "erased read never surfaces plaintext"
                );
            }
            other => panic!("expected sealed Bytes envelope after erase, got {other:?}"),
        }

        // AC4: the chain STILL verifies after the crypto-shred. Clear the
        // reconstruction cache first so verify re-reads the *surviving* ciphertext
        // from storage rather than a warm cache — proving the leaf recomputes
        // identically from the on-disk envelope, not from a stale plaintext read.
        db.historical.read().__test_clear_property_cache();
        let v = db.verify_chain().unwrap();
        assert!(
            v.passed,
            "provenance chain must remain valid after crypto-shred (leaf bound ciphertext); earliest_broken_seq={:?}",
            v.earliest_broken_seq
        );
    }

    /// (a, edge variant) Same property for an **edge** subject.
    #[test]
    fn verify_chain_passes_after_crypto_shred_edge() {
        let dir = tempfile::tempdir().unwrap();
        let db = enc_chain_db(dir.path());

        let a = db
            .create_node("Person", PropertyMapBuilder::new().insert("n", "a").build())
            .unwrap();
        let b = db
            .create_node("Person", PropertyMapBuilder::new().insert("n", "b").build())
            .unwrap();
        let edge = db
            .create_edge(
                a,
                b,
                "KNOWS",
                PropertyMapBuilder::new().insert("stage", "ph").build(),
            )
            .unwrap();
        db.designate_subject(
            "subject-E",
            vec![DesignationTarget::WholeEdge(edge.as_u64())],
        )
        .unwrap();
        db.replace_edge(
            edge,
            PropertyMapBuilder::new().insert("secret", SEALED).build(),
        )
        .unwrap();
        wait_seq(&db, 4);

        // Guardrail: stored edge value is a sealed envelope.
        {
            let hist = db.historical.read();
            let vid = hist.get_current_edge_version(edge).unwrap();
            let props = hist.reconstruct_edge_properties(vid).unwrap();
            match props.get("secret").unwrap() {
                PropertyValue::Bytes(bytes) => {
                    assert!(is_envelope(bytes), "designated edge value must be sealed");
                    assert_ne!(bytes.as_ref(), SEALED.as_bytes());
                }
                other => panic!("expected sealed edge Bytes, got {other:?}"),
            }
        }

        assert!(
            db.verify_chain().unwrap().passed,
            "chain valid before erase"
        );
        db.erase_subject("subject-E").unwrap();
        // Re-read the surviving ciphertext from storage (not a warm cache).
        db.historical.read().__test_clear_edge_property_cache();
        let v = db.verify_chain().unwrap();
        assert!(
            v.passed,
            "edge chain must remain valid after crypto-shred; earliest_broken_seq={:?}",
            v.earliest_broken_seq
        );
    }

    /// (b) Tampering with the stored sealed envelope IS still detected — the chain
    /// binds the ciphertext bytes, so any mutation of them breaks verification.
    #[test]
    fn tamper_of_sealed_envelope_still_detected() {
        let dir = tempfile::tempdir().unwrap();
        let db = enc_chain_db(dir.path());

        let alice = db
            .create_node(
                "Person",
                PropertyMapBuilder::new()
                    .insert("stage", "placeholder")
                    .build(),
            )
            .unwrap();
        db.designate_subject(
            "subject-T",
            vec![DesignationTarget::WholeNode(alice.as_u64())],
        )
        .unwrap();
        db.replace_node(
            alice,
            "Person",
            PropertyMapBuilder::new().insert("email", SEALED).build(),
        )
        .unwrap();
        wait_seq(&db, 2);
        assert!(db.verify_chain().unwrap().passed);

        // Flip one byte of the stored sealed-envelope value in place, then clear
        // the reconstruction cache. The cache assumes per-version immutability
        // (`node_property_cache`, never invalidated), so a genuine on-disk tamper
        // is only re-read after the cache misses — e.g. on a fresh process / cold
        // reopen. Clearing it here reproduces that fresh read within one process.
        {
            let mut hist = db.historical.write();
            let vid = hist.get_current_node_version(alice).unwrap();
            let mut v = hist.get_node_version(vid).unwrap().clone();
            let full = hist.reconstruct_node_properties(vid).unwrap();
            let mut env = match full.get("email").unwrap() {
                PropertyValue::Bytes(b) => {
                    assert!(is_envelope(b), "value must be sealed to tamper it");
                    b.to_vec()
                }
                other => panic!("expected sealed Bytes to tamper, got {other:?}"),
            };
            let last = env.len() - 1;
            env[last] ^= 0xFF; // mutate the AEAD tag/ciphertext region
            let tampered = full
                .builder()
                .insert("email", PropertyValue::Bytes(Arc::from(env)))
                .build();
            v.data = VersionData::anchor(tampered);
            hist.insert_restored_node_version(v).unwrap();
            hist.__test_clear_property_cache();
        }

        let v = db.verify_chain().unwrap();
        assert!(!v.passed, "a mutated sealed envelope must be detected");
        assert!(v.earliest_broken_seq.is_some(), "tamper is localized");
    }

    /// (c) Sanity: with encryption enabled but NO designation, a normal node's
    /// value is stored as plaintext and the chain verifies — zero regression for
    /// non-crypto-shred data.
    #[test]
    fn non_designated_node_chain_still_verifies() {
        let dir = tempfile::tempdir().unwrap();
        let db = enc_chain_db(dir.path());
        let bob = db
            .create_node(
                "Person",
                PropertyMapBuilder::new().insert("name", "Bob").build(),
            )
            .unwrap();
        wait_seq(&db, 1);

        // Non-designated: stored value is exactly the plaintext (never an envelope).
        assert_eq!(
            stored_node_property(&db, bob, "name"),
            PropertyValue::from("Bob".to_string()),
            "a non-designated write must not be sealed"
        );
        assert!(db.verify_chain().unwrap().passed);
    }
}
