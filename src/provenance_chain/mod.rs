//! Tamper-evident provenance hash chain (Issue #3351).
//!
//! An opt-in, self-contained sidecar that binds every committed transaction into
//! a domain-separated SHA-256 hash chain over the database's recorded history, so
//! that any post-hoc alteration of stored versions — editing a byte of a version,
//! deleting/reordering/inserting a transaction, truncating the tail — is
//! detectable, and (against an exported anchor) rollback and fork are provable.
//!
//! # Layers (this module is the pure core; no database wiring)
//!
//! - [`config`] — [`ChainConfig`] / [`ChainFsyncMode`], opt-in and disabled by
//!   default.
//! - [`canonical`] — the injective, deterministic canonical encoder over a
//!   normalized [`VersionHashInput`], plus the folding primitives
//!   ([`version_leaf`], [`tx_digest`], [`chain_step`], [`genesis_digest`]).
//! - [`record`] — [`ChainTxRecord`] and [`ChainHead`] with binary + serde codecs.
//! - [`store`] — the append-only [`ChainStore`] (header + length-framed records +
//!   head checkpoint), tolerant of a torn trailing record.
//! - [`verify`] — DB-independent verification: [`verify_full`],
//!   [`verify_entity`], and [`verify_against_anchor`] over a [`VersionSource`].
//!
//! The chain unit is a committed transaction. Versions of one transaction share
//! one commit timestamp, which is the grouping and ordering key. Each version is
//! reduced to a leaf hash; the leaves fold into a per-transaction digest; each
//! transaction digest chains onto the previous, seeded by an explicit genesis
//! record (enable LSN + timestamp).

pub mod canonical;
pub mod config;
pub mod engine;
pub mod error;
pub mod record;
pub mod store;
pub mod verify;

pub use canonical::{
    CHAIN_GENESIS_DOMAIN, CHAIN_LEAF_DOMAIN, CHAIN_NODE_DOMAIN, CHAIN_TX_DOMAIN, EntityKind,
    ProvenanceHashInput, VersionHashInput, chain_step, from_hex_32, genesis_digest, to_hex,
    tx_digest, version_leaf,
};
pub use config::{ChainConfig, ChainFsyncMode};
pub use engine::{LastVerified, PendingTx, ProvenanceChain};
pub use error::{ChainError, ChainResult};
pub use record::{ChainHead, ChainTxRecord};
pub use store::ChainStore;
pub use verify::{
    ChainVerification, EntityIndex, VersionSource, verify_against_anchor, verify_entity,
    verify_full,
};

#[cfg(test)]
mod integration_tests {
    //! End-to-end core flow: build a chain, persist it, reload, verify.
    use super::*;
    use crate::core::property::PropertyValue;
    use std::collections::HashMap;

    struct MapSource(HashMap<(EntityKind, u64, u64), VersionHashInput>);
    impl VersionSource for MapSource {
        fn fetch(&self, kind: EntityKind, id: u64, version_id: u64) -> Option<VersionHashInput> {
            self.0.get(&(kind, id, version_id)).cloned()
        }
    }

    fn version(id: u64, vid: u64, name: &str) -> VersionHashInput {
        VersionHashInput {
            entity_kind: EntityKind::Node,
            entity_id: id,
            version_id: vid,
            prev_version_id: None,
            label: "Person".to_string(),
            source: None,
            target: None,
            valid_from: Some(10),
            valid_to: None,
            transaction_from: Some(10),
            transaction_to: None,
            is_current: true,
            is_tombstone: false,
            provenance: None,
            properties: vec![("name".to_string(), PropertyValue::string(name))],
        }
    }

    #[test]
    fn persist_reload_and_verify_full_chain() {
        let dir = tempfile::tempdir().unwrap();
        let genesis = ChainHead::genesis(1, 0);
        let mut src = MapSource(HashMap::new());

        // Seal three transactions.
        let store = ChainStore::open(dir.path(), ChainFsyncMode::PerTransaction).unwrap();
        let mut prev = genesis.digest;
        for seq in 1..=3u64 {
            let v = version(seq, 1, &format!("n{seq}"));
            src.0
                .insert((v.entity_kind, v.entity_id, v.version_id), v.clone());
            let leaf = version_leaf(&v);
            let txd = tx_digest(1000 + seq as i64, 0, &[leaf]);
            let digest = chain_step(&prev, &txd);
            let rec = ChainTxRecord {
                seq,
                commit_ts: 1000 + seq as i64,
                commit_ts_logical: 0,
                tx_id: seq,
                anchor_lsn: seq,
                leaves: vec![leaf],
                entity_refs: vec![(EntityKind::Node, seq, 1)],
                digest,
            };
            store.append(&rec).unwrap();
            prev = digest;
        }
        // Checkpoint the head.
        let head = ChainHead {
            seq: 3,
            digest: prev,
            commit_ts: 1003,
            anchor_lsn: 3,
            genesis_digest: genesis.genesis_digest,
        };
        store.write_head_checkpoint(&head).unwrap();

        // Reopen, reload, verify.
        let store2 = ChainStore::open(dir.path(), ChainFsyncMode::PerTransaction).unwrap();
        let records = store2.load().unwrap();
        assert_eq!(records.len(), 3);
        let checkpoint = store2.read_head_checkpoint().unwrap().unwrap();
        assert_eq!(checkpoint, head);

        let result = verify_full(&records, &src, &genesis);
        assert!(result.passed, "reason: {:?}", result.reason);
        assert_eq!(result.head_seq, 3);
        assert_eq!(result.head_digest_hex, to_hex(&head.digest));

        // The reloaded head extends the exported anchor.
        assert!(verify_against_anchor(&records, &genesis, &checkpoint).passed);
    }
}
