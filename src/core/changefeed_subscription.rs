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

/// Default maximum concurrently-live subscriptions **per authenticated principal**
/// (Issue #3678).
///
/// Chosen as `DEFAULT_MAX_SUBSCRIPTIONS / 8` so a single principal cannot exhaust
/// the global fan-out cap and starve others: with the default global cap of 128,
/// at least 8 distinct principals must be active before the global cap binds. The
/// value is deliberately generous for a well-behaved client (few live SSE streams
/// / long-polls) while still enforcing fairness.
pub const DEFAULT_MAX_PER_PRINCIPAL_SUBSCRIPTIONS: usize = 16;

/// Caps governing a [`ChangefeedBroadcaster`].
///
/// The bounds protect memory and fairness: `max_subscriptions` caps total fan-out
/// breadth, `buffer_capacity` caps how far a single slow consumer may fall behind
/// before it is disconnected (Lagged), and `max_subscriptions_per_principal` (plus
/// optional `per_principal_overrides`, Issue #3678) caps how many concurrent
/// subscriptions any one authenticated principal may hold so no principal can
/// exhaust the global cap and starve others.
///
/// The per-principal cap is enforced **only** when a subscription is created with a
/// principal key (the MCP/HTTP changefeed surfaces always supply one — the
/// authenticated principal id, or the shared `"anonymous"` bucket in anonymous
/// mode). The bare embedded [`ChangefeedBroadcaster::subscribe`] /
/// [`crate::db::AletheiaDB::subscribe_changes`] path carries no principal and is
/// unaffected — omitting the principal reproduces the pre-#3678 behavior exactly.
/// Marked `#[non_exhaustive]` (Issue #3678): the per-principal fields dropped
/// the previous `Copy` derive (the struct now holds a `HashMap`), so external
/// callers must construct it via [`ChangefeedConfig::default`] +
/// field updates rather than a bare struct literal, and future field additions
/// stay non-breaking.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(default))]
#[non_exhaustive]
pub struct ChangefeedConfig {
    /// Maximum number of concurrently-live subscriptions.
    pub max_subscriptions: usize,
    /// Per-subscription buffer capacity, in events, before overflow → Lagged.
    pub buffer_capacity: usize,
    /// Default maximum concurrently-live subscriptions per authenticated principal
    /// (Issue #3678). Applies to every principal without an explicit override.
    pub max_subscriptions_per_principal: usize,
    /// Per-principal overrides, keyed by principal id (Issue #3678). A principal
    /// listed here uses its override limit instead of
    /// `max_subscriptions_per_principal`. The shared anonymous bucket can be
    /// tuned via the `"anonymous"` key.
    ///
    /// An override of **`0` is an intentional block**: that principal can never
    /// hold a changefeed subscription (not even its first), because the quota
    /// check rejects any subscribe once `current >= limit` and `current` starts
    /// at `0`. This is deliberately **asymmetric** with the default
    /// `max_subscriptions_per_principal`, which is floored at `1` by
    /// [`ChangefeedBroadcaster::with_config`]/`set_config` (a zero *default*
    /// would disable the feed for everyone and is treated as a misconfiguration,
    /// whereas a zero *override* is a per-principal deny switch). Override values
    /// are stored verbatim and never floored.
    pub per_principal_overrides: HashMap<String, usize>,
}

impl Default for ChangefeedConfig {
    fn default() -> Self {
        ChangefeedConfig {
            max_subscriptions: DEFAULT_MAX_SUBSCRIPTIONS,
            buffer_capacity: DEFAULT_BUFFER_CAPACITY,
            max_subscriptions_per_principal: DEFAULT_MAX_PER_PRINCIPAL_SUBSCRIPTIONS,
            per_principal_overrides: HashMap::new(),
        }
    }
}

impl ChangefeedConfig {
    /// Fluent builders over [`ChangefeedConfig::default`] (Issue #3678). Because
    /// the struct is `#[non_exhaustive]`, out-of-crate callers cannot use a
    /// struct literal (with or without functional-record-update); these
    /// consuming setters are the ergonomic construction path.
    ///
    /// Set the global maximum number of concurrently-live subscriptions.
    #[must_use]
    pub fn with_max_subscriptions(mut self, max_subscriptions: usize) -> Self {
        self.max_subscriptions = max_subscriptions;
        self
    }

