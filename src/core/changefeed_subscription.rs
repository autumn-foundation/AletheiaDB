//! Push changefeed subscriptions (Issue #3375, Rust-API-only slice).
//!
//! The [`crate::db::AletheiaDB::list_changes`] API (Issue #3216) is a *pull* feed:
//! a caller repeatedly asks "what changed between T1 and T2?". This module adds the
//! complementary *push* feed: a caller [`subscribe`](ChangefeedBroadcaster::subscribe)s
//! once with a [`ChangeFilter`] and is handed a [`Subscription`] whose buffer fills with
//! matching [`ChangeRecord`]s as transactions commit, with no polling.
//!
//! # Durability contract (at-least-once + resume)
//!
//! The push feed is **best-effort at-least-once**. The *durable ground truth* remains
//! `list_changes` (backed by historical storage, which WAL recovery rebuilds). The
//! broadcaster never blocks the writer and never persists anything: if a consumer is slow
//! (its bounded buffer overflows) it is marked **Lagged** and disconnected; if the process
//! crashes after a commit fsynced but *before* the in-memory broadcast ran, the event was
//! never pushed at all. In **both** cases the consumer recovers with zero loss by resuming
//! a `list_changes` pull from its last [`resume_token`](Subscription::resume_token) — the
//! encoded [`ChangeCursor`](crate::core::changefeed) of the last event it drained. The
//! union of (live-delivered ∪ resume-pull) equals exactly the #3216 pull over the window.
//!
//! Because a resume re-pulls from a cursor at or before some already-delivered events,
//! **duplicate delivery is possible on resume**. The dedup key is the stable
//! [`ChangeRecord::cursor`](crate::core::changefeed) (equivalently
//! `(tx-time, kind, entity_id, version_id)`): a consumer that has seen a cursor once
//! ignores it on re-delivery.
//!
//! # Concurrency & lock discipline
//!
//! The broadcaster's locks (registry, per-subscriber, and the ordered-emit `sequencer`) are
//! **leaves**: the commit path reserves an ordered ticket
//! ([`reserve_emit_ticket`](ChangefeedBroadcaster::reserve_emit_ticket) — a lock-free atomic
//! taken under the `current_timestamp` lock so ticket order == commit-timestamp order) and,
//! only *after* every write-path lock has been released, submits its records
//! ([`EmitTicket::submit`]). The sequencer then releases records into subscriber buffers in
//! strict commit order, so per-subscriber delivery is cursor-ascending regardless of the order
//! concurrent commits finish — the precondition that makes `last_delivered` a zero-loss resume
//! high-water-mark. None of these leaf locks ever call back into historical / WAL / timestamp
//! state. A slow consumer can never back-pressure the writer or another subscriber — a full
//! buffer is dropped-into-Lagged, not awaited.
//!
//! The blocking [`recv_timeout`](Subscription::recv_timeout) is a synchronous
//! `Mutex`+`Condvar` long-poll (no async runtime dependency); an HTTP SSE surface and an
//! MCP `await_changes` tool that wrap it are a coordinated follow-up (Lane 1).

use crate::core::changefeed::{ChangeRecord, ChangeType, EntityKind};
use crate::core::error::{Result, StorageError};
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, RwLock, Weak};
use std::time::{Duration, Instant};

/// Default maximum number of concurrently-live subscriptions per broadcaster.
pub const DEFAULT_MAX_SUBSCRIPTIONS: usize = 128;

/// Default per-subscription buffer capacity (events retained before Lagged).
pub const DEFAULT_BUFFER_CAPACITY: usize = 1024;

/// Caps governing a [`ChangefeedBroadcaster`].
///
/// Both bounds protect memory: `max_subscriptions` caps fan-out breadth, `buffer_capacity`
/// caps how far a single slow consumer may fall behind before it is disconnected (Lagged)
/// rather than growing without bound.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChangefeedConfig {
    /// Maximum number of concurrently-live subscriptions.
    pub max_subscriptions: usize,
    /// Per-subscription buffer capacity, in events, before overflow → Lagged.
    pub buffer_capacity: usize,
}

impl Default for ChangefeedConfig {
    fn default() -> Self {
        ChangefeedConfig {
            max_subscriptions: DEFAULT_MAX_SUBSCRIPTIONS,
            buffer_capacity: DEFAULT_BUFFER_CAPACITY,
        }
    }
}

/// Predicate selecting which committed changes a [`Subscription`] receives.
///
/// Each dimension is optional. Semantics (chosen so each test dimension is meaningful):
///
/// - A **filter-less** filter (all dimensions `None`) matches **everything**.
/// - If either `node_labels` or `edge_types` is set, the record's *kind* must have a set
///   dimension containing its label: setting only `node_labels` therefore yields **only**
///   matching node changes (edges excluded), and setting only `edge_types` yields **only**
///   matching edge changes (nodes excluded). Set both to receive both kinds.
/// - `change_types`, if set, is an independent AND: the record's [`ChangeType`] must be in
///   the set, regardless of kind.
///
/// Label / edge-type matching is **exact string** match, mirroring #3216's `label` filter.
#[derive(Debug, Clone, Default)]
pub struct ChangeFilter {
    node_labels: Option<HashSet<String>>,
    edge_types: Option<HashSet<String>>,
    change_types: Option<HashSet<ChangeType>>,
}

