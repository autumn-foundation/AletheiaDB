//! Verification of a provenance hash chain, independent of the database (Issue #3351).
//!
//! Verification recomputes leaves and the folded chain from a
//! [`VersionSource`] (which reconstructs the authoritative version content) plus
//! the genesis, and compares against the sealed digests:
//! - [`verify_full`] walks the whole chain from genesis and localizes the
//!   **earliest** broken sequence number.
//! - [`verify_entity`] recomputes only one entity's leaves using a per-entity
//!   index, with no full scan.
//! - [`verify_against_anchor`] proves the current chain extends a previously
//!   exported [`ChainHead`], detecting rollback (truncation) and fork
//!   (divergence).

use std::collections::HashMap;

use super::canonical::{EntityKind, VersionHashInput, chain_step, tx_digest, version_leaf};
use super::record::{ChainHead, ChainTxRecord};

/// Reconstructs the authoritative, hashable form of a specific version.
///
/// Implemented against whatever holds the real version content (the historical
/// store at runtime, or a synthetic map in tests). Returning `None` means the
/// referenced version cannot be found, which fails verification.
pub trait VersionSource {
    /// Fetch the normalized hash input for `(kind, id, version_id)`.
    fn fetch(&self, kind: EntityKind, id: u64, version_id: u64) -> Option<VersionHashInput>;
}

/// Outcome of a chain verification pass.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct ChainVerification {
    /// Whether the chain verified cleanly.
    pub passed: bool,
    /// Sequence number of the verified head (last checked record, or the
    /// genesis/anchor seq when nothing was checked).
    pub head_seq: u64,
    /// Hex digest of the verified head.
    pub head_digest_hex: String,
    /// The earliest sequence number that failed, if any (localizes tamper).
    pub earliest_broken_seq: Option<u64>,
    /// Human-readable reason for a failure.
    pub reason: Option<String>,
    /// Number of transactions actually checked.
    pub transactions_checked: u64,
}

impl ChainVerification {
    fn broken(
        head_seq: u64,
        head_digest_hex: String,
        seq: u64,
        reason: impl Into<String>,
        checked: u64,
    ) -> Self {
        ChainVerification {
            passed: false,
            head_seq,
            head_digest_hex,
            earliest_broken_seq: Some(seq),
            reason: Some(reason.into()),
            transactions_checked: checked,
        }
    }
}

/// Full-chain verification: recompute every leaf from `src` and re-fold from
/// `genesis`, returning the earliest broken sequence number on tamper.
#[must_use]
pub fn verify_full(
    records: &[ChainTxRecord],
    src: &dyn VersionSource,
    genesis: &ChainHead,
) -> ChainVerification {
    let mut prev = genesis.digest;
    let mut checked: u64 = 0;

    for rec in records {
        // Recompute each leaf from the authoritative source.
        let mut recomputed_leaves = Vec::with_capacity(rec.entity_refs.len());
        for (kind, id, vid) in &rec.entity_refs {
            match src.fetch(*kind, *id, *vid) {
                Some(input) => recomputed_leaves.push(version_leaf(&input)),
                None => {
                    return ChainVerification::broken(
                        rec.seq.saturating_sub(1),
                        super::canonical::to_hex(&prev),
                        rec.seq,
                        format!("version {vid} for entity {id} not found in source"),
                        checked,
                    );
                }
            }
        }

        // The stored leaves must match what the source produces (localizes a
        // single-version tamper precisely).
        if recomputed_leaves != rec.leaves {
            return ChainVerification::broken(
                rec.seq.saturating_sub(1),
                super::canonical::to_hex(&prev),
                rec.seq,
                "recomputed leaf(s) differ from sealed leaves".to_string(),
                checked,
            );
        }

        // Recompute the tx digest and fold; compare to the sealed digest.
        let txd = tx_digest(rec.commit_ts, rec.tx_id, &recomputed_leaves);
        let step = chain_step(&prev, &txd);
        if step != rec.digest {
            return ChainVerification::broken(
                rec.seq.saturating_sub(1),
                super::canonical::to_hex(&prev),
                rec.seq,
                "recomputed chain digest differs from sealed digest".to_string(),
                checked,
            );
        }

        prev = rec.digest;
        checked += 1;
    }

    let head_seq = records.last().map(|r| r.seq).unwrap_or(genesis.seq);
    ChainVerification {
        passed: true,
        head_seq,
        head_digest_hex: super::canonical::to_hex(&prev),
        earliest_broken_seq: None,
        reason: None,
        transactions_checked: checked,
    }
}

