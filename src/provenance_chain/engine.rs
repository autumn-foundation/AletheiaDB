//! Runtime engine wiring the provenance hash chain into a live database
//! (Issue #3351).
//!
//! The [`ProvenanceChain`] owns the [`ChainStore`], an in-memory head and
//! running [`EntityIndex`], and a background **sealer** thread. The hot write
//! path only *enqueues* the version refs a committed transaction produced; the
//! sealer reads their authoritative content back from historical storage (via a
//! [`VersionSource`]), folds the per-transaction digest onto the running head,
//! and appends a [`ChainTxRecord`]. Because the seal path and the verify path
//! read the SAME [`VersionSource`], a sealed leaf always reproduces exactly
//! unless the stored version content was tampered with.
//!
//! # Ordering (Issue #3351 finding 4)
//!
//! The chain digest is a deterministic function of the record set **sorted by
//! the full HLC commit timestamp** `(commit_ts, commit_ts_logical)` (tie-broken
//! by the sorted leaves), NOT of enqueue/arrival order. [`Sealer::seal_one`]
//! inserts each new record at its canonical sorted position: the common case is
//! an append at the tail (`O(1)` fold), and a rare out-of-order arrival re-folds
//! only the bounded suffix after the insertion point and rewrites the log so the
//! persisted order stays canonical. Consequently a live-sealed chain and a
//! post-crash rebuilt chain (which folds replayed history in the same sorted
//! order) yield the identical head digest — enqueue order cannot fork them. The
//! sealer's reorder buffer is now only a batching convenience.
//!
//! Correctness never depends on the channel capacity: an enqueue that finds the
//! channel full seals **inline** (drop-to-sync-seal) — which still routes through
//! the same sorted-insert `seal_one`, so it cannot fold out of order — rather
//! than blocking or dropping the transaction, so every committed transaction is
//! always sealed exactly once.
//!
//! # Recovery
//!
//! The sealed prefix lives in the append-only log; the unsealed tail is always
//! re-derivable from durable history. After WAL replay the caller folds any
//! transactions whose commit timestamp is beyond the loaded head back into the
//! chain via [`ProvenanceChain::seal_pending_sync`] before starting the sealer.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, TrySendError, sync_channel};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use super::canonical::{EntityKind, chain_step, tx_digest, version_leaf};
use super::config::{ChainConfig, ChainFsyncMode};
use super::error::{ChainError, ChainResult};
use super::record::{ChainHead, ChainTxRecord};
use super::store::ChainStore;
use super::verify::{
    ChainVerification, EntityIndex, VersionSource, verify_against_anchor, verify_entity,
    verify_full,
};

/// Bounded capacity of the hot-path -> sealer channel. Chosen generously; when
/// it fills the enqueue path seals inline, so this only trades a little memory
/// for a lower inline-seal rate under bursty load. Correctness is independent of
/// this value.
const SEAL_CHANNEL_CAPACITY: usize = 4096;

/// A committed transaction handed from the hot path to the sealer.
///
/// Carries only the version refs the transaction produced (not their content):
/// the sealer resolves authoritative content from the [`VersionSource`], so a
/// leaf is always recomputed from stored history, never from a possibly-stale
/// in-flight buffer.
#[derive(Debug, Clone)]
pub struct PendingTx {
    /// Commit timestamp (micros since epoch) — the primary grouping/ordering key.
    pub commit_ts_micros: i64,
    /// Logical counter of the HLC commit timestamp — the ordering tie-breaker so
    /// two transactions in the same microsecond stay distinct (Issue #3351
    /// finding 4).
    pub commit_ts_logical: u32,
    /// The committing transaction's id. Carried as record metadata only; NOT
    /// folded into the digest (a rebuild cannot recover it).
    pub tx_id: u64,
    /// WAL LSN this transaction is anchored at.
    pub anchor_lsn: u64,
    /// `(kind, entity_id, version_id)` for each version the transaction wrote.
    pub entity_refs: Vec<(EntityKind, u64, u64)>,
}