impl ChangeFilter {
    /// A filter that matches every committed change.
    pub fn all() -> Self {
        ChangeFilter::default()
    }

    /// Restrict to node changes whose label is one of `labels` (exact match).
    #[must_use]
    pub fn with_node_labels<I, S>(mut self, labels: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.node_labels = Some(labels.into_iter().map(Into::into).collect());
        self
    }

    /// Restrict to edge changes whose type is one of `types` (exact match).
    #[must_use]
    pub fn with_edge_types<I, S>(mut self, types: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.edge_types = Some(types.into_iter().map(Into::into).collect());
        self
    }

    /// Restrict to the given change types (created / modified / deleted).
    #[must_use]
    pub fn with_change_types<I>(mut self, types: I) -> Self
    where
        I: IntoIterator<Item = ChangeType>,
    {
        self.change_types = Some(types.into_iter().collect());
        self
    }

    /// Whether `record` passes this filter (see the type-level semantics).
    pub fn matches(&self, record: &ChangeRecord) -> bool {
        if let Some(cts) = &self.change_types
            && !cts.contains(&record.change_type)
        {
            return false;
        }

        match (&self.node_labels, &self.edge_types) {
            // No kind/label constraint at all → any kind passes.
            (None, None) => true,
            // At least one kind dimension is constrained: the record's own kind must
            // have a set dimension that contains its label.
            _ => match record.kind {
                EntityKind::Node => self
                    .node_labels
                    .as_ref()
                    .is_some_and(|s| s.contains(&record.label)),
                EntityKind::Edge => self
                    .edge_types
                    .as_ref()
                    .is_some_and(|s| s.contains(&record.label)),
            },
        }
    }
}

/// Error returned by [`Subscription::recv_timeout`] when events cannot be delivered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecvError {
    /// The subscription overflowed its bounded buffer and was disconnected. No further
    /// live events will arrive. Recover losslessly by resuming a `list_changes` pull from
    /// `resume_token` — the cursor of the last event the consumer drained, or, if it never
    /// drained one, the **baseline frontier cursor captured at subscribe time** (see
    /// [`ChangefeedBroadcaster::subscribe_with_baseline`]) so "resume from the subscription's
    /// start point" is always a well-defined, gap-free anchor. `None` only for a core-level
    /// subscription created without a baseline (the DB [`crate::db::AletheiaDB::subscribe_changes`]
    /// API always sets one).
    Lagged {
        /// Encoded [`ChangeCursor`](crate::core::changefeed) to resume from, if any.
        resume_token: Option<String>,
    },
}

/// Mutable, lock-guarded state of a single subscriber.
struct SubscriberInner {
    queue: VecDeque<ChangeRecord>,
    /// Encoded cursor of the last event the *consumer drained* (via poll/recv_timeout).
    last_delivered: Option<String>,
    /// Set once the buffer overflowed; the subscription is then disconnected.
    lagged: bool,
}

/// Shared per-subscriber state, held by both the broadcaster (for pushing) and the
/// [`Subscription`] handle (for draining).
struct SubscriberState {
    id: u64,
    filter: ChangeFilter,
    buffer_capacity: usize,
    inner: Mutex<SubscriberInner>,
    cvar: Condvar,
}

impl SubscriberState {
    /// Push every record in `records` that matches this subscriber's filter, in order,
    /// as one contiguous batch (a single lock acquisition, so one commit's records are
    /// never interleaved with another's for this subscriber). Overflow → Lagged.
    ///
    /// Returns `true` if at least one record was buffered (so the caller can notify).
    fn push_matching(&self, records: &[ChangeRecord]) -> bool {
        // Fast path: avoid taking the lock at all when nothing matches (the common case
        // for a selective idle subscriber).
        if !records.iter().any(|r| self.filter.matches(r)) {
            return false;
        }

        // Poison-safety (Issue #3375 review F6/F9): `push_matching` runs synchronously on a
        // writer's commit thread AFTER that transaction is already committed. A poisoned
        // subscriber lock (some *other* consumer thread panicked while draining) must NEVER
        // unwind this unrelated, already-committed writer — recover the guard and treat the
        // faulty subscriber as just another buffer to push into (it will be dropped/Lagged
        // on its own). No production path panics on poisoning.
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if inner.lagged {
            return false;
        }
        let mut pushed = false;
        for record in records {
            if !self.filter.matches(record) {
                continue;
            }
            if inner.queue.len() >= self.buffer_capacity {
                // Slow consumer: disconnect it rather than back-pressure the writer.
                inner.lagged = true;
                break;
            }
            inner.queue.push_back(record.clone());
            pushed = true;
        }
        drop(inner);
        if pushed {
            self.cvar.notify_all();
        }
        pushed
    }
}