    /// Set the per-subscription buffer capacity (events retained before Lagged).
    #[must_use]
    pub fn with_buffer_capacity(mut self, buffer_capacity: usize) -> Self {
        self.buffer_capacity = buffer_capacity;
        self
    }

    /// Set the default per-principal subscription cap (applies to every
    /// principal without an explicit override).
    #[must_use]
    pub fn with_max_subscriptions_per_principal(mut self, max: usize) -> Self {
        self.max_subscriptions_per_principal = max;
        self
    }

    /// Add or replace a per-principal override. A limit of `0` blocks the
    /// principal entirely (see the field docs).
    #[must_use]
    pub fn with_per_principal_override(
        mut self,
        principal: impl Into<String>,
        limit: usize,
    ) -> Self {
        self.per_principal_overrides.insert(principal.into(), limit);
        self
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
    /// The per-principal quota bucket this subscription counts against, if any
    /// (Issue #3678). `None` for the bare embedded path (no principal accounting).
    /// Stored here so [`ChangefeedBroadcaster::deregister`] can find and decrement
    /// the correct principal counter from the subscription id alone — the single
    /// leak-proof release anchor for every drop cause (unsubscribe, disconnect,
    /// expiry).
    principal_key: Option<String>,
    inner: Mutex<SubscriberInner>,
    cvar: Condvar,
    /// Async-awaitable readiness signal (Issue #3673), present only when a tokio
    /// runtime is compiled in (the changefeed HTTP/MCP surfaces). It mirrors
    /// `cvar` for the *async* waiter [`Subscription::recv_async`]: a permit-storing
    /// `Notify` so a wakeup that races the buffer check is never lost. The
    /// synchronous [`Subscription::recv_timeout`] Condvar path is unchanged and
    /// remains the only wait mechanism in `--no-default-features` (no-tokio)
    /// builds, so embedded callers keep the dependency-free long-poll.
    #[cfg(any(feature = "mcp-server", feature = "http-server"))]
    notify: tokio::sync::Notify,
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
        let mut became_lagged = false;
        for record in records {
            if !self.filter.matches(record) {
                continue;
            }
            if inner.queue.len() >= self.buffer_capacity {
                // Slow consumer: disconnect it rather than back-pressure the writer.
                inner.lagged = true;
                became_lagged = true;
                break;
            }
            inner.queue.push_back(record.clone());
            pushed = true;
        }
        drop(inner);
        if pushed {
            self.cvar.notify_all();
        }
        // Wake the async waiter (Issue #3673) on new data OR on the lagged
        // transition, so a lagged subscription's `recv_async` resolves promptly
        // to its `RecvError::Lagged` instead of waiting out the whole timeout.
        // The sync Condvar path above is left byte-identical (`if pushed`) so
        // delivery semantics for embedded callers are unchanged.
        #[cfg(any(feature = "mcp-server", feature = "http-server"))]
        if pushed || became_lagged {
            self.notify.notify_one();
        }
        #[cfg(not(any(feature = "mcp-server", feature = "http-server")))]
        let _ = became_lagged;
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

    /// Event-driven async counterpart to [`recv_timeout`](Self::recv_timeout)
    /// (Issue #3673).
    ///
    /// Awaits up to `timeout` for buffered events, parking on a
    /// `tokio::sync::Notify` instead of a blocking `Condvar` wait, so a suspended
    /// long-poll consumes **zero** runtime worker threads (fixing the
    /// `block_in_place` / `spawn_blocking` worker-pin the MCP/HTTP changefeed
    /// surfaces previously relied on). The drain, resume-token, timeout-empty, and
    /// `RecvError::Lagged` semantics are **identical** to `recv_timeout` — only the
    /// wait mechanism differs — and the surface owns the future, so dropping it on
    /// client disconnect drops this `Subscription` immediately (prompt slot
    /// release).
    ///
    /// Returns:
    /// - `Ok(events)` with one or more events as soon as any are buffered;
    /// - `Ok(vec![])` if the timeout elapses with nothing buffered;
    /// - `Err(RecvError::Lagged { resume_token })` once the buffer has been drained
    ///   and the subscription was disconnected for overflow.
    #[cfg(any(feature = "mcp-server", feature = "http-server"))]
    pub async fn recv_async(
        &self,
        timeout: Duration,
    ) -> std::result::Result<Vec<ChangeRecord>, RecvError> {
        let deadline = Instant::now().checked_add(timeout);
        loop {
            // Create and REGISTER the wakeup future *before* inspecting the buffer.
            // `Notified::enable()` arms the waiter (consuming an already-stored
            // permit if one exists), so a `notify_one` racing the buffer check
            // below is never lost — the permit-storing lost-wakeup guard.
            let notified = self.state.notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();

            // Drain / lagged check — identical to `recv_timeout`. The lock is
            // released (scope end) before any `.await`, so the future stays `Send`
            // and never holds the subscriber mutex across a suspension point.
            {
                let mut inner = self.state.inner.lock().unwrap_or_else(|e| e.into_inner());
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
            }

            let remaining = match deadline {
                Some(d) => d.saturating_duration_since(Instant::now()),
                None => timeout,
            };
            if remaining.is_zero() {
                return Ok(Vec::new());
            }

            tokio::select! {
                // Woken by a push (or a lagged transition): loop to drain.
                _ = notified => {}
                // Deadline elapsed: re-check once; honestly time out only if still
                // nothing buffered and not lagged.
                _ = tokio::time::sleep(remaining) => {
                    let inner = self.state.inner.lock().unwrap_or_else(|e| e.into_inner());
                    if inner.queue.is_empty() && !inner.lagged {
                        return Ok(Vec::new());
                    }
                }
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

/// The registry guarded by the broadcaster's `subscribers` write lock.
///
/// Folding the per-principal quota accounting (Issue #3678) into the **same**
/// lock that guards the global subscriber map makes "check global cap → check
/// per-principal quota → insert → bump counters" one atomic critical section, so
/// two concurrent subscribes for the same principal can never both observe
/// `count-1` and over-admit (no TOCTOU / false-exhaustion). The per-principal
/// limit config lives here too so a live [`ChangefeedBroadcaster::set_config`]
/// updates it atomically with the counters.
struct Registry {
    /// Live subscriptions by id.
    subscribers: HashMap<u64, Arc<SubscriberState>>,
    /// Live subscription count per principal bucket (Issue #3678). An entry is
    /// removed when its count reaches zero so the map never grows unbounded with
    /// churned-through principals.
    per_principal_counts: HashMap<String, usize>,
    /// Default per-principal cap (Issue #3678).
    max_per_principal: usize,
    /// Per-principal cap overrides, keyed by principal id (Issue #3678).
    per_principal_overrides: HashMap<String, usize>,
}

impl Registry {
    /// The effective per-principal cap for `key` (its override, else the default).
    fn limit_for(&self, key: &str) -> usize {
        self.per_principal_overrides
            .get(key)
            .copied()
            .unwrap_or(self.max_per_principal)
    }
}

/// Fan-out hub for the push changefeed.
///
/// Held as an `Arc` on the database; [`subscribe`](Self::subscribe) hands out
/// [`Subscription`]s and the commit path reserves an ordered ticket
/// ([`reserve_emit_ticket`](Self::reserve_emit_ticket)) then submits each committed
/// transaction's records through it (which releases them to subscribers in cursor order).
pub struct ChangefeedBroadcaster {
    subscribers: RwLock<Registry>,
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
            subscribers: RwLock::new(Registry {
                subscribers: HashMap::new(),
                per_principal_counts: HashMap::new(),
                max_per_principal: config.max_subscriptions_per_principal.max(1),
                per_principal_overrides: config.per_principal_overrides.clone(),
            }),
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

    /// Update the caps. Affects the `max_subscriptions` and per-principal checks
    /// immediately and the `buffer_capacity` of **future** subscriptions (existing
    /// ones keep the capacity they were created with).
    pub fn set_config(&self, config: ChangefeedConfig) {
        self.max_subscriptions
            .store(config.max_subscriptions.max(1), Ordering::Relaxed);
        self.buffer_capacity
            .store(config.buffer_capacity.max(1), Ordering::Relaxed);
        // Per-principal limits live under the registry lock so they update
        // atomically with the counters they govern (Issue #3678).
        let mut reg = self.subscribers.write().unwrap_or_else(|e| e.into_inner());
        reg.max_per_principal = config.max_subscriptions_per_principal.max(1);
        reg.per_principal_overrides = config.per_principal_overrides.clone();
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
        self.subscribe_with_baseline_and_principal(filter, baseline, None)
    }

    /// Register a new subscription, seeding its resume `baseline` and accounting it
    /// against the per-principal quota bucket `principal_key` (Issue #3678).
    ///
    /// When `principal_key` is `Some(key)` the per-principal cap is enforced
    /// **atomically** with the global cap under one write lock — see [`Registry`].
    /// On breach a [`StorageError::PrincipalQuotaExceeded`] is returned (mapping to
    /// the MCP/HTTP `RESOURCE_EXHAUSTED` / `retriable:true` envelope). When
    /// `principal_key` is `None` no per-principal accounting happens (the bare
    /// embedded path), reproducing the pre-#3678 behavior exactly.
    ///
    /// The counter is incremented here and decremented **only** in
    /// [`deregister`](Self::deregister) via the single [`Subscription`] `Drop`
    /// hook, so a slot is released on every drop cause (unsubscribe, client
    /// disconnect, long-poll return, expiry) with no leak.
    ///
    /// # Errors
    ///
    /// [`StorageError::CapacityExceeded`] at the global cap, or
    /// [`StorageError::PrincipalQuotaExceeded`] at the principal's cap.
    pub fn subscribe_with_baseline_and_principal(
        self: &Arc<Self>,
        filter: ChangeFilter,
        baseline: Option<String>,
        principal_key: Option<String>,
    ) -> Result<Subscription> {
        let mut reg = self.subscribers.write().unwrap_or_else(|e| e.into_inner());
        let max = self.max_subscriptions.load(Ordering::Relaxed);
        if reg.subscribers.len() >= max {
            return Err(StorageError::CapacityExceeded {
                resource: "changefeed subscriptions".to_string(),
                current: reg.subscribers.len(),
                limit: max,
            }
            .into());
        }
        // Per-principal fairness check, atomic with the global check above and the
        // insert below (Issue #3678). Both caps are enforced; whichever binds
        // first wins.
        if let Some(key) = &principal_key {
            let current = reg.per_principal_counts.get(key).copied().unwrap_or(0);
            let limit = reg.limit_for(key);
            if current >= limit {
                drop(reg);
                #[cfg(feature = "observability")]
                crate::observability::METRICS.inc_changefeed_quota_rejection();
                return Err(StorageError::PrincipalQuotaExceeded {
                    principal: key.clone(),
                    current,
                    limit,
                }
                .into());
            }
        }
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let state = Arc::new(SubscriberState {
            id,
            filter,
            buffer_capacity: self.buffer_capacity.load(Ordering::Relaxed),
            principal_key: principal_key.clone(),
            inner: Mutex::new(SubscriberInner {
                queue: VecDeque::new(),
                last_delivered: baseline,
                lagged: false,
            }),
            cvar: Condvar::new(),
            #[cfg(any(feature = "mcp-server", feature = "http-server"))]
            notify: tokio::sync::Notify::new(),
        });
        reg.subscribers.insert(id, Arc::clone(&state));
        if let Some(key) = principal_key {
            *reg.per_principal_counts.entry(key).or_insert(0) += 1;
        }
        // Release: publish the subscriber insertion before the count becomes visible, so a
        // subsequent commit's Acquire gate load observes this subscriber (review F10).
        self.subscriber_count
            .store(reg.subscribers.len(), Ordering::Release);
        drop(reg);
        #[cfg(feature = "observability")]
        crate::observability::METRICS.inc_changefeed_active();
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
        let reg = self.subscribers.read().unwrap_or_else(|e| e.into_inner());
        for state in reg.subscribers.values() {
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
            .subscribers
            .len()
    }

    /// The number of currently-live subscriptions held by `principal_key`
    /// (Issue #3678). `0` for an unknown / fully-released principal.
    pub fn principal_subscription_count(&self, principal_key: &str) -> usize {
        self.subscribers
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .per_principal_counts
            .get(principal_key)
            .copied()
            .unwrap_or(0)
    }

    /// A snapshot of live subscription counts per principal bucket, sorted by
    /// principal id for stable output (Issue #3678). Feeds the authenticated
    /// `database_stats` per-principal breakdown (never `/metrics`, which stays
    /// bounded-label-only).
    pub fn per_principal_counts(&self) -> Vec<(String, usize)> {
        let reg = self.subscribers.read().unwrap_or_else(|e| e.into_inner());
        let mut out: Vec<(String, usize)> = reg
            .per_principal_counts
            .iter()
            .map(|(k, v)| (k.clone(), *v))
            .collect();
        out.sort_by(|a, b| a.0.cmp(&b.0));
        out
    }

    /// A coherent snapshot of the total live-subscription count and the
    /// per-principal breakdown taken under a **single** `subscribers` read lock
    /// (Issue #3678). Prevents the `database_stats` aggregate and per-principal
    /// rows from straddling two instants under concurrent subscribe/deregister:
    /// with separate `subscription_count()` + `per_principal_counts()` calls a
    /// commit between them could make the total inconsistent with the rows.
    /// The per-principal vec is sorted by principal id for stable output.
    pub fn subscription_count_and_per_principal(&self) -> (usize, Vec<(String, usize)>) {
        let reg = self.subscribers.read().unwrap_or_else(|e| e.into_inner());
        let total = reg.subscribers.len();
        let mut out: Vec<(String, usize)> = reg
            .per_principal_counts
            .iter()
            .map(|(k, v)| (k.clone(), *v))
            .collect();
        out.sort_by(|a, b| a.0.cmp(&b.0));
        (total, out)
    }

    fn deregister(&self, id: u64) {
        let mut reg = self.subscribers.write().unwrap_or_else(|e| e.into_inner());
        if let Some(state) = reg.subscribers.remove(&id) {
            self.subscriber_count
                .store(reg.subscribers.len(), Ordering::Release);
            // Decrement the per-principal counter (Issue #3678). This is the ONLY
            // decrement site — reached via the single `Subscription::Drop` hook for
            // every release cause — so a slot can never leak. Written with the
            // `is_some_and` combinator (not a `&&`-`let`-chain) to stay on stable
            // combinators; the intermediate `let` also keeps clippy's
            // `collapsible_if` from suggesting a re-collapse into a let-chain.
            if let Some(key) = state.principal_key.as_ref() {
                let released_to_zero = reg.per_principal_counts.get_mut(key).is_some_and(|count| {
                    *count -= 1;
                    *count == 0
                });
                if released_to_zero {
                    reg.per_principal_counts.remove(key);
                }
            }
            drop(reg);
            #[cfg(feature = "observability")]
            crate::observability::METRICS.dec_changefeed_active();
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
            ..ChangefeedConfig::default()
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
            ..ChangefeedConfig::default()
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
            ..ChangefeedConfig::default()
        });
        let _s1 = b.subscribe(ChangeFilter::all()).unwrap();
        assert!(b.subscribe(ChangeFilter::all()).is_err());
    }

    // ── Per-principal subscription quota (Issue #3678) ────────────────────────

    /// Subscribe with an explicit principal key.
    fn sub_as(b: &Arc<ChangefeedBroadcaster>, principal: &str) -> Result<Subscription> {
        b.subscribe_with_baseline_and_principal(
            ChangeFilter::all(),
            None,
            Some(principal.to_string()),
        )
    }

    fn quota_config(default_per_principal: usize, max_subscriptions: usize) -> ChangefeedConfig {
        ChangefeedConfig {
            max_subscriptions,
            buffer_capacity: 16,
            max_subscriptions_per_principal: default_per_principal,
            per_principal_overrides: HashMap::new(),
        }
    }

    #[test]
    fn per_principal_cap_enforced_below_global() {
        // default per-principal = 2, global far above: the 3rd subscribe for the
        // SAME principal is refused with PrincipalQuotaExceeded even though the
        // global cap is nowhere near.
        let b = Arc::new(ChangefeedBroadcaster::with_config(quota_config(2, 100)));
        let _a1 = sub_as(&b, "alice").unwrap();
        let _a2 = sub_as(&b, "alice").unwrap();
        let err = sub_as(&b, "alice").unwrap_err();
        match err {
            crate::core::error::Error::Storage(StorageError::PrincipalQuotaExceeded {
                principal,
                current,
                limit,
            }) => {
                assert_eq!(principal, "alice");
                assert_eq!(current, 2);
                assert_eq!(limit, 2);
            }
            other => panic!("expected PrincipalQuotaExceeded, got {other:?}"),
        }
        assert_eq!(b.subscription_count(), 2, "the rejected sub is not counted");
        assert_eq!(b.principal_subscription_count("alice"), 2);
    }

    #[test]
    fn two_principals_isolated() {
        // alice at her cap does not affect bob; counts are tracked independently.
        let b = Arc::new(ChangefeedBroadcaster::with_config(quota_config(2, 100)));
        let _a1 = sub_as(&b, "alice").unwrap();
        let _a2 = sub_as(&b, "alice").unwrap();
        assert!(sub_as(&b, "alice").is_err(), "alice at cap");
        // bob is unaffected.
        let _b1 = sub_as(&b, "bob").unwrap();
        let _b2 = sub_as(&b, "bob").unwrap();
        assert_eq!(b.principal_subscription_count("alice"), 2);
        assert_eq!(b.principal_subscription_count("bob"), 2);
    }

    #[test]
    fn per_principal_override_honored() {
        // override {alice: 5} over default 2: alice reaches 5, bob still capped at 2.
        let mut cfg = quota_config(2, 100);
        cfg.per_principal_overrides.insert("alice".to_string(), 5);
        let b = Arc::new(ChangefeedBroadcaster::with_config(cfg));
        let mut alice_subs = Vec::new();
        for _ in 0..5 {
            alice_subs.push(sub_as(&b, "alice").unwrap());
        }
        assert!(sub_as(&b, "alice").is_err(), "alice capped at override 5");
        assert_eq!(b.principal_subscription_count("alice"), 5);
        let _b1 = sub_as(&b, "bob").unwrap();
        let _b2 = sub_as(&b, "bob").unwrap();
        assert!(sub_as(&b, "bob").is_err(), "bob still at default 2");
    }

    #[test]
    fn drop_releases_per_principal_slot() {
        let b = Arc::new(ChangefeedBroadcaster::with_config(quota_config(2, 100)));
        let a1 = sub_as(&b, "alice").unwrap();
        let _a2 = sub_as(&b, "alice").unwrap();
        assert!(sub_as(&b, "alice").is_err());
        assert_eq!(b.principal_subscription_count("alice"), 2);
        drop(a1);
        assert_eq!(b.principal_subscription_count("alice"), 1);
        // A slot freed up: alice can subscribe again.
        let _a3 = sub_as(&b, "alice").unwrap();
        assert_eq!(b.principal_subscription_count("alice"), 2);
    }

    #[test]
    fn deregister_decrements_correct_principal() {
        let b = Arc::new(ChangefeedBroadcaster::with_config(quota_config(4, 100)));
        let _a1 = sub_as(&b, "alice").unwrap();
        let b1 = sub_as(&b, "bob").unwrap();
        let _b2 = sub_as(&b, "bob").unwrap();
        assert_eq!(b.principal_subscription_count("alice"), 1);
        assert_eq!(b.principal_subscription_count("bob"), 2);
        drop(b1);
        assert_eq!(
            b.principal_subscription_count("alice"),
            1,
            "alice untouched"
        );
        assert_eq!(b.principal_subscription_count("bob"), 1);
    }

    #[test]
    fn global_cap_still_wins_when_lower() {
        // global cap 1 below per-principal 10: the 2nd subscribe (any principal)
        // hits the GLOBAL cap, preserving the existing global behavior.
        let b = Arc::new(ChangefeedBroadcaster::with_config(quota_config(10, 1)));
        let _a1 = sub_as(&b, "alice").unwrap();
        let err = sub_as(&b, "bob").unwrap_err();
        match err {
            crate::core::error::Error::Storage(StorageError::CapacityExceeded {
                resource,
                limit,
                ..
            }) => {
                assert_eq!(resource, "changefeed subscriptions");
                assert_eq!(limit, 1);
            }
            other => panic!("expected global CapacityExceeded, got {other:?}"),
        }
    }

    #[test]
    fn none_principal_bypasses_quota() {
        // The bare embedded path (no principal) is unaffected by the per-principal
        // quota even below it (reproduces pre-#3678 behavior).
        let b = Arc::new(ChangefeedBroadcaster::with_config(quota_config(1, 100)));
        let _s1 = b.subscribe(ChangeFilter::all()).unwrap();
        let _s2 = b.subscribe(ChangeFilter::all()).unwrap();
        let _s3 = b.subscribe(ChangeFilter::all()).unwrap();
        assert_eq!(b.subscription_count(), 3);
        assert!(
            b.per_principal_counts().is_empty(),
            "no principal accounting for the bare path"
        );
    }

    #[test]
    fn rapid_connect_disconnect_churn_no_leak_no_false_exhaust() {
        // The load-bearing race test (AC d). N threads each subscribe→immediately
        // drop, many iterations, for one principal with cap k. Assert: (i) no
        // iteration is spuriously rejected while fewer than k are concurrently
        // live (no false-exhaustion), and (ii) after quiescence the principal's
        // counter is exactly 0 and a fresh `cap` subscribes succeed (no leak).
        use std::sync::Barrier;
        const THREADS: usize = 8;
        const ITERS: usize = 400;
        const CAP: usize = THREADS; // each thread holds at most 1 at a time → never exceeds CAP

        let b = Arc::new(ChangefeedBroadcaster::with_config(quota_config(
            CAP, 10_000,
        )));
        let barrier = Arc::new(Barrier::new(THREADS));
        let mut handles = Vec::new();
        for _ in 0..THREADS {
            let b = Arc::clone(&b);
            let barrier = Arc::clone(&barrier);
            handles.push(std::thread::spawn(move || {
                barrier.wait();
                for _ in 0..ITERS {
                    // With CAP == THREADS and each thread holding at most one live
                    // subscription at a time, the principal's live count can never
                    // exceed CAP, so a rejection here would be a FALSE exhaustion.
                    let sub = sub_as(&b, "churn")
                        .expect("subscribe within cap must not be falsely rejected");
                    drop(sub);
                }
            }));
        }
        for h in handles {
            h.join().unwrap();
        }

        // No leak: after quiescence the counter is exactly 0 (entry pruned).
        assert_eq!(
            b.principal_subscription_count("churn"),
            0,
            "no slot leaked after churn"
        );
        assert!(b.per_principal_counts().is_empty());
        // A fresh `cap` subscribes succeed (the quota is fully released).
        let mut fresh = Vec::new();
        for _ in 0..CAP {
            fresh.push(sub_as(&b, "churn").expect("fresh cap subscribes after churn"));
        }
        assert!(sub_as(&b, "churn").is_err(), "cap binds again after refill");
    }

    #[test]
    fn per_principal_override_zero_always_blocks() {
        // An override of 0 is an intentional per-principal deny switch: the very
        // first subscribe is rejected (current=0, limit=0 → 0 >= 0), unlike the
        // default which is floored at 1. Asymmetry documented on the field.
        let mut cfg = quota_config(16, 1000);
        cfg.per_principal_overrides.insert("blocked".to_string(), 0);
        let b = Arc::new(ChangefeedBroadcaster::with_config(cfg));
        let err = sub_as(&b, "blocked").unwrap_err();
        match err {
            crate::core::error::Error::Storage(StorageError::PrincipalQuotaExceeded {
                principal,
                current,
                limit,
            }) => {
                assert_eq!(principal, "blocked");
                assert_eq!(current, 0);
                assert_eq!(limit, 0, "override 0 blocks even the first subscribe");
            }
            other => panic!("expected PrincipalQuotaExceeded, got {other:?}"),
        }
        assert_eq!(b.principal_subscription_count("blocked"), 0);
        // A different principal is unaffected by another's 0 override.
        let _ok = sub_as(&b, "allowed").expect("un-overridden principal still admitted");
    }

    #[test]
    fn default_max_per_principal_is_16() {
        // Pin the shipped default so a silent change to the constant fails CI
        // (AC "default" as the shipped value, not just a mechanism).
        assert_eq!(DEFAULT_MAX_PER_PRINCIPAL_SUBSCRIPTIONS, 16);
        assert_eq!(
            ChangefeedConfig::default().max_subscriptions_per_principal,
            16
        );
        // And it is actually enforced at the default: a fresh broadcaster admits
        // exactly 16 for one principal, rejecting the 17th.
        let b = Arc::new(ChangefeedBroadcaster::new());
        let mut held = Vec::new();
        for _ in 0..16 {
            held.push(sub_as(&b, "def").expect("first 16 admitted at the default cap"));
        }
        assert!(
            sub_as(&b, "def").is_err(),
            "17th rejected at default cap 16"
        );
    }

    #[test]
    fn concurrent_over_admit_exactly_cap_wins() {
        // Over-admit direction under real contention (AC d complement): with a
        // small cap and many threads each *holding* its subscription, exactly
        // `CAP` subscribes win and every other gets PrincipalQuotaExceeded — the
        // atomic critical section admits no `count > CAP`.
        use std::sync::Barrier;
        const CAP: usize = 4;
        const THREADS: usize = 32;

        let b = Arc::new(ChangefeedBroadcaster::with_config(quota_config(
            CAP, 10_000,
        )));
        let barrier = Arc::new(Barrier::new(THREADS));
        let mut handles = Vec::new();
        for _ in 0..THREADS {
            let b = Arc::clone(&b);
            let barrier = Arc::clone(&barrier);
            handles.push(std::thread::spawn(move || -> Result<Subscription> {
                barrier.wait();
                sub_as(&b, "contended")
            }));
        }
        let mut winners = Vec::new();
        let mut rejections = 0usize;
        for h in handles {
            match h.join().unwrap() {
                Ok(sub) => winners.push(sub), // held alive → count stays at CAP
                Err(crate::core::error::Error::Storage(StorageError::PrincipalQuotaExceeded {
                    limit,
                    ..
                })) => {
                    assert_eq!(limit, CAP);
                    rejections += 1;
                }
                Err(other) => panic!("unexpected error under contention: {other:?}"),
            }
        }
        assert_eq!(winners.len(), CAP, "exactly CAP subscribes admitted");
        assert_eq!(
            rejections,
            THREADS - CAP,
            "all others rejected, none over-admitted"
        );
        assert_eq!(b.principal_subscription_count("contended"), CAP);
    }

    #[cfg(feature = "serde")]
    #[test]
    fn changefeed_config_serde_round_trip() {
        // A config with a populated per-principal override map round-trips
        // losslessly through JSON.
        let mut cfg = ChangefeedConfig::default()
            .with_max_subscriptions(200)
            .with_buffer_capacity(64)
            .with_max_subscriptions_per_principal(3);
        cfg = cfg
            .with_per_principal_override("vip", 10)
            .with_per_principal_override("blocked", 0);
        let json = serde_json::to_string(&cfg).expect("serialize");
        let back: ChangefeedConfig = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(cfg, back);

        // A legacy blob omitting the two #3678 fields back-fills the defaults via
        // `#[serde(default)]` (not 0), so an old config keeps working.
        let legacy = r#"{"max_subscriptions": 50, "buffer_capacity": 8}"#;
        let filled: ChangefeedConfig = serde_json::from_str(legacy).expect("legacy deserialize");
        assert_eq!(filled.max_subscriptions, 50);
        assert_eq!(filled.buffer_capacity, 8);
        assert_eq!(
            filled.max_subscriptions_per_principal, DEFAULT_MAX_PER_PRINCIPAL_SUBSCRIPTIONS,
            "missing per-principal cap defaults to 16, not 0"
        );
        assert!(filled.per_principal_overrides.is_empty());
    }
}