/// The result of the most recent verification pass, cached for O(1) stats.
#[derive(Debug, Clone, Copy)]
pub struct LastVerified {
    /// Whether the pass verified cleanly.
    pub passed: bool,
    /// Wallclock micros when the pass completed.
    pub at_micros: i64,
}

/// Messages sent to the background sealer thread.
enum SealMsg {
    Seal(PendingTx),
    Stop,
}

/// Shared sealing state: the append-only store, the version source, and the
/// mutable in-memory chain (records + head + running index). Cloned as an
/// `Arc` into the sealer thread and shared with the owning [`ProvenanceChain`].
struct Sealer {
    store: Arc<ChainStore>,
    source: Arc<dyn VersionSource + Send + Sync>,
    genesis: ChainHead,
    inner: Mutex<ChainInner>,
}

/// The mutable in-memory chain state, guarded by a single mutex.
struct ChainInner {
    records: Vec<ChainTxRecord>,
    head: ChainHead,
    index: EntityIndex,
}

impl Sealer {
    /// Seal one transaction: recompute its leaves from the source, insert the
    /// record at its **commit-timestamp-sorted** position, (re-)fold the affected
    /// suffix, persist, and advance the head + entity index.
    ///
    /// # Deterministic, commit-ordered digest (Issue #3351 finding 4)
    ///
    /// The chain digest is a function of the record set **sorted by the full HLC
    /// commit timestamp** `(commit_ts, commit_ts_logical)`, tie-broken by the
    /// (already sorted) leaves — never of arrival order. The common case is a
    /// tail append (`O(1)` fold + one `append`); a rare out-of-order arrival
    /// re-folds the bounded suffix after the insertion point and rewrites the log
    /// so the persisted order stays canonical. This makes a live-sealed chain and
    /// a post-crash rebuilt chain (which folds replayed history in the same
    /// sorted order) produce the identical head digest, eliminating false forks.
    ///
    /// A transaction whose versions cannot be resolved at all (every ref missing
    /// from the source) produces no record — there is nothing to bind.
    fn seal_one(&self, pending: PendingTx) -> ChainResult<()> {
        // Deterministic leaf order, independent of enqueue/buffer order.
        let mut refs = pending.entity_refs;
        refs.sort_by(|a, b| (a.0.tag(), a.1, a.2).cmp(&(b.0.tag(), b.1, b.2)));

        let mut kept_refs: Vec<(EntityKind, u64, u64)> = Vec::with_capacity(refs.len());
        let mut leaves: Vec<[u8; 32]> = Vec::with_capacity(refs.len());
        for (kind, id, vid) in refs {
            if let Some(input) = self.source.fetch(kind, id, vid) {
                leaves.push(version_leaf(&input));
                kept_refs.push((kind, id, vid));
            }
        }
        if leaves.is_empty() {
            return Ok(());
        }

        let mut inner = self
            .inner
            .lock()
            .map_err(|_| ChainError::Io("provenance chain inner lock poisoned".into()))?;

        // Find the sorted insertion position by the canonical commit-order key.
        let idx = inner.records.partition_point(|r| {
            (r.commit_ts, r.commit_ts_logical, &r.leaves)
                < (pending.commit_ts_micros, pending.commit_ts_logical, &leaves)
        });

        // Insert an un-folded shell; `refold_suffix` fills seq + digest.
        inner.records.insert(
            idx,
            ChainTxRecord {
                seq: 0,
                commit_ts: pending.commit_ts_micros,
                commit_ts_logical: pending.commit_ts_logical,
                tx_id: pending.tx_id,
                anchor_lsn: pending.anchor_lsn,
                leaves,
                entity_refs: kept_refs,
                digest: [0u8; 32],
            },
        );

        let is_tail = idx + 1 == inner.records.len();
        self.refold_suffix(&mut inner, idx);

        if is_tail {
            // Common case: append the single new record to the log.
            let rec = inner.records[idx].clone();
            self.store.append(&rec)?;
            inner.index.push_record(idx, &rec);
        } else {
            // Rare out-of-order arrival: the suffix digests/seqs shifted, so
            // rewrite the log in canonical order and rebuild the entity index.
            let snapshot = inner.records.clone();
            self.store.rewrite(&snapshot)?;
            inner.index = EntityIndex::build(&inner.records);
        }

        let last = inner
            .records
            .last()
            .expect("records is non-empty after insert");
        inner.head = ChainHead {
            seq: last.seq,
            digest: last.digest,
            commit_ts: last.commit_ts,
            anchor_lsn: last.anchor_lsn,
            genesis_digest: self.genesis.genesis_digest,
        };
        Ok(())
    }