/// A live changefeed subscription handle (the consumer side).
///
/// Drop deregisters the subscription from its broadcaster (no leak). Cloning is
/// intentionally not provided: a subscription is a single consumer's cursor into the feed.
pub struct Subscription {
    state: Arc<SubscriberState>,
    broadcaster: Weak<ChangefeedBroadcaster>,
}

impl Subscription {
    /// The subscription's unique id within its broadcaster.
    pub fn id(&self) -> u64 {
        self.state.id
    }

    /// The [`ChangeFilter`] this subscription was created with.
    pub fn filter(&self) -> &ChangeFilter {
        &self.state.filter
    }

    /// Drain all currently-buffered events without blocking.
    ///
    /// Returns an empty vec if nothing is buffered (including when the subscription is
    /// Lagged and already drained — check [`is_lagged`](Self::is_lagged) /
    /// [`recv_timeout`](Self::recv_timeout) to observe the Lagged transition).
    pub fn poll(&self) -> Vec<ChangeRecord> {
        let mut inner = self.state.inner.lock().unwrap_or_else(|e| e.into_inner());
        let drained: Vec<ChangeRecord> = inner.queue.drain(..).collect();
        if let Some(last) = drained.last() {
            inner.last_delivered = Some(last.cursor().encode());
        }
        drained
    }

    /// Block up to `timeout` for events.
    ///
    /// Returns:
    /// - `Ok(events)` with one or more events as soon as any are buffered;
    /// - `Ok(vec![])` if the timeout elapses with nothing buffered (timeout-empty);
    /// - `Err(RecvError::Lagged { resume_token })` once the buffer has been drained and the
    ///   subscription was disconnected for overflow.
    pub fn recv_timeout(
        &self,
        timeout: Duration,
    ) -> std::result::Result<Vec<ChangeRecord>, RecvError> {
        let deadline = Instant::now().checked_add(timeout);
        let mut inner = self.state.inner.lock().unwrap_or_else(|e| e.into_inner());
        loop {
            if !inner.queue.is_empty() {
                let drained: Vec<ChangeRecord> = inner.queue.drain(..).collect();
                if let Some(last) = drained.last() {
                    inner.last_delivered = Some(last.cursor().encode());
                }
                return Ok(drained);
            }
            if inner.lagged {
                return Err(RecvError::Lagged {
                    resume_token: inner.last_delivered.clone(),
                });
            }
            let remaining = match deadline {
                Some(d) => d.saturating_duration_since(Instant::now()),
                None => timeout,
            };
            if remaining.is_zero() {
                return Ok(Vec::new());
            }
            let (guard, wait) = self
                .state
                .cvar
                .wait_timeout(inner, remaining)
                .unwrap_or_else(|e| e.into_inner());
            inner = guard;
            if wait.timed_out() && inner.queue.is_empty() && !inner.lagged {
                return Ok(Vec::new());
            }
        }
    }

    /// Encoded [`ChangeCursor`](crate::core::changefeed) of the last event this subscription
    /// drained, or `None` if it has drained nothing yet. Hand this to `list_changes` (as the
    /// `cursor`) to resume the pull feed exactly after the last event seen.
    pub fn resume_token(&self) -> Option<String> {
        self.state
            .inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .last_delivered
            .clone()
    }

    /// Whether this subscription has been disconnected for buffer overflow.
    pub fn is_lagged(&self) -> bool {
        self.state
            .inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .lagged
    }
}

impl std::fmt::Debug for Subscription {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Subscription")
            .field("id", &self.state.id)
            .field("lagged", &self.is_lagged())
            .finish_non_exhaustive()
    }
}

impl Drop for Subscription {
    fn drop(&mut self) {
        if let Some(broadcaster) = self.broadcaster.upgrade() {
            broadcaster.deregister(self.state.id);
        }
    }
}

/// Ordered-emit sequencer state (Issue #3375 HIGH review fix).
///
/// Guards the strict commit-timestamp-ascending release of records into subscriber buffers.
/// A commit reserves a monotonic `seq` *under the `current_timestamp` lock* (so `seq` order
/// == commit-timestamp order == [`ChangeCursor`](crate::core::changefeed) order), then, once
/// its records are built post-commit, [`submit`](ChangefeedBroadcaster::submit_sequenced)s
/// `(seq, records)`. `submit` parks the records if an earlier `seq` has not been released yet,
/// or drains `pending` in order — pushing to subscriber buffers *inside* this critical section
/// — otherwise. This guarantees every subscriber sees records cursor-ascending regardless of
/// the order concurrent commits finish their apply/record-build, which is exactly what makes
/// the `last_delivered`-as-high-water-mark resume token a zero-loss contract.
struct EmitSequencer {
    /// The next `seq` eligible for release.
    next_release: u64,
    /// Reserved-but-not-yet-released records, keyed by `seq` (ascending).
    pending: BTreeMap<u64, Vec<ChangeRecord>>,
}

