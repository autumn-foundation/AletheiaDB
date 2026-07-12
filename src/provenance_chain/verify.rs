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

    /// Run `f` against a fetch view for the duration of one verification pass,
    /// giving a lock-backed source the chance to acquire its underlying shared
    /// lock **once** for the whole pass instead of re-locking on every
    /// [`fetch`](Self::fetch) (Issue #3351 AC4 / task #2).
    ///
    /// The default implementation simply runs `f` against `self` — correct for
    /// any source whose `fetch` is already cheap/lock-free (e.g. an in-memory
    /// test map). A source that guards real storage behind a `RwLock` overrides
    /// this to hold its read guard across `f`, so a full/entity verify takes the
    /// read lock a single time rather than once per version.
    fn scoped(&self, f: &mut dyn FnMut(&dyn VersionSource)) {
        // Object-safe self-reborrow: `&Self` cannot coerce to `&dyn` for an
        // unsized `Self`, so wrap it in a `Sized` adapter that delegates `fetch`.
        struct Reborrow<'a, S: ?Sized>(&'a S);
        impl<S: VersionSource + ?Sized> VersionSource for Reborrow<'_, S> {
            fn fetch(
                &self,
                kind: EntityKind,
                id: u64,
                version_id: u64,
            ) -> Option<VersionHashInput> {
                self.0.fetch(kind, id, version_id)
            }
            // `Reborrow` is `Sized`, so `&self` coerces to `&dyn` directly here —
            // overriding avoids the default rebuilding `Reborrow<Reborrow<..>>`.
            fn scoped(&self, f: &mut dyn FnMut(&dyn VersionSource)) {
                f(self);
            }
        }
        f(&Reborrow(self));
    }
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
///
/// Routes through [`VersionSource::scoped`] so a lock-backed source (the live
/// historical store) holds its read lock **once** for the whole pass rather than
/// re-acquiring it per version (Issue #3351 task #2).
#[must_use]
pub fn verify_full(
    records: &[ChainTxRecord],
    src: &dyn VersionSource,
    genesis: &ChainHead,
) -> ChainVerification {
    let mut out: Option<ChainVerification> = None;
    src.scoped(&mut |s| out = Some(verify_full_inner(records, s, genesis)));
    out.expect("VersionSource::scoped must invoke its callback exactly once")
}