    /// Recompute `seq` and `digest` for every record from `start` to the tail,
    /// folding each onto its predecessor (genesis for index 0). `records[start]`
    /// and everything after it must already be in canonical order.
    fn refold_suffix(&self, inner: &mut ChainInner, start: usize) {
        for i in start..inner.records.len() {
            let prev = if i == 0 {
                self.genesis.digest
            } else {
                inner.records[i - 1].digest
            };
            let txd = {
                let rec = &inner.records[i];
                tx_digest(rec.commit_ts, rec.commit_ts_logical, &rec.leaves)
            };
            let digest = chain_step(&prev, &txd);
            let rec = &mut inner.records[i];
            rec.seq = (i as u64) + 1;
            rec.digest = digest;
        }
    }

    /// Persist the current head as an atomic checkpoint (carries the genesis
    /// digest, enabling a fast, genesis-stable reopen).
    fn checkpoint(&self) -> ChainResult<()> {
        let head = {
            let inner = self
                .inner
                .lock()
                .map_err(|_| ChainError::Io("provenance chain inner lock poisoned".into()))?;
            inner.head.clone()
        };
        self.store.write_head_checkpoint(&head)
    }
}

/// The database-facing runtime handle for the provenance hash chain.
pub struct ProvenanceChain {
    sealer: Arc<Sealer>,
    genesis: ChainHead,
    sender: SyncSender<SealMsg>,
    /// Receiver held until [`start`](Self::start) spawns the sealer thread.
    pending_rx: Mutex<Option<Receiver<SealMsg>>>,
    /// Set once the chain has been shut down; further enqueues seal inline.
    stopped: AtomicBool,
    handle: Mutex<Option<JoinHandle<()>>>,
    last_verified: Mutex<Option<LastVerified>>,
    fsync: ChainFsyncMode,
}

impl ProvenanceChain {
    /// Open (or create) the chain under `data_dir`, loading any persisted
    /// records and head checkpoint. The sealer thread is NOT started yet — the
    /// caller rebuilds the unsealed tail (via [`seal_pending_sync`]) and then
    /// calls [`start`].
    ///
    /// `genesis_lsn`/`genesis_ts_micros` seed a fresh genesis and are used ONLY
    /// when the store has no prior head checkpoint; an existing chain preserves
    /// its original genesis digest across restarts.
    ///
    /// [`seal_pending_sync`]: Self::seal_pending_sync
    /// [`start`]: Self::start
    pub fn open(
        config: &ChainConfig,
        data_dir: &Path,
        genesis_lsn: u64,
        genesis_ts_micros: i64,
        source: Arc<dyn VersionSource + Send + Sync>,
    ) -> ChainResult<Arc<Self>> {
        let dir = config.resolve_dir(data_dir);
        let store = Arc::new(ChainStore::open(&dir, config.fsync)?);
        let mut records = store.load()?;
        // The digest chain is defined over records in canonical commit-timestamp
        // order (Issue #3351 finding 4). The store persists them so, but sort
        // defensively so the loaded head/fold are canonical regardless.
        records.sort_by(|a, b| {
            (a.commit_ts, a.commit_ts_logical, &a.leaves).cmp(&(
                b.commit_ts,
                b.commit_ts_logical,
                &b.leaves,
            ))
        });

        // Reconstruct the genesis. An existing head checkpoint carries the
        // original genesis digest, so the genesis stays stable across restarts;
        // a fresh store gets a genesis seeded from (lsn, ts) and persisted
        // synchronously so a crash before the first periodic checkpoint still
        // reopens against the same genesis.
        let genesis = match store.read_head_checkpoint()? {
            Some(cp) => ChainHead {
                seq: 0,
                digest: cp.genesis_digest,
                commit_ts: genesis_ts_micros,
                anchor_lsn: genesis_lsn,
                genesis_digest: cp.genesis_digest,
            },
            None => {
                let g = ChainHead::genesis(genesis_lsn, genesis_ts_micros);
                store.write_head_checkpoint(&g)?;
                g
            }
        };

        // The append-only log is the source of truth for the sealed prefix; the
        // in-memory head is the last logged record (or genesis when empty).
        let head = records
            .last()
            .map(|r| ChainHead {
                seq: r.seq,
                digest: r.digest,
                commit_ts: r.commit_ts,
                anchor_lsn: r.anchor_lsn,
                genesis_digest: genesis.genesis_digest,
            })
            .unwrap_or_else(|| genesis.clone());
        let index = EntityIndex::build(&records);

        let sealer = Arc::new(Sealer {
            store,
            source,
            genesis: genesis.clone(),
            inner: Mutex::new(ChainInner {
                records,
                head,
                index,
            }),
        });

        let (sender, rx) = sync_channel(SEAL_CHANNEL_CAPACITY);
        Ok(Arc::new(ProvenanceChain {
            sealer,
            genesis,
            sender,
            pending_rx: Mutex::new(Some(rx)),
            stopped: AtomicBool::new(false),
            handle: Mutex::new(None),
            last_verified: Mutex::new(None),
            fsync: config.fsync,
        }))
    }