/// Fan-out hub for the push changefeed.
///
/// Held as an `Arc` on the database; [`subscribe`](Self::subscribe) hands out
/// [`Subscription`]s and the commit path reserves an ordered ticket
/// ([`reserve_emit_ticket`](Self::reserve_emit_ticket)) then submits each committed
/// transaction's records through it (which releases them to subscribers in cursor order).
pub struct ChangefeedBroadcaster {
    subscribers: RwLock<HashMap<u64, Arc<SubscriberState>>>,
    next_id: AtomicU64,
    /// Lock-free fast-path gate: emit-related work is skipped entirely when this is zero, so
    /// the zero-subscriber write path pays only a single atomic load and reserves no ticket.
    ///
    /// Ordering (review F10): loaded `Acquire` (in [`has_subscribers`](Self::has_subscribers) /
    /// [`emit`](Self::emit)) and stored `Release` (in [`subscribe`](Self::subscribe) /
    /// [`deregister`](Self::deregister)) so a just-completed subscribe happens-before a
    /// subsequent commit's gate check — a weakly-ordered arch cannot skip emit for a
    /// consumer that has already subscribed.
    subscriber_count: AtomicUsize,
    max_subscriptions: AtomicUsize,
    buffer_capacity: AtomicUsize,
    /// Monotonic ticket reservation counter (Issue #3375 ordered emit). Bumped once per
    /// commit that has subscribers, *while the `current_timestamp` lock is held*, so a
    /// ticket's `seq` totally orders commits by commit timestamp.
    emit_ticket_counter: AtomicU64,
    /// The ordered-release sequencer. A leaf lock acquired only post-commit (never while a
    /// write-path primitive is held); it never re-enters historical / WAL / current_timestamp.
    sequencer: Mutex<EmitSequencer>,
}

/// RAII reservation for the ordered emit of one commit (Issue #3375 HIGH review fix).
///
/// Reserved *under the `current_timestamp` lock* via
/// [`ChangefeedBroadcaster::reserve_emit_ticket`] so its [`seq`](Self::seq) totally orders
/// commits. The success path consumes it with [`submit`](Self::submit). If the commit instead
/// **aborts or panics** after reserving but before submitting (e.g. a WAL failure or the
/// Issue #3416 commit-time write-skew re-check in `apply_changes`), `Drop` submits an **empty**
/// release for the reserved `seq` so the sequencer never stalls on a hole — a reserved-but-
/// never-submitted `seq` would otherwise permanently block every later commit's records.
#[must_use = "an EmitTicket must be submitted (or dropped to release its sequence)"]
pub struct EmitTicket {
    broadcaster: Arc<ChangefeedBroadcaster>,
    seq: u64,
    submitted: bool,
}

impl EmitTicket {
    /// The reserved sequence number (commit-timestamp order).
    pub fn seq(&self) -> u64 {
        self.seq
    }

    /// Submit this commit's `records` for ordered release to subscribers, consuming the
    /// ticket. Records are pushed to subscriber buffers only once every earlier `seq` has
    /// been released, in strict cursor order.
    pub fn submit(mut self, records: Vec<ChangeRecord>) {
        self.submitted = true;
        self.broadcaster.submit_sequenced(self.seq, records);
    }
}

impl Drop for EmitTicket {
    fn drop(&mut self) {
        if !self.submitted {
            // Aborted/panicked after reserving: release an empty slot so the sequencer
            // never stalls waiting for a `seq` that will never be submitted.
            self.broadcaster.submit_sequenced(self.seq, Vec::new());
        }
    }
}

impl std::fmt::Debug for EmitTicket {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EmitTicket")
            .field("seq", &self.seq)
            .field("submitted", &self.submitted)
            .finish()
    }
}

impl std::fmt::Debug for ChangefeedBroadcaster {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChangefeedBroadcaster")
            .field(
                "subscriber_count",
                &self.subscriber_count.load(Ordering::Relaxed),
            )
            .field(
                "max_subscriptions",
                &self.max_subscriptions.load(Ordering::Relaxed),
            )
            .field(
                "buffer_capacity",
                &self.buffer_capacity.load(Ordering::Relaxed),
            )
            .finish()
    }
}

impl ChangefeedBroadcaster {
    /// Create a broadcaster with the default caps ([`ChangefeedConfig::default`]).
    pub fn new() -> Self {
        Self::with_config(ChangefeedConfig::default())
    }

    /// Create a broadcaster with explicit caps.
    pub fn with_config(config: ChangefeedConfig) -> Self {
        ChangefeedBroadcaster {
            subscribers: RwLock::new(HashMap::new()),
            next_id: AtomicU64::new(1),
            subscriber_count: AtomicUsize::new(0),
            max_subscriptions: AtomicUsize::new(config.max_subscriptions.max(1)),
            buffer_capacity: AtomicUsize::new(config.buffer_capacity.max(1)),
            emit_ticket_counter: AtomicU64::new(0),
            sequencer: Mutex::new(EmitSequencer {
                next_release: 0,
                pending: BTreeMap::new(),
            }),
        }
    }