/// Maps an entity identity to every `(record index, leaf position, version id)`
/// where it appears — enabling entity-scoped verification without a full scan.
#[derive(Debug, Default)]
pub struct EntityIndex {
    inner: HashMap<(EntityKind, u64), Vec<EntityOccurrence>>,
}

#[derive(Debug, Clone, Copy)]
struct EntityOccurrence {
    record_idx: usize,
    leaf_pos: usize,
    version_id: u64,
}

impl EntityIndex {
    /// Build the index by scanning the records once (the only full pass; later
    /// entity verifications reuse it).
    #[must_use]
    pub fn build(records: &[ChainTxRecord]) -> Self {
        let mut inner: HashMap<(EntityKind, u64), Vec<EntityOccurrence>> = HashMap::new();
        for (record_idx, rec) in records.iter().enumerate() {
            for (leaf_pos, (kind, id, version_id)) in rec.entity_refs.iter().enumerate() {
                inner
                    .entry((*kind, *id))
                    .or_default()
                    .push(EntityOccurrence {
                        record_idx,
                        leaf_pos,
                        version_id: *version_id,
                    });
            }
        }
        EntityIndex { inner }
    }
}

/// Entity-scoped verification: recompute only `entity`'s leaves via `src` and
/// confirm, for each touching record, that the sealed leaf matches and the
/// record's own digest folds consistently from its (stored) predecessor.
///
/// No full scan of the chain — only the records that reference `entity`.
#[must_use]
pub fn verify_entity(
    records: &[ChainTxRecord],
    index: &EntityIndex,
    entity: (EntityKind, u64),
    genesis: &ChainHead,
    src: &dyn VersionSource,
) -> ChainVerification {
    let occurrences = match index.inner.get(&entity) {
        Some(o) => o,
        None => {
            // Entity absent from the chain: trivially consistent (nothing to check).
            let head_seq = records.last().map(|r| r.seq).unwrap_or(genesis.seq);
            return ChainVerification {
                passed: true,
                head_seq,
                head_digest_hex: String::new(),
                earliest_broken_seq: None,
                reason: Some("entity not present in chain".to_string()),
                transactions_checked: 0,
            };
        }
    };

    let mut checked: u64 = 0;
    for occ in occurrences {
        let rec = &records[occ.record_idx];

        // Recompute just this entity's leaf.
        let input = match src.fetch(entity.0, entity.1, occ.version_id) {
            Some(i) => i,
            None => {
                return ChainVerification::broken(
                    rec.seq.saturating_sub(1),
                    String::new(),
                    rec.seq,
                    format!("version {} not found in source", occ.version_id),
                    checked,
                );
            }
        };
        let recomputed_leaf = version_leaf(&input);
        if recomputed_leaf != rec.leaves[occ.leaf_pos] {
            return ChainVerification::broken(
                rec.seq.saturating_sub(1),
                String::new(),
                rec.seq,
                "entity leaf differs from sealed leaf".to_string(),
                checked,
            );
        }

        // Confirm the record's digest folds from its stored predecessor. This is
        // a local check using neighbor digests, not a full re-fold.
        let prev = if occ.record_idx == 0 {
            genesis.digest
        } else {
            records[occ.record_idx - 1].digest
        };
        let txd = tx_digest(rec.commit_ts, rec.tx_id, &rec.leaves);
        if chain_step(&prev, &txd) != rec.digest {
            return ChainVerification::broken(
                rec.seq.saturating_sub(1),
                String::new(),
                rec.seq,
                "record digest inconsistent with stored predecessor".to_string(),
                checked,
            );
        }
        checked += 1;
    }

    let head_seq = records.last().map(|r| r.seq).unwrap_or(genesis.seq);
    ChainVerification {
        passed: true,
        head_seq,
        head_digest_hex: super::canonical::to_hex(
            &records.last().map(|r| r.digest).unwrap_or(genesis.digest),
        ),
        earliest_broken_seq: None,
        reason: None,
        transactions_checked: checked,
    }
}