    /// Spawn the background sealer thread. Idempotent: a second call is a no-op.
    pub fn start(self: &Arc<Self>) {
        let Some(rx) = self
            .pending_rx
            .lock()
            .ok()
            .and_then(|mut guard| guard.take())
        else {
            return;
        };
        let sealer = Arc::clone(&self.sealer);
        let handle = std::thread::Builder::new()
            .name("aletheia-provenance-sealer".into())
            .spawn(move || run_sealer(sealer, rx))
            .expect("failed to spawn provenance sealer thread");
        if let Ok(mut guard) = self.handle.lock() {
            *guard = Some(handle);
        }
    }

    /// Enqueue a committed transaction for sealing. Non-blocking: when the
    /// channel is full (or the sealer has stopped) the transaction is sealed
    /// inline so it is never lost.
    pub fn enqueue_commit(&self, pending: PendingTx) {
        if self.stopped.load(Ordering::Acquire) {
            let _ = self.sealer.seal_one(pending);
            return;
        }
        match self.sender.try_send(SealMsg::Seal(pending)) {
            Ok(()) => {}
            Err(TrySendError::Full(SealMsg::Seal(p)))
            | Err(TrySendError::Disconnected(SealMsg::Seal(p))) => {
                let _ = self.sealer.seal_one(p);
            }
            Err(_) => {}
        }
    }

    /// Seal one transaction synchronously (used to rebuild the unsealed tail at
    /// startup, before the sealer thread is running).
    pub fn seal_pending_sync(&self, pending: PendingTx) -> ChainResult<()> {
        self.sealer.seal_one(pending)
    }

    /// Persist the current head checkpoint.
    pub fn checkpoint(&self) -> ChainResult<()> {
        self.sealer.checkpoint()
    }

    /// The current in-memory chain head.
    #[must_use]
    pub fn head(&self) -> ChainHead {
        self.sealer
            .inner
            .lock()
            .map(|inner| inner.head.clone())
            .unwrap_or_else(|_| self.genesis.clone())
    }

    /// Export the current head as an external anchor (rollback/fork proof).
    #[must_use]
    pub fn export_head(&self) -> ChainHead {
        self.head()
    }

    /// The genesis head this chain was seeded with.
    #[must_use]
    pub fn genesis(&self) -> ChainHead {
        self.genesis.clone()
    }

    /// The most recent verification result, if any (O(1), for stats).
    #[must_use]
    pub fn last_verified(&self) -> Option<LastVerified> {
        self.last_verified.lock().ok().and_then(|g| *g)
    }