    /// Update the caps. Affects the `max_subscriptions` check immediately and the
    /// `buffer_capacity` of **future** subscriptions (existing ones keep the capacity they
    /// were created with).
    pub fn set_config(&self, config: ChangefeedConfig) {
        self.max_subscriptions
            .store(config.max_subscriptions.max(1), Ordering::Relaxed);
        self.buffer_capacity
            .store(config.buffer_capacity.max(1), Ordering::Relaxed);
    }

    /// Register a new subscription with the given filter.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError::CapacityExceeded`] when the broadcaster is already at its
    /// `max_subscriptions` cap (maps to the MCP/#3234 `FAILED_PRECONDITION`/resource class).
    pub fn subscribe(self: &Arc<Self>, filter: ChangeFilter) -> Result<Subscription> {
        self.subscribe_with_baseline(filter, None)
    }

    /// Register a new subscription, seeding its resume anchor with `baseline` (Issue #3375
    /// review F4).
    ///
    /// `baseline` is the encoded [`ChangeCursor`](crate::core::changefeed) of the committed
    /// frontier at subscribe time (captured by
    /// [`crate::db::AletheiaDB::subscribe_changes`] under the `current_timestamp` lock). It
    /// becomes the subscription's initial `last_delivered`, so [`Subscription::resume_token`]
    /// is well-defined *before the consumer drains anything* and a
    /// `RecvError::Lagged { resume_token }` on an as-yet-undrained subscription still yields a
    /// gap-free "resume from where I subscribed" anchor. Draining the first event overwrites
    /// it with that event's cursor as usual. Pass `None` for no anchor (used by the core-level
    /// unit tests).
    ///
    /// # Errors
    ///
    /// Returns [`StorageError::CapacityExceeded`] when the broadcaster is already at its
    /// `max_subscriptions` cap.
    pub fn subscribe_with_baseline(
        self: &Arc<Self>,
        filter: ChangeFilter,
        baseline: Option<String>,
    ) -> Result<Subscription> {
        let mut subs = self.subscribers.write().unwrap_or_else(|e| e.into_inner());
        let max = self.max_subscriptions.load(Ordering::Relaxed);
        if subs.len() >= max {
            return Err(StorageError::CapacityExceeded {
                resource: "changefeed subscriptions".to_string(),
                current: subs.len(),
                limit: max,
            }
            .into());
        }
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let state = Arc::new(SubscriberState {
            id,
            filter,
            buffer_capacity: self.buffer_capacity.load(Ordering::Relaxed),
            inner: Mutex::new(SubscriberInner {
                queue: VecDeque::new(),
                last_delivered: baseline,
                lagged: false,
            }),
            cvar: Condvar::new(),
        });
        subs.insert(id, Arc::clone(&state));
        // Release: publish the subscriber insertion before the count becomes visible, so a
        // subsequent commit's Acquire gate load observes this subscriber (review F10).
        self.subscriber_count.store(subs.len(), Ordering::Release);
        Ok(Subscription {
            state,
            broadcaster: Arc::downgrade(self),
        })
    }

    /// Push a committed transaction's `records` to every matching subscriber, **unordered**.
    ///
    /// This is the raw fan-out primitive (also the direct entry point used by the core unit
    /// tests). The production commit path does **not** call this directly; it goes through the
    /// ordered [`reserve_emit_ticket`](Self::reserve_emit_ticket) / [`EmitTicket::submit`]
    /// path, which invokes [`push_to_subscribers`](Self::push_to_subscribers) only from inside
    /// the ordered drain. Cheap when there are no subscribers (single Acquire load) and never
    /// blocks on a slow consumer (overflow marks that subscriber Lagged and moves on).
    pub fn emit(&self, records: &[ChangeRecord]) {
        if records.is_empty() || self.subscriber_count.load(Ordering::Acquire) == 0 {
            return;
        }
        self.push_to_subscribers(records);
    }

    /// The registry-fanout half of emit: push `records` to every matching subscriber buffer.
    /// Invoked from [`emit`](Self::emit) and, on the production path, from inside the ordered
    /// drain in [`submit_sequenced`](Self::submit_sequenced) (so per-subscriber delivery is
    /// cursor-ascending).
    fn push_to_subscribers(&self, records: &[ChangeRecord]) {
        let subs = self.subscribers.read().unwrap_or_else(|e| e.into_inner());
        for state in subs.values() {
            state.push_matching(records);
        }
    }

    /// Reserve a monotonic ordered-emit ticket for the commit currently in progress.
    ///
    /// MUST be called while the `current_timestamp` lock is held (right after the commit
    /// timestamp is assigned), so the ticket's `seq` totally orders commits by commit
    /// timestamp — equivalently by [`ChangeCursor`](crate::core::changefeed). The reservation
    /// is a single atomic `fetch_add` (no lock taken), so it does not violate the write-path
    /// lock order. The returned [`EmitTicket`] MUST later be
    /// [`submit`](EmitTicket::submit)ted (success) or dropped (abort → empty release).
    pub fn reserve_emit_ticket(self: &Arc<Self>) -> EmitTicket {
        let seq = self.emit_ticket_counter.fetch_add(1, Ordering::Relaxed);
        EmitTicket {
            broadcaster: Arc::clone(self),
            seq,
            submitted: false,
        }
    }