fn verify_full_inner(
    records: &[ChainTxRecord],
    src: &dyn VersionSource,
    genesis: &ChainHead,
) -> ChainVerification {
    // Fold in canonical commit-timestamp order (Issue #3351 finding 4); records
    // reach here already sorted, but sort a local copy defensively so the fold is
    // order-independent.
    let mut ordered: Vec<&ChainTxRecord> = records.iter().collect();
    ordered.sort_by(|a, b| {
        (a.commit_ts, a.commit_ts_logical, &a.leaves).cmp(&(
            b.commit_ts,
            b.commit_ts_logical,
            &b.leaves,
        ))
    });

    let mut prev = genesis.digest;
    let mut checked: u64 = 0;

    for rec in &ordered {
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
        let txd = tx_digest(rec.commit_ts, rec.commit_ts_logical, &recomputed_leaves);
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

    // Structural timeline-consistency check (Issue #3351 finding 2b). Runs after
    // the fold so a leaf/digest tamper is reported first.
    if let Some(broken) = check_timeline_consistency(&ordered, src) {
        return broken;
    }

    let head_seq = ordered.last().map(|r| r.seq).unwrap_or(genesis.seq);
    ChainVerification {
        passed: true,
        head_seq,
        head_digest_hex: super::canonical::to_hex(&prev),
        earliest_broken_seq: None,
        reason: None,
        transactions_checked: checked,
    }
}

/// Per-entity bi-temporal timeline-consistency check (Issue #3351 finding 2b).
///
/// The leaf hash binds the interval **starts**, the tombstone flag, and — for
/// born-closed terminal versions — the valid-time **end** (see
/// `db::chain_source::normalize_immutable`), but a *live* version's ends are
/// mutated by later supersession and so are not directly hashable. This check
/// catches structural interval-END tampering that the leaf hash cannot:
///
/// **What it catches:** per entity, the versions ordered by transaction-time
/// start must have non-decreasing transaction starts and each version's own
/// interval must be well-formed (`valid_from <= valid_to`, `tx_from <= tx_to`).
/// A tombstone/retraction whose end was widened past a well-formedness bound, or
/// a version whose recorded transaction start was moved before an earlier one,
/// is flagged.
///
/// **What it does NOT catch:** it cannot, on its own, detect an interval-end
/// edit that keeps every per-version interval individually well-formed and does
/// not reorder transaction starts (e.g. extending a *terminal* retraction's
/// `valid_to` to a still-valid future point when nothing follows it) — that case
/// is covered instead by the leaf hash binding the born-closed terminal
/// `valid_to` (finding 2c). The two mechanisms are complementary.
fn check_timeline_consistency(
    ordered: &[&ChainTxRecord],
    src: &dyn VersionSource,
) -> Option<ChainVerification> {
    use std::collections::BTreeMap;

    // Collect (tx_from, valid_from, valid_to, tx_to) per entity from the source.
    struct Iv {
        seq: u64,
        valid_from: Option<i64>,
        valid_to: Option<i64>,
        tx_from: Option<i64>,
        tx_to: Option<i64>,
    }
    let mut by_entity: BTreeMap<(u8, u64), Vec<Iv>> = BTreeMap::new();
    for rec in ordered {
        for (kind, id, vid) in &rec.entity_refs {
            if let Some(input) = src.fetch(*kind, *id, *vid) {
                by_entity.entry((kind.tag(), *id)).or_default().push(Iv {
                    seq: rec.seq,
                    valid_from: input.valid_from,
                    valid_to: input.valid_to,
                    tx_from: input.transaction_from,
                    tx_to: input.transaction_to,
                });
            }
        }
    }

    for ivs in by_entity.values() {
        let mut prev_tx_from: Option<i64> = None;
        for iv in ivs {
            // Each version's own interval must be well-formed.
            if let (Some(vf), Some(vt)) = (iv.valid_from, iv.valid_to)
                && vt < vf
            {
                return Some(ChainVerification::broken(
                    iv.seq.saturating_sub(1),
                    String::new(),
                    iv.seq,
                    "timeline inconsistency: valid_to precedes valid_from".to_string(),
                    0,
                ));
            }
            if let (Some(tf), Some(tt)) = (iv.tx_from, iv.tx_to)
                && tt < tf
            {
                return Some(ChainVerification::broken(
                    iv.seq.saturating_sub(1),
                    String::new(),
                    iv.seq,
                    "timeline inconsistency: transaction_to precedes transaction_from".to_string(),
                    0,
                ));
            }
            // Transaction starts are monotonic in fold order for one entity.
            if let (Some(prev), Some(cur)) = (prev_tx_from, iv.tx_from)
                && cur < prev
            {
                return Some(ChainVerification::broken(
                    iv.seq.saturating_sub(1),
                    String::new(),
                    iv.seq,
                    "timeline inconsistency: transaction start regressed for entity".to_string(),
                    0,
                ));
            }
            prev_tx_from = iv.tx_from.or(prev_tx_from);
        }
    }
    None
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

    /// Extend the index with one newly-appended record at `record_idx`, keeping
    /// a running index in lockstep with the sealed log (Issue #3351 engine).
    /// `record_idx` must be the record's position in the `records` slice a later
    /// [`verify_entity`] will be called against.
    pub fn push_record(&mut self, record_idx: usize, record: &ChainTxRecord) {
        for (leaf_pos, (kind, id, version_id)) in record.entity_refs.iter().enumerate() {
            self.inner
                .entry((*kind, *id))
                .or_default()
                .push(EntityOccurrence {
                    record_idx,
                    leaf_pos,
                    version_id: *version_id,
                });
        }
    }
}

/// Entity-scoped verification: recompute only `entity`'s leaves via `src` and
/// confirm, for each touching record, that the sealed leaf matches and the
/// record's own digest folds consistently from its (stored) predecessor.
///
/// No full scan of the chain — only the records that reference `entity`.
///
/// Like [`verify_full`], routes through [`VersionSource::scoped`] so the live
/// historical store is locked once for the (already entity-scoped) pass.
#[must_use]
pub fn verify_entity(
    records: &[ChainTxRecord],
    index: &EntityIndex,
    entity: (EntityKind, u64),
    genesis: &ChainHead,
    src: &dyn VersionSource,
) -> ChainVerification {
    let mut out: Option<ChainVerification> = None;
    src.scoped(&mut |s| out = Some(verify_entity_inner(records, index, entity, genesis, s)));
    out.expect("VersionSource::scoped must invoke its callback exactly once")
}

fn verify_entity_inner(
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
        let txd = tx_digest(rec.commit_ts, rec.commit_ts_logical, &rec.leaves);
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

/// Prove the current chain append-only-extends a previously exported anchor
/// (Issue #3351 finding 3).
///
/// # Why this RE-FOLDS instead of trusting the stored digest
///
/// A prior version merely looked up the record at `anchor.seq` and compared its
/// stored `digest` field to `anchor.digest`. That is forgeable: an attacker who
/// knows the (offsite) anchor digest can fabricate a `chain.log` that simply
/// carries `anchor.digest` in the `digest` field of the record at `anchor.seq`
/// while every leaf/fold underneath is bogus — and it would pass. Instead this
/// **recomputes the chain from genesis** up to `anchor.seq`: it re-folds each
/// record's `tx_digest`/`chain_step` from the sealed leaves, requires the
/// recomputed digest at `anchor.seq` to equal `anchor.digest`, and requires the
/// chain's own genesis digest to equal the anchor's `genesis_digest`. Only then
/// is the equality trustworthy. It then confirms the current chain extends that
/// verified prefix (no fork past the anchor).
///
/// Note this re-folds the log's own leaves (proving the log is internally sound
/// and genuinely descends from the anchored genesis); it does not re-fetch
/// version content — pair with [`verify_full`] for on-disk version tamper.
#[must_use]
pub fn verify_against_anchor(
    records: &[ChainTxRecord],
    genesis: &ChainHead,
    anchor: &ChainHead,
) -> ChainVerification {
    // Fold in canonical order (defensive; records arrive sorted).
    let mut ordered: Vec<&ChainTxRecord> = records.iter().collect();
    ordered.sort_by(|a, b| {
        (a.commit_ts, a.commit_ts_logical, &a.leaves).cmp(&(
            b.commit_ts,
            b.commit_ts_logical,
            &b.leaves,
        ))
    });

    let head_seq = ordered.last().map(|r| r.seq).unwrap_or(0);
    let head_digest_hex =
        super::canonical::to_hex(&ordered.last().map(|r| r.digest).unwrap_or(anchor.digest));

    // The anchor must descend from the same genesis this chain was seeded with;
    // otherwise the comparison below would be against an unrelated chain.
    if anchor.genesis_digest != genesis.genesis_digest {
        return ChainVerification::broken(
            head_seq,
            head_digest_hex,
            anchor.seq,
            "anchor genesis digest does not match this chain's genesis (unrelated chain)"
                .to_string(),
            0,
        );
    }

    // Re-fold from genesis, checking each record's stored digest against the
    // recomputed fold, and capture the recomputed digest at `anchor.seq`.
    let mut prev = genesis.digest;
    let mut recomputed_at_anchor: Option<[u8; 32]> = None;
    let mut checked: u64 = 0;
    for rec in &ordered {
        let txd = tx_digest(rec.commit_ts, rec.commit_ts_logical, &rec.leaves);
        let step = chain_step(&prev, &txd);
        if step != rec.digest {
            // The log is internally inconsistent — a forged/tampered record.
            return ChainVerification::broken(
                rec.seq.saturating_sub(1),
                super::canonical::to_hex(&prev),
                rec.seq,
                "recomputed chain digest differs from stored digest (forged/tampered log)"
                    .to_string(),
                checked,
            );
        }
        if rec.seq == anchor.seq {
            recomputed_at_anchor = Some(step);
        }
        prev = rec.digest;
        checked += 1;
        if rec.seq == anchor.seq {
            // We have re-folded the whole prefix up to the anchor; stop verifying
            // the prefix (records beyond it are the extension).
            break;
        }
    }

    // A genesis anchor (seq 0) is extended by any chain that shares its genesis
    // (already checked above).
    if anchor.seq == 0 {
        return ChainVerification {
            passed: true,
            head_seq,
            head_digest_hex,
            earliest_broken_seq: None,
            reason: None,
            transactions_checked: checked,
        };
    }

    match recomputed_at_anchor {
        None => ChainVerification::broken(
            head_seq,
            head_digest_hex,
            anchor.seq,
            format!(
                "chain truncated: no record at anchor seq {} (rollback)",
                anchor.seq
            ),
            checked,
        ),
        Some(d) if d == anchor.digest => ChainVerification {
            passed: true,
            head_seq,
            head_digest_hex,
            earliest_broken_seq: None,
            reason: None,
            transactions_checked: checked,
        },
        Some(_) => ChainVerification::broken(
            head_seq,
            head_digest_hex,
            anchor.seq,
            format!(
                "chain diverged: recomputed digest at seq {} differs from anchor (fork)",
                anchor.seq
            ),
            checked,
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
            label: "Person".to_string(),
            source: None,
            target: None,
            valid_from: Some(100),
            valid_to: None,
            transaction_from: Some(100),
            transaction_to: None,
            is_current: true,
            is_tombstone: false,
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
            let commit_ts_logical = 0u32;
            let tx_id = seq;
            let txd = tx_digest(commit_ts, commit_ts_logical, &[leaf]);
            let digest = chain_step(&prev, &txd);
            records.push(ChainTxRecord {
                seq,
                commit_ts,
                commit_ts_logical,
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

    /// AC1 (reorder): moving a transaction to a different point in the timeline
    /// changes the head digest and is caught + localized. We swap two records'
    /// commit timestamps (their timeline position) while leaving the sealed
    /// digests untouched: the fold re-orders by commit timestamp, recomputes each
    /// `tx_digest` (which binds the commit timestamp), and no longer reproduces
    /// the sealed digests.
    #[test]
    fn verify_full_detects_reordered_transactions() {
        let (mut records, src, genesis) = build_chain(&[(1, 1, "a"), (2, 1, "b"), (3, 1, "c")]);
        let clean_head = records.last().unwrap().digest;

        // Swap the commit timestamps of tx 1 and tx 2 — a timeline reorder.
        let ts0 = records[0].commit_ts;
        records[0].commit_ts = records[1].commit_ts;
        records[1].commit_ts = ts0;

        let v = verify_full(&records, &src, &genesis);
        assert!(!v.passed, "a transaction reorder must be detected");
        assert!(
            v.earliest_broken_seq.is_some(),
            "reorder tamper is localized"
        );
        // The reorder necessarily changes the canonical head digest.
        assert_ne!(
            v.head_digest_hex,
            crate::provenance_chain::canonical::to_hex(&clean_head),
            "reorder must change the folded head digest"
        );
    }

    /// AC1 (insert): a forged transaction spliced into the chain is caught +
    /// localized. The forged record references a version that stored history does
    /// not contain, so its leaf cannot be reproduced — the fold breaks exactly at
    /// the forged sequence.
    #[test]
    fn verify_full_detects_forged_inserted_transaction() {
        let (mut records, src, genesis) = build_chain(&[(1, 1, "a"), (2, 1, "b"), (3, 1, "c")]);

        // Fabricate a record that sorts between tx 1 and tx 2 but references a
        // version (entity 999) absent from the source.
        let forged = ChainTxRecord {
            seq: 99,
            commit_ts: records[0].commit_ts + 1,
            commit_ts_logical: 0,
            tx_id: 12345,
            anchor_lsn: 42,
            leaves: vec![[0xAAu8; 32]],
            entity_refs: vec![(EntityKind::Node, 999, 999)],
            digest: [0xBBu8; 32],
        };
        records.insert(1, forged);

        let v = verify_full(&records, &src, &genesis);
        assert!(!v.passed, "a forged inserted transaction must be detected");
        assert_eq!(
            v.earliest_broken_seq,
            Some(99),
            "the forged record's sequence is localized"
        );
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
        let (records, _src, genesis) = build_chain(&[(1, 1, "a"), (2, 1, "b"), (3, 1, "c")]);
        let anchor = ChainHead {
            seq: 2,
            digest: records[1].digest,
            commit_ts: records[1].commit_ts,
            anchor_lsn: records[1].anchor_lsn,
            genesis_digest: genesis.genesis_digest,
        };
        let v = verify_against_anchor(&records, &genesis, &anchor);
        assert!(v.passed, "reason: {:?}", v.reason);
    }

    #[test]
    fn verify_against_anchor_detects_truncation() {
        let (records, _src, genesis) = build_chain(&[(1, 1, "a"), (2, 1, "b"), (3, 1, "c")]);
        let anchor = ChainHead {
            seq: 3,
            digest: records[2].digest,
            commit_ts: records[2].commit_ts,
            anchor_lsn: records[2].anchor_lsn,
            genesis_digest: genesis.genesis_digest,
        };
        // Roll back to only 2 records.
        let truncated = &records[..2];
        let v = verify_against_anchor(truncated, &genesis, &anchor);
        assert!(!v.passed);
        assert_eq!(v.earliest_broken_seq, Some(3));
        assert!(v.reason.unwrap().contains("rollback"));
    }

    #[test]
    fn verify_against_anchor_detects_fork() {
        let (records, _src, genesis) = build_chain(&[(1, 1, "a"), (2, 1, "b"), (3, 1, "c")]);
        let anchor = ChainHead {
            seq: 2,
            // A digest that does not match the real record at seq 2.
            digest: [0x11u8; 32],
            commit_ts: records[1].commit_ts,
            anchor_lsn: records[1].anchor_lsn,
            genesis_digest: genesis.genesis_digest,
        };
        let v = verify_against_anchor(&records, &genesis, &anchor);
        assert!(!v.passed);
        assert_eq!(v.earliest_broken_seq, Some(2));
        assert!(v.reason.unwrap().contains("fork"));
    }

    /// Issue #3351 finding 3: a forged log whose record at `anchor.seq` carries
    /// the known anchor digest but whose fold is broken must be REJECTED (the
    /// old compare-stored-digest path accepted it).
    #[test]
    fn verify_against_anchor_rejects_forged_digest_with_broken_fold() {
        let (records, _src, genesis) = build_chain(&[(1, 1, "a"), (2, 1, "b"), (3, 1, "c")]);
        let anchor = ChainHead {
            seq: 2,
            digest: records[1].digest,
            commit_ts: records[1].commit_ts,
            anchor_lsn: records[1].anchor_lsn,
            genesis_digest: genesis.genesis_digest,
        };
        // Forge a log: keep the anchor digest at seq 2, but corrupt the leaves so
        // the fold no longer produces that digest.
        let mut forged = records.clone();
        forged[1].leaves[0][0] ^= 0xFF; // break the fold under the anchored digest
        let v = verify_against_anchor(&forged, &genesis, &anchor);
        assert!(!v.passed, "forged log must be rejected");
        assert!(
            v.reason.as_deref().unwrap().contains("forged")
                || v.reason.as_deref().unwrap().contains("tampered")
        );
    }
}