    fn record_verification(&self, result: &ChainVerification) {
        if let Ok(mut g) = self.last_verified.lock() {
            *g = Some(LastVerified {
                passed: result.passed,
                at_micros: crate::core::temporal::time::now().wallclock(),
            });
        }
    }

    /// Full-chain verification: recompute every leaf from stored history and
    /// re-fold from genesis, localizing the earliest broken sequence on tamper.
    #[must_use]
    pub fn verify_full(&self) -> ChainVerification {
        let records = self.snapshot_records();
        let result = verify_full(&records, &*self.sealer.source, &self.genesis);
        self.record_verification(&result);
        result
    }

    /// Entity-scoped verification: recompute only `(kind, id)`'s leaves.
    ///
    /// Truly scan-free (Issue #3351 task #1): instead of cloning every record
    /// and rebuilding the whole [`EntityIndex`] on each call — O(total tx) — this
    /// locks `inner` and hands the core verifier the sealer's **running**
    /// `records` + `index` directly. The core then touches only the records that
    /// reference the entity (leaf recompute is already entity-scoped), so the
    /// whole call is O(entity versions), independent of database size. The
    /// running index is kept correct by [`Sealer::seal_one`] on both the common
    /// tail-append path (`push_record`) and the rare out-of-order re-fold path
    /// (full `EntityIndex::build`), so it always matches `inner.records`.
    #[must_use]
    pub fn verify_entity(&self, kind: EntityKind, id: u64) -> ChainVerification {
        let result = match self.sealer.inner.lock() {
            Ok(inner) => verify_entity(
                &inner.records,
                &inner.index,
                (kind, id),
                &self.genesis,
                &*self.sealer.source,
            ),
            Err(_) => ChainVerification {
                passed: false,
                head_seq: self.genesis.seq,
                head_digest_hex: String::new(),
                earliest_broken_seq: None,
                reason: Some("provenance chain inner lock poisoned".to_string()),
                transactions_checked: 0,
            },
        };
        self.record_verification(&result);
        result
    }

    /// Prove the current chain extends a previously exported anchor.
    #[must_use]
    pub fn verify_against_anchor(&self, anchor: &ChainHead) -> ChainVerification {
        let records = self.snapshot_records();
        let result = verify_against_anchor(&records, &self.genesis, anchor);
        self.record_verification(&result);
        result
    }

    fn snapshot_records(&self) -> Vec<ChainTxRecord> {
        self.sealer
            .inner
            .lock()
            .map(|inner| inner.records.clone())
            .unwrap_or_default()
    }

    /// Flush and stop the sealer thread. Idempotent.
    pub fn shutdown(&self) {
        self.stop_and_flush();
    }

    /// Signal the sealer to stop, join it, and flush the log + checkpoint the
    /// head. Idempotent: the `stopped` swap guards against a double stop, so a
    /// `Drop` after an explicit [`shutdown`](Self::shutdown) is a no-op.
    ///
    /// With `stopped` set, concurrent enqueues seal inline, so the channel
    /// drains and the `Stop` is guaranteed to be delivered.
    fn stop_and_flush(&self) {
        if self.stopped.swap(true, Ordering::AcqRel) {
            return;
        }
        let _ = self.sender.send(SealMsg::Stop);
        if let Ok(mut guard) = self.handle.lock()
            && let Some(handle) = guard.take()
        {
            let _ = handle.join();
        }
        // Final durability: flush the log and checkpoint the head.
        if self.fsync != ChainFsyncMode::Never {
            let _ = self.sealer.store.flush();
        }
        let _ = self.sealer.checkpoint();
    }
}

impl Drop for ProvenanceChain {
    fn drop(&mut self) {
        self.stop_and_flush();
    }
}