    /// Insert `(seq, records)` into the sequencer and drain every now-contiguous prefix into
    /// subscriber buffers in strict `seq` (== cursor) order.
    ///
    /// The push happens **inside** the sequencer critical section so two concurrent submits
    /// can never reorder their pushes: whichever holds the lock releases its contiguous run
    /// atomically. A writer never *waits* here — it either drains (possibly cascading records
    /// parked earlier by other, later-finishing commits) or parks its own records and returns.
    /// The sequencer lock is a leaf (only registry + subscriber leaf locks are taken beneath
    /// it, post-commit); it never re-enters historical / WAL / current_timestamp.
    fn submit_sequenced(&self, seq: u64, records: Vec<ChangeRecord>) {
        let mut seqr = self.sequencer.lock().unwrap_or_else(|e| e.into_inner());
        seqr.pending.insert(seq, records);
        // Drain the contiguous run starting at `next_release`.
        loop {
            let next = seqr.next_release;
            // Only release when the lowest parked seq is exactly the watermark.
            match seqr.pending.first_key_value() {
                Some((&k, _)) if k == next => {}
                _ => break,
            }
            let recs = seqr
                .pending
                .remove(&next)
                .expect("first_key_value just confirmed the entry is present");
            seqr.next_release += 1;
            // Push under the sequencer lock to preserve global release order. Empty runs
            // (aborted tickets, or commits with no changefeed records) just advance the
            // watermark without touching subscribers.
            if !recs.is_empty() {
                self.push_to_subscribers(&recs);
            }
        }
    }

    /// Whether any subscription is currently live (fast, lock-free).
    pub fn has_subscribers(&self) -> bool {
        self.subscriber_count.load(Ordering::Acquire) != 0
    }

    /// The number of currently-live subscriptions.
    pub fn subscription_count(&self) -> usize {
        self.subscribers
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .len()
    }

    fn deregister(&self, id: u64) {
        let mut subs = self.subscribers.write().unwrap_or_else(|e| e.into_inner());
        if subs.remove(&id).is_some() {
            self.subscriber_count.store(subs.len(), Ordering::Release);
        }
    }
}

impl Default for ChangefeedBroadcaster {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::changefeed::{ChangeType, EntityKind};
    use crate::core::temporal::{TimeRange, Timestamp};

    fn record(
        entity_id: u64,
        version_id: u64,
        kind: EntityKind,
        change_type: ChangeType,
        label: &str,
        tx: i64,
    ) -> ChangeRecord {
        let start = Timestamp::from(tx);
        // Open-ended tx range (start..MAX) so the derived cursor uses `start`.
        let tx_range = TimeRange::new(start, Timestamp::from(i64::MAX - 1000)).unwrap();
        let valid_range = if change_type == ChangeType::Deleted {
            // Tombstone: empty valid range (a point at the deletion instant).
            TimeRange::new(start, start).unwrap()
        } else {
            TimeRange::new(start, Timestamp::from(i64::MAX - 1000)).unwrap()
        };
        ChangeRecord {
            entity_id,
            version_id,
            kind,
            change_type,
            label: label.to_string(),
            transaction_time_range: tx_range,
            valid_time_range: valid_range,
        }
    }

    #[test]
    fn filterless_matches_everything() {
        let f = ChangeFilter::all();
        assert!(f.matches(&record(
            1,
            1,
            EntityKind::Node,
            ChangeType::Created,
            "Person",
            10
        )));
        assert!(f.matches(&record(
            2,
            2,
            EntityKind::Edge,
            ChangeType::Deleted,
            "KNOWS",
            11
        )));
    }

    #[test]
    fn node_label_filter_excludes_edges_and_other_labels() {
        let f = ChangeFilter::all().with_node_labels(["Person"]);
        assert!(f.matches(&record(
            1,
            1,
            EntityKind::Node,
            ChangeType::Created,
            "Person",
            10
        )));
        assert!(!f.matches(&record(
            1,
            1,
            EntityKind::Node,
            ChangeType::Created,
            "Company",
            10
        )));
        // An edge is excluded when only node labels are specified.
        assert!(!f.matches(&record(
            2,
            2,
            EntityKind::Edge,
            ChangeType::Created,
            "KNOWS",
            11
        )));
    }

    #[test]
    fn edge_type_filter_excludes_nodes() {
        let f = ChangeFilter::all().with_edge_types(["KNOWS"]);
        assert!(f.matches(&record(
            2,
            2,
            EntityKind::Edge,
            ChangeType::Created,
            "KNOWS",
            11
        )));
        assert!(!f.matches(&record(
            2,
            2,
            EntityKind::Edge,
            ChangeType::Created,
            "LIKES",
            11
        )));
        assert!(!f.matches(&record(
            1,
            1,
            EntityKind::Node,
            ChangeType::Created,
            "Person",
            10
        )));
    }

