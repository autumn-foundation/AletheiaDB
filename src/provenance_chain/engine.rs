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
//! # Ordering
//!
//! The sealer keeps a small reorder buffer keyed by `(commit_ts, tx_id)` so
//! records are appended in commit order even when two racing writers enqueue out
//! of order. Correctness never depends on the channel capacity: an enqueue that
//! finds the channel full seals **inline** (drop-to-sync-seal) rather than
//! blocking or dropping the transaction, so every committed transaction is
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
    /// Commit timestamp (micros since epoch) — the grouping and ordering key.
    pub commit_ts_micros: i64,
    /// The committing transaction's id.
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
    /// Seal one transaction: recompute its leaves from the source, fold onto the
    /// running head, append the record, and advance the head + entity index.
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

        let seq = inner.head.seq + 1;
        let prev = inner.head.digest;
        let txd = tx_digest(pending.commit_ts_micros, pending.tx_id, &leaves);
        let digest = chain_step(&prev, &txd);
        let record = ChainTxRecord {
            seq,
            commit_ts: pending.commit_ts_micros,
            tx_id: pending.tx_id,
            anchor_lsn: pending.anchor_lsn,
            leaves,
            entity_refs: kept_refs,
            digest,
        };

        self.store.append(&record)?;

        let record_idx = inner.records.len();
        inner.index.push_record(record_idx, &record);
        inner.head = ChainHead {
            seq,
            digest,
            commit_ts: pending.commit_ts_micros,
            anchor_lsn: pending.anchor_lsn,
            genesis_digest: self.genesis.genesis_digest,
        };
        inner.records.push(record);
        Ok(())
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
        let records = store.load()?;

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
    #[must_use]
    pub fn verify_entity(&self, kind: EntityKind, id: u64) -> ChainVerification {
        let records = self.snapshot_records();
        let index = EntityIndex::build(&records);
        let result = verify_entity(
            &records,
            &index,
            (kind, id),
            &self.genesis,
            &*self.sealer.source,
        );
        self.record_verification(&result);
        result
    }

    /// Prove the current chain extends a previously exported anchor.
    #[must_use]
    pub fn verify_against_anchor(&self, anchor: &ChainHead) -> ChainVerification {
        let records = self.snapshot_records();
        let result = verify_against_anchor(&records, anchor);
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
        if self.stopped.swap(true, Ordering::AcqRel) {
            return;
        }
        // With `stopped` set, concurrent enqueues seal inline, so the channel
        // drains and this Stop is guaranteed to be delivered.
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
        // `stopped`/`handle` guard against a double shutdown if one already ran.
        if self.stopped.swap(true, Ordering::AcqRel) {
            return;
        }
        let _ = self.sender.send(SealMsg::Stop);
        if let Ok(mut guard) = self.handle.lock()
            && let Some(handle) = guard.take()
        {
            let _ = handle.join();
        }
        if self.fsync != ChainFsyncMode::Never {
            let _ = self.sealer.store.flush();
        }
        let _ = self.sealer.checkpoint();
    }
}

/// The background sealer loop: drain the channel into a commit-ordered reorder
/// buffer, then flush the buffer to the log in `(commit_ts, tx_id)` order.
fn run_sealer(sealer: Arc<Sealer>, rx: Receiver<SealMsg>) {
    let mut buffer: BTreeMap<(i64, u64), PendingTx> = BTreeMap::new();
    loop {
        match rx.recv() {
            Ok(SealMsg::Seal(p)) => {
                buffer.insert((p.commit_ts_micros, p.tx_id), p);
                // Absorb everything already queued so a burst flushes in order.
                let mut stop = false;
                loop {
                    match rx.try_recv() {
                        Ok(SealMsg::Seal(p2)) => {
                            buffer.insert((p2.commit_ts_micros, p2.tx_id), p2);
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

/// Drain the reorder buffer in ascending `(commit_ts, tx_id)` order, sealing
/// each transaction, then checkpoint the advanced head.
fn flush_buffer(sealer: &Sealer, buffer: &mut BTreeMap<(i64, u64), PendingTx>) {
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