/// The background sealer loop: drain the channel into a commit-ordered reorder
/// buffer, then flush the buffer to the log in `(commit_ts, commit_ts_logical,
/// tx_id)` order. Final canonical ordering is enforced by
/// [`Sealer::seal_one`]'s sorted insert regardless.
fn run_sealer(sealer: Arc<Sealer>, rx: Receiver<SealMsg>) {
    let mut buffer: BTreeMap<(i64, u32, u64), PendingTx> = BTreeMap::new();
    loop {
        match rx.recv() {
            Ok(SealMsg::Seal(p)) => {
                buffer.insert((p.commit_ts_micros, p.commit_ts_logical, p.tx_id), p);
                // Absorb everything already queued so a burst flushes in order.
                let mut stop = false;
                loop {
                    match rx.try_recv() {
                        Ok(SealMsg::Seal(p2)) => {
                            buffer
                                .insert((p2.commit_ts_micros, p2.commit_ts_logical, p2.tx_id), p2);
                        }
                        Ok(SealMsg::Stop) => {
                            stop = true;
                            break;
                        }
                        Err(std::sync::mpsc::TryRecvError::Empty) => break,
                        Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                            stop = true;
                            break;
                        }
                    }
                }
                flush_buffer(&sealer, &mut buffer);
                if stop {
                    let _ = sealer.checkpoint();
                    return;
                }
            }
            Ok(SealMsg::Stop) | Err(_) => {
                flush_buffer(&sealer, &mut buffer);
                let _ = sealer.checkpoint();
                return;
            }
        }
    }
}

/// Drain the reorder buffer in ascending `(commit_ts, commit_ts_logical, tx_id)`
/// order, sealing each transaction, then checkpoint the advanced head.
fn flush_buffer(sealer: &Sealer, buffer: &mut BTreeMap<(i64, u32, u64), PendingTx>) {
    if buffer.is_empty() {
        return;
    }
    while let Some((&key, _)) = buffer.iter().next() {
        if let Some(pending) = buffer.remove(&key) {
            let _ = sealer.seal_one(pending);
        }
    }
    let _ = sealer.checkpoint();
}

#[cfg(test)]
mod ordering_tests {
    //! Issue #3351 finding 4b: the head digest is a deterministic function of the
    //! commit-timestamp-ordered record set, independent of enqueue arrival order.

    use super::*;
    use crate::core::property::PropertyValue;
    use crate::provenance_chain::canonical::VersionHashInput;
    use std::collections::HashMap;

    struct MapSource(HashMap<(EntityKind, u64, u64), VersionHashInput>);
    impl VersionSource for MapSource {
        fn fetch(&self, kind: EntityKind, id: u64, version_id: u64) -> Option<VersionHashInput> {
            self.0.get(&(kind, id, version_id)).cloned()
        }
    }

    fn vinput(id: u64, vid: u64) -> VersionHashInput {
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
            properties: vec![("name".to_string(), PropertyValue::string("n"))],
        }
    }

    /// Seal `specs` (each `(commit_ts, logical, entity_id, version_id)`) into a
    /// fresh chain via the synchronous path and return the resulting head digest.
    fn head_after(specs: &[(i64, u32, u64, u64)]) -> [u8; 32] {
        let dir = tempfile::tempdir().unwrap();
        let mut map = HashMap::new();
        for (_, _, id, vid) in specs {
            let vi = vinput(*id, *vid);
            map.insert((vi.entity_kind, vi.entity_id, vi.version_id), vi);
        }
        let source: Arc<dyn VersionSource + Send + Sync> = Arc::new(MapSource(map));
        let config = ChainConfig {
            enabled: true,
            fsync: ChainFsyncMode::Never,
            dir: None,
        };
        let chain = ProvenanceChain::open(&config, dir.path(), 1, 0, source).unwrap();
        for (commit_ts, logical, id, vid) in specs {
            chain
                .seal_pending_sync(PendingTx {
                    commit_ts_micros: *commit_ts,
                    commit_ts_logical: *logical,
                    tx_id: *vid, // arbitrary metadata; not folded
                    anchor_lsn: 1,
                    entity_refs: vec![(EntityKind::Node, *id, *vid)],
                })
                .unwrap();
        }
        chain.head().digest
    }

    #[test]
    fn out_of_order_enqueue_yields_same_head_as_in_order() {
        // Three transactions with distinct commit timestamps.
        let in_order = [(100, 0, 1, 1), (200, 0, 2, 2), (300, 0, 3, 3)];
        let reversed = [(300, 0, 3, 3), (100, 0, 1, 1), (200, 0, 2, 2)];
        let shuffled = [(200, 0, 2, 2), (300, 0, 3, 3), (100, 0, 1, 1)];

        let h_in = head_after(&in_order);
        let h_rev = head_after(&reversed);
        let h_shuf = head_after(&shuffled);

        assert_eq!(
            h_in, h_rev,
            "reverse-order enqueue must match in-order head"
        );
        assert_eq!(h_in, h_shuf, "shuffled enqueue must match in-order head");
    }

    #[test]
    fn same_wallclock_distinct_logical_stays_ordered() {
        // Two transactions in the same microsecond, distinguished by logical.
        let a = [(100, 1, 1, 1), (100, 2, 2, 2)];
        let b = [(100, 2, 2, 2), (100, 1, 1, 1)];
        assert_eq!(head_after(&a), head_after(&b));
    }
}