    #[test]
    fn change_type_filter_is_kind_independent_and() {
        let f = ChangeFilter::all().with_change_types([ChangeType::Deleted]);
        assert!(f.matches(&record(
            1,
            1,
            EntityKind::Node,
            ChangeType::Deleted,
            "Person",
            10
        )));
        assert!(f.matches(&record(
            2,
            2,
            EntityKind::Edge,
            ChangeType::Deleted,
            "KNOWS",
            11
        )));
        assert!(!f.matches(&record(
            1,
            1,
            EntityKind::Node,
            ChangeType::Created,
            "Person",
            10
        )));
    }

    #[test]
    fn combined_filter_and_composes() {
        let f = ChangeFilter::all()
            .with_node_labels(["Person"])
            .with_change_types([ChangeType::Created]);
        assert!(f.matches(&record(
            1,
            1,
            EntityKind::Node,
            ChangeType::Created,
            "Person",
            10
        )));
        // Right label, wrong change type.
        assert!(!f.matches(&record(
            1,
            1,
            EntityKind::Node,
            ChangeType::Modified,
            "Person",
            10
        )));
        // Right change type, wrong label.
        assert!(!f.matches(&record(
            1,
            1,
            EntityKind::Node,
            ChangeType::Created,
            "Company",
            10
        )));
    }

    #[test]
    fn subscribe_deregisters_on_drop() {
        let b = Arc::new(ChangefeedBroadcaster::new());
        assert_eq!(b.subscription_count(), 0);
        let sub = b.subscribe(ChangeFilter::all()).unwrap();
        assert_eq!(b.subscription_count(), 1);
        assert!(b.has_subscribers());
        drop(sub);
        assert_eq!(b.subscription_count(), 0);
        assert!(!b.has_subscribers());
    }

    #[test]
    fn max_subscriptions_cap_is_enforced() {
        let b = Arc::new(ChangefeedBroadcaster::with_config(ChangefeedConfig {
            max_subscriptions: 2,
            buffer_capacity: 16,
        }));
        let _s1 = b.subscribe(ChangeFilter::all()).unwrap();
        let _s2 = b.subscribe(ChangeFilter::all()).unwrap();
        let err = b.subscribe(ChangeFilter::all()).unwrap_err();
        match err {
            crate::core::error::Error::Storage(StorageError::CapacityExceeded {
                resource,
                current,
                limit,
            }) => {
                assert_eq!(resource, "changefeed subscriptions");
                assert_eq!(current, 2);
                assert_eq!(limit, 2);
            }
            other => panic!("expected CapacityExceeded, got {other:?}"),
        }
    }