/// Prove the current chain extends a previously exported anchor: the record at
/// `anchor.seq` must exist and match `anchor.digest`. A missing seq means the
/// chain was truncated (rollback); a mismatching digest means it diverged (fork).
#[must_use]
pub fn verify_against_anchor(records: &[ChainTxRecord], anchor: &ChainHead) -> ChainVerification {
    let head_seq = records.last().map(|r| r.seq).unwrap_or(0);
    let head_digest_hex =
        super::canonical::to_hex(&records.last().map(|r| r.digest).unwrap_or(anchor.digest));

    // A genesis anchor (seq 0) is extended by any chain; the digest can only be
    // checked once real records exist, which they always do here.
    if anchor.seq == 0 {
        return ChainVerification {
            passed: true,
            head_seq,
            head_digest_hex,
            earliest_broken_seq: None,
            reason: None,
            transactions_checked: 0,
        };
    }

    match records.iter().find(|r| r.seq == anchor.seq) {
        None => ChainVerification::broken(
            head_seq,
            head_digest_hex,
            anchor.seq,
            format!(
                "chain truncated: no record at anchor seq {} (rollback)",
                anchor.seq
            ),
            0,
        ),
        Some(rec) if rec.digest == anchor.digest => ChainVerification {
            passed: true,
            head_seq,
            head_digest_hex,
            earliest_broken_seq: None,
            reason: None,
            transactions_checked: 1,
        },
        Some(_) => ChainVerification::broken(
            head_seq,
            head_digest_hex,
            anchor.seq,
            format!(
                "chain diverged: digest at seq {} differs from anchor (fork)",
                anchor.seq
            ),
            0,
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::property::PropertyValue;
    use std::collections::HashMap;

    /// A synthetic in-memory version source keyed by (kind, id, version_id).
    #[derive(Default)]
    struct MapSource {
        map: HashMap<(EntityKind, u64, u64), VersionHashInput>,
    }

    impl MapSource {
        fn insert(&mut self, input: VersionHashInput) {
            self.map.insert(
                (input.entity_kind, input.entity_id, input.version_id),
                input,
            );
        }
    }

    impl VersionSource for MapSource {
        fn fetch(&self, kind: EntityKind, id: u64, version_id: u64) -> Option<VersionHashInput> {
            self.map.get(&(kind, id, version_id)).cloned()
        }
    }

    fn input(id: u64, vid: u64, name: &str) -> VersionHashInput {
        VersionHashInput {
            entity_kind: EntityKind::Node,
            entity_id: id,
            version_id: vid,
            prev_version_id: None,
            valid_from: Some(100),
            valid_to: None,
            transaction_from: Some(100),
            transaction_to: None,
            is_current: true,
            provenance: None,
            properties: vec![("name".to_string(), PropertyValue::string(name))],
        }
    }

    /// Build a clean chain of N single-version transactions plus a source and genesis.
    fn build_chain(specs: &[(u64, u64, &str)]) -> (Vec<ChainTxRecord>, MapSource, ChainHead) {
        let genesis = ChainHead::genesis(1, 0);
        let mut src = MapSource::default();
        let mut records = Vec::new();
        let mut prev = genesis.digest;
        for (i, (id, vid, name)) in specs.iter().enumerate() {
            let vi = input(*id, *vid, name);
            src.insert(vi.clone());
            let leaf = version_leaf(&vi);
            let seq = (i + 1) as u64;
            let commit_ts = 1000 + seq as i64;
            let tx_id = seq;
            let txd = tx_digest(commit_ts, tx_id, &[leaf]);
            let digest = chain_step(&prev, &txd);
            records.push(ChainTxRecord {
                seq,
                commit_ts,
                tx_id,
                anchor_lsn: seq,
                leaves: vec![leaf],
                entity_refs: vec![(EntityKind::Node, *id, *vid)],
                digest,
            });
            prev = digest;
        }
        (records, src, genesis)
    }

    #[test]
    fn verify_full_passes_on_clean_chain() {
        let (records, src, genesis) = build_chain(&[(1, 1, "a"), (2, 1, "b"), (3, 1, "c")]);
        let v = verify_full(&records, &src, &genesis);
        assert!(v.passed, "reason: {:?}", v.reason);
        assert_eq!(v.transactions_checked, 3);
        assert_eq!(v.head_seq, 3);
        assert!(v.earliest_broken_seq.is_none());
    }

    #[test]
    fn verify_full_localizes_a_mutated_version() {
        let (records, mut src, genesis) = build_chain(&[(1, 1, "a"), (2, 1, "b"), (3, 1, "c")]);
        // Tamper the stored version content of the entity in tx 2.
        src.insert(input(2, 1, "TAMPERED"));
        let v = verify_full(&records, &src, &genesis);
        assert!(!v.passed);
        assert_eq!(v.earliest_broken_seq, Some(2));
    }

    #[test]
    fn verify_full_detects_deleted_transaction() {
        let (mut records, src, genesis) = build_chain(&[(1, 1, "a"), (2, 1, "b"), (3, 1, "c")]);
        // Delete the middle transaction; the fold breaks at what is now seq 3.
        records.remove(1);
        let v = verify_full(&records, &src, &genesis);
        assert!(!v.passed);
        // seq 1 still folds from genesis; the break is at the record after it.
        assert_eq!(v.earliest_broken_seq, Some(3));
    }

    #[test]
    fn verify_full_detects_single_mutated_leaf() {
        let (mut records, src, genesis) = build_chain(&[(1, 1, "a"), (2, 1, "b")]);
        // Flip a byte of a sealed leaf.
        records[0].leaves[0][0] ^= 0xFF;
        let v = verify_full(&records, &src, &genesis);
        assert!(!v.passed);
        assert_eq!(v.earliest_broken_seq, Some(1));
    }

    #[test]
    fn verify_entity_passes_clean_without_full_scan() {
        let (records, src, genesis) = build_chain(&[(1, 1, "a"), (2, 1, "b"), (1, 2, "a2")]);
        let index = EntityIndex::build(&records);
        let v = verify_entity(&records, &index, (EntityKind::Node, 1), &genesis, &src);
        assert!(v.passed, "reason: {:?}", v.reason);
        // Entity 1 appears in tx 1 and tx 3 only.
        assert_eq!(v.transactions_checked, 2);
    }

    #[test]
    fn verify_entity_catches_mutation() {
        let (records, mut src, genesis) = build_chain(&[(1, 1, "a"), (2, 1, "b"), (1, 2, "a2")]);
        let index = EntityIndex::build(&records);
        src.insert(input(1, 2, "HACKED"));
        let v = verify_entity(&records, &index, (EntityKind::Node, 1), &genesis, &src);
        assert!(!v.passed);
        assert_eq!(v.earliest_broken_seq, Some(3));
    }

    #[test]
    fn verify_against_anchor_passes_on_extension() {
        let (records, _src, _genesis) = build_chain(&[(1, 1, "a"), (2, 1, "b"), (3, 1, "c")]);
        let anchor = ChainHead {
            seq: 2,
            digest: records[1].digest,
            commit_ts: records[1].commit_ts,
            anchor_lsn: records[1].anchor_lsn,
            genesis_digest: [0u8; 32],
        };
        let v = verify_against_anchor(&records, &anchor);
        assert!(v.passed, "reason: {:?}", v.reason);
    }

    #[test]
    fn verify_against_anchor_detects_truncation() {
        let (records, _src, _genesis) = build_chain(&[(1, 1, "a"), (2, 1, "b"), (3, 1, "c")]);
        let anchor = ChainHead {
            seq: 3,
            digest: records[2].digest,
            commit_ts: records[2].commit_ts,
            anchor_lsn: records[2].anchor_lsn,
            genesis_digest: [0u8; 32],
        };
        // Roll back to only 2 records.
        let truncated = &records[..2];
        let v = verify_against_anchor(truncated, &anchor);
        assert!(!v.passed);
        assert_eq!(v.earliest_broken_seq, Some(3));
        assert!(v.reason.unwrap().contains("rollback"));
    }

    #[test]
    fn verify_against_anchor_detects_fork() {
        let (records, _src, _genesis) = build_chain(&[(1, 1, "a"), (2, 1, "b"), (3, 1, "c")]);
        let anchor = ChainHead {
            seq: 2,
            // A digest that does not match the real record at seq 2.
            digest: [0x11u8; 32],
            commit_ts: records[1].commit_ts,
            anchor_lsn: records[1].anchor_lsn,
            genesis_digest: [0u8; 32],
        };
        let v = verify_against_anchor(&records, &anchor);
        assert!(!v.passed);
        assert_eq!(v.earliest_broken_seq, Some(2));
        assert!(v.reason.unwrap().contains("fork"));
    }
}