#[cfg(test)]
mod scan_free_tests {
    //! Issue #3351 task #1: entity-scoped verify must touch only the target
    //! entity's records — O(entity versions), not O(database size). Proven by
    //! counting `VersionSource::fetch` calls: a `verify_entity` fetches exactly
    //! the entity's version count, regardless of how many other transactions the
    //! chain holds.

    use super::*;
    use crate::core::property::PropertyValue;
    use crate::provenance_chain::canonical::VersionHashInput;
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};

    /// A version source that counts every `fetch` so a test can assert an
    /// entity-scoped verify does not re-fetch the whole database.
    struct CountingSource {
        map: HashMap<(EntityKind, u64, u64), VersionHashInput>,
        fetches: AtomicUsize,
    }
    impl VersionSource for CountingSource {
        fn fetch(&self, kind: EntityKind, id: u64, version_id: u64) -> Option<VersionHashInput> {
            self.fetches.fetch_add(1, AtomicOrdering::Relaxed);
            self.map.get(&(kind, id, version_id)).cloned()
        }
    }

    fn vinput(id: u64, vid: u64) -> VersionHashInput {
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
            properties: vec![("name".to_string(), PropertyValue::string("n"))],
        }
    }

    #[test]
    fn entity_verify_fetches_only_the_entity_versions() {
        // Five transactions; entity 1 appears in exactly two of them.
        let specs: [(i64, u32, u64, u64); 5] = [
            (100, 0, 1, 1), // entity 1, v1
            (200, 0, 2, 2), // entity 2
            (300, 0, 3, 3), // entity 3
            (400, 0, 1, 4), // entity 1, v4 (second occurrence)
            (500, 0, 4, 5), // entity 4
        ];

        let dir = tempfile::tempdir().unwrap();
        let mut map = HashMap::new();
        for (_, _, id, vid) in specs {
            let vi = vinput(id, vid);
            map.insert((vi.entity_kind, vi.entity_id, vi.version_id), vi);
        }
        let source = Arc::new(CountingSource {
            map,
            fetches: AtomicUsize::new(0),
        });
        let config = ChainConfig {
            enabled: true,
            fsync: ChainFsyncMode::Never,
            dir: None,
        };
        let chain =
            ProvenanceChain::open(&config, dir.path(), 1, 0, source.clone() as Arc<_>).unwrap();
        for (commit_ts, logical, id, vid) in specs {
            chain
                .seal_pending_sync(PendingTx {
                    commit_ts_micros: commit_ts,
                    commit_ts_logical: logical,
                    tx_id: vid,
                    anchor_lsn: 1,
                    entity_refs: vec![(EntityKind::Node, id, vid)],
                })
                .unwrap();
        }

        // Reset the counter: sealing itself fetches (once per version). We only
        // want to measure the *verify* pass.
        source.fetches.store(0, AtomicOrdering::Relaxed);

        let v = chain.verify_entity(EntityKind::Node, 1);
        assert!(v.passed, "clean entity verify: {:?}", v.reason);
        assert_eq!(
            v.transactions_checked, 2,
            "entity 1 is present in exactly two transactions"
        );
        assert_eq!(
            source.fetches.load(AtomicOrdering::Relaxed),
            2,
            "entity verify must fetch only entity 1's two versions, not all five"
        );
    }
}