    #[test]
    fn emit_delivers_only_matching_records_in_order() {
        let b = Arc::new(ChangefeedBroadcaster::new());
        let sub = b
            .subscribe(ChangeFilter::all().with_node_labels(["Person"]))
            .unwrap();
        let records = vec![
            record(1, 1, EntityKind::Node, ChangeType::Created, "Person", 10),
            record(2, 2, EntityKind::Edge, ChangeType::Created, "KNOWS", 11),
            record(3, 3, EntityKind::Node, ChangeType::Created, "Person", 12),
        ];
        b.emit(&records);
        let got = sub.poll();
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].entity_id, 1);
        assert_eq!(got[1].entity_id, 3);
    }

    #[test]
    fn overflow_marks_lagged_and_preserves_resume_token() {
        let b = Arc::new(ChangefeedBroadcaster::with_config(ChangefeedConfig {
            max_subscriptions: 8,
            buffer_capacity: 2,
        }));
        let sub = b.subscribe(ChangeFilter::all()).unwrap();

        // Deliver two, drain them → resume_token advances.
        b.emit(&[
            record(1, 1, EntityKind::Node, ChangeType::Created, "P", 10),
            record(2, 2, EntityKind::Node, ChangeType::Created, "P", 11),
        ]);
        let drained = sub.poll();
        assert_eq!(drained.len(), 2);
        let token = sub.resume_token().expect("token after drain");

        // Now overflow: push 3 into a cap-2 buffer while consumer never drains.
        b.emit(&[
            record(3, 3, EntityKind::Node, ChangeType::Created, "P", 12),
            record(4, 4, EntityKind::Node, ChangeType::Created, "P", 13),
            record(5, 5, EntityKind::Node, ChangeType::Created, "P", 14),
        ]);
        assert!(sub.is_lagged());
        // Drain the buffered prefix, then observe the Lagged signal.
        let _prefix = sub.poll();
        match sub.recv_timeout(Duration::from_millis(10)) {
            Err(RecvError::Lagged { resume_token }) => {
                assert!(resume_token.is_some());
            }
            other => panic!("expected Lagged, got {other:?}"),
        }
        // The stashed token is a valid cursor.
        assert!(!token.is_empty());
    }

    #[test]
    fn recv_timeout_returns_empty_on_timeout() {
        let b = Arc::new(ChangefeedBroadcaster::new());
        let sub = b.subscribe(ChangeFilter::all()).unwrap();
        let start = Instant::now();
        let got = sub.recv_timeout(Duration::from_millis(50)).unwrap();
        assert!(got.is_empty());
        assert!(start.elapsed() >= Duration::from_millis(40));
    }

    #[test]
    fn recv_timeout_wakes_on_emit() {
        let b = Arc::new(ChangefeedBroadcaster::new());
        let sub = b.subscribe(ChangeFilter::all()).unwrap();
        let b2 = Arc::clone(&b);
        let handle = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(20));
            b2.emit(&[record(1, 1, EntityKind::Node, ChangeType::Created, "P", 10)]);
        });
        let got = sub.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(got.len(), 1);
        handle.join().unwrap();
    }

    #[test]
    fn sequencer_releases_records_in_ascending_seq_order() {
        // HIGH review fix: submitting seq 1 (records B@101) BEFORE seq 0 (records A@100) must
        // deliver NOTHING until seq 0 is submitted, then both in ascending cursor order.
        let b = Arc::new(ChangefeedBroadcaster::new());
        let sub = b.subscribe(ChangeFilter::all()).unwrap();

        let t0 = b.reserve_emit_ticket();
        let t1 = b.reserve_emit_ticket();
        assert_eq!(t0.seq(), 0);
        assert_eq!(t1.seq(), 1);

        // Submit the LATER sequence first: it must park, delivering nothing.
        t1.submit(vec![record(
            101,
            101,
            EntityKind::Node,
            ChangeType::Created,
            "P",
            101,
        )]);
        assert!(
            sub.poll().is_empty(),
            "seq 1 must not be delivered before seq 0 is released"
        );

        // Now release seq 0: both cascade out in ascending order (A then B).
        t0.submit(vec![record(
            100,
            100,
            EntityKind::Node,
            ChangeType::Created,
            "P",
            100,
        )]);
        let got = sub.poll();
        assert_eq!(
            got.len(),
            2,
            "both records released once the hole is filled"
        );
        assert_eq!(got[0].entity_id, 100, "A (seq 0) delivered first");
        assert_eq!(got[1].entity_id, 101, "B (seq 1) delivered second");
    }

    #[test]
    fn dropped_ticket_empty_release_does_not_stall_sequencer() {
        // RAII contract: reserving seq 0 then DROPPING it without submit must not stall the
        // sequencer — seq 1 must still be released.
        let b = Arc::new(ChangefeedBroadcaster::new());
        let sub = b.subscribe(ChangeFilter::all()).unwrap();

        let t0 = b.reserve_emit_ticket();
        let t1 = b.reserve_emit_ticket();
        assert_eq!(t0.seq(), 0);
        assert_eq!(t1.seq(), 1);

        // Abort seq 0: Drop submits an empty release so the watermark advances past it.
        drop(t0);

        // seq 1 must now flow through (no permanent stall on the never-submitted seq 0).
        t1.submit(vec![record(
            101,
            101,
            EntityKind::Node,
            ChangeType::Created,
            "P",
            101,
        )]);
        let got = sub.poll();
        assert_eq!(got.len(), 1, "seq 1 released despite seq 0 being dropped");
        assert_eq!(got[0].entity_id, 101);
    }

    #[test]
    fn dropped_ticket_before_earlier_submit_still_orders() {
        // A dropped middle ticket must not block a later one, and an earlier real submit still
        // leads. Reserve 0,1,2; drop 1 (empty); submit 2 then 0. Expect [A(0), C(2)].
        let b = Arc::new(ChangefeedBroadcaster::new());
        let sub = b.subscribe(ChangeFilter::all()).unwrap();

        let t0 = b.reserve_emit_ticket();
        let t1 = b.reserve_emit_ticket();
        let t2 = b.reserve_emit_ticket();

        drop(t1); // seq 1 empty-released (parked until seq 0 releases)
        t2.submit(vec![record(
            102,
            102,
            EntityKind::Node,
            ChangeType::Created,
            "P",
            102,
        )]);
        // Nothing yet: seq 0 still outstanding.
        assert!(sub.poll().is_empty());

        t0.submit(vec![record(
            100,
            100,
            EntityKind::Node,
            ChangeType::Created,
            "P",
            100,
        )]);
        let got = sub.poll();
        // seq 0 -> A, seq 1 -> empty (dropped), seq 2 -> C.
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].entity_id, 100);
        assert_eq!(got[1].entity_id, 102);
    }

    #[test]
    fn set_config_updates_max() {
        let b = Arc::new(ChangefeedBroadcaster::new());
        b.set_config(ChangefeedConfig {
            max_subscriptions: 1,
            buffer_capacity: 4,
        });
        let _s1 = b.subscribe(ChangeFilter::all()).unwrap();
        assert!(b.subscribe(ChangeFilter::all()).is_err());
    }
}
