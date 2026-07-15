//! Pre-anchor hook machinery for historical storage.
//!
//! Pre-anchor hooks are the integration point between graph anchors and
//! external synchronized snapshots (e.g. temporal vector indexes). A hook is
//! invoked **before** an anchor is stored so it can create a snapshot and
//! return a snapshot id that is persisted atomically with the anchor, giving
//! strong consistency for provenance tracking between graph versioning and
//! vector indexing.
//!
//! This module hardens hook execution (Issue #3525, follow-up from #354):
//!
//! * **Multiple hooks per entity kind.** Each entity kind (node/edge) owns an
//!   ordered `Vec` of hooks instead of a single `Option`. Hooks run in
//!   registration order; the snapshot id slot is single, so the **last** hook
//!   that returns `Ok(Some(id))` wins (documented last-writer-wins). A failure
//!   (`Err`), panic, or timeout of one hook is isolated and never prevents the
//!   remaining hooks from running (partial-failure semantics).
//! * **Panic isolation.** Every hook invocation is wrapped in
//!   [`std::panic::catch_unwind`]. A panicking hook is caught and degraded
//!   exactly like an `Err` return — the anchor is still created (without a
//!   snapshot id from that hook) and the write path is never unwound.
//! * **Bounded execution (timeout).** With a configured timeout, each hook runs
//!   on a detached worker thread while the caller waits on a channel with
//!   `recv_timeout`. If the deadline elapses the caller stops waiting, degrades
//!   gracefully, and records a timeout. See the locking note below.
//!
//! ## Locking
//!
//! Hooks are invoked from `add_node_version` / `add_edge_version`, which run
//! while the caller holds the `historical` write lock (see the lock-order
//! contract in `CLAUDE.md`). The hardening here never *increases* how long that
//! lock is held relative to the previous behavior:
//!
//! * **No timeout configured (default):** the hook runs inline under the lock,
//!   exactly as before — identical lock-hold profile, plus panic isolation.
//! * **Timeout configured:** the caller blocks for at most `timeout` before
//!   giving up, so a hung hook now holds `historical` for a *bounded* duration
//!   instead of forever. This is strictly better than the previous unbounded
//!   behavior.
//!
//! Rust cannot safely cancel a running closure, so a timed-out hook thread is
//! **detached** and may run to completion in the background; "timeout" means
//! "the write path stops waiting and degrades", not "the hook is killed". Hooks
//! must not call back into `historical`, `wal`, or `current_timestamp` (the
//! lock-order contract), so the detached thread cannot deadlock against the
//! write path.

use crate::core::property::PropertyMap;
use crate::core::temporal::Timestamp;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::Duration;

/// Pre-anchor hook for creating snapshots before anchor storage.
///
/// This hook is called **before** storing an anchor to create synchronized
/// snapshots and return a snapshot id to be stored atomically with the anchor.
/// This enables strong consistency for provenance tracking between graph
/// versioning and vector indexing.
///
/// # Arguments
///
/// * `entity_type` - Type of entity (`"node"` or `"edge"`)
/// * `entity_id` - ID of the entity as `u64`
/// * `timestamp` - Transaction timestamp when the anchor is being created
/// * `properties` - Property map that will be stored in the anchor
///
/// # Returns
///
/// * `Ok(Some(snapshot_id))` - Snapshot created successfully, link to anchor
/// * `Ok(None)` - No snapshot needed or empty index
/// * `Err(e)` - Snapshot creation failed (anchor is still created — graceful
///   degradation). A **panic** inside the hook is caught and treated the same
///   way as `Err`.
///
/// # Panics / blocking
///
/// A hook must be panic-tolerant-by-design from the caller's perspective: a
/// panic is caught and degraded, never propagated into the write path. A hook
/// should also avoid blocking indefinitely; configure a timeout via
/// [`HistoricalStorage::set_pre_anchor_hook_timeout`](super::HistoricalStorage::set_pre_anchor_hook_timeout)
/// to bound execution. A hook must never call back into `historical`, `wal`, or
/// `current_timestamp` (lock-order contract).
///
/// # Examples
///
/// ```ignore
/// use aletheiadb::storage::historical::PreAnchorHook;
/// use std::sync::Arc;
///
/// let hook: PreAnchorHook = Arc::new(|entity_type, entity_id, timestamp, properties| {
///     // Create vector snapshot before anchor is stored
///     let snapshot_id = create_vector_snapshot(timestamp, properties)?;
///     Ok(Some(snapshot_id))
/// });
/// ```
pub type PreAnchorHook = Arc<
    dyn Fn(
            /* entity_type */ &str,
            /* entity_id */ u64,
            /* timestamp */ Timestamp,
            /* properties */ &PropertyMap,
        ) -> crate::core::error::Result<Option<usize>>
        + Send
        + Sync,
>;

/// Context for pre-anchor hook invocation.
///
/// Groups the parameters needed to invoke a pre-anchor hook, improving
/// readability and reducing the number of function parameters.
pub(crate) struct AnchorHookContext<'a> {
    /// Entity type (`"node"` or `"edge"`)
    pub(crate) entity_type: &'a str,
    /// Entity ID as `u64`
    pub(crate) entity_id: u64,
    /// Transaction timestamp
    pub(crate) timestamp: Timestamp,
    /// Properties being stored in the anchor
    pub(crate) properties: &'a PropertyMap,
}

/// Observability counters for pre-anchor hook execution.
///
/// All counters are monotonically increasing and updated with `Relaxed`
/// ordering (they are advisory metrics, not synchronization). Read a stable
/// point-in-time view via [`HookMetrics::snapshot`].
#[derive(Debug, Default)]
pub(crate) struct HookMetrics {
    /// Total hook invocations attempted (one per hook per anchor).
    invocations: AtomicU64,
    /// Invocations that completed successfully (`Ok(Some)` or `Ok(None)`).
    successes: AtomicU64,
    /// Invocations that returned `Err(_)`.
    failures: AtomicU64,
    /// Invocations that panicked (caught by `catch_unwind`).
    panics: AtomicU64,
    /// Invocations that exceeded the configured timeout.
    timeouts: AtomicU64,
}

impl HookMetrics {
    /// Take a consistent-enough snapshot of the current counters.
    pub(crate) fn snapshot(&self) -> HookMetricsSnapshot {
        HookMetricsSnapshot {
            invocations: self.invocations.load(Ordering::Relaxed),
            successes: self.successes.load(Ordering::Relaxed),
            failures: self.failures.load(Ordering::Relaxed),
            panics: self.panics.load(Ordering::Relaxed),
            timeouts: self.timeouts.load(Ordering::Relaxed),
        }
    }
}

/// Point-in-time snapshot of `HookMetrics`, returned by
/// [`HistoricalStorage::hook_metrics`](super::HistoricalStorage::hook_metrics).
///
/// Lets a caller observe how many pre-anchor hook invocations succeeded,
/// failed, panicked, or timed out — the observability surface for graceful
/// degradation (Issue #3525).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct HookMetricsSnapshot {
    /// Total hook invocations attempted.
    pub invocations: u64,
    /// Invocations that completed successfully (`Ok(Some)` or `Ok(None)`).
    pub successes: u64,
    /// Invocations that returned an error and were degraded.
    pub failures: u64,
    /// Invocations that panicked and were caught/degraded.
    pub panics: u64,
    /// Invocations that exceeded the configured timeout and were degraded.
    pub timeouts: u64,
}

/// Outcome of a single hook invocation, already reduced to `Send`-friendly
/// data so it can cross the watchdog channel without requiring the error type
/// to be `Send`.
enum HookOutcome {
    /// Hook returned `Ok(Some(id))`.
    Snapshot(usize),
    /// Hook returned `Ok(None)`.
    NoSnapshot,
    /// Hook returned `Err(_)`; message preserved for logging.
    Error(String),
    /// Hook panicked and was caught by `catch_unwind`.
    Panic,
    /// Hook did not complete within the configured timeout.
    Timeout,
}

/// Run all registered pre-anchor hooks in order and return the resolved
/// snapshot id (if any) to store on the anchor.
///
/// Hooks run in registration order. Each invocation is isolated: a failure,
/// panic, or timeout of one hook is recorded and logged but does not prevent
/// later hooks from running. Because the anchor has a single snapshot-id slot,
/// the **last** hook that returns `Ok(Some(id))` wins.
pub(crate) fn run_pre_anchor_hooks(
    hooks: &[PreAnchorHook],
    ctx: &AnchorHookContext<'_>,
    timeout: Option<Duration>,
    metrics: &HookMetrics,
) -> Option<usize> {
    let mut snapshot_id: Option<usize> = None;

    for hook in hooks {
        metrics.invocations.fetch_add(1, Ordering::Relaxed);

        let outcome = match timeout {
            None => invoke_inline(hook, ctx),
            Some(t) => invoke_with_timeout(hook, ctx, t),
        };

        match outcome {
            HookOutcome::Snapshot(id) => {
                metrics.successes.fetch_add(1, Ordering::Relaxed);
                // Last-writer-wins across multiple hooks.
                snapshot_id = Some(id);
                #[cfg(feature = "observability")]
                tracing::debug!(
                    "Pre-anchor hook returned snapshot id {} for {} {}",
                    id,
                    ctx.entity_type,
                    ctx.entity_id
                );
            }
            HookOutcome::NoSnapshot => {
                metrics.successes.fetch_add(1, Ordering::Relaxed);
                #[cfg(feature = "observability")]
                tracing::debug!(
                    "Pre-anchor hook returned None for {} {} (no snapshot needed)",
                    ctx.entity_type,
                    ctx.entity_id
                );
            }
            HookOutcome::Error(_message) => {
                metrics.failures.fetch_add(1, Ordering::Relaxed);
                #[cfg(feature = "observability")]
                tracing::warn!(
                    "Pre-anchor hook failed for {} {} at timestamp {}: {} (anchor will still be created)",
                    ctx.entity_type,
                    ctx.entity_id,
                    ctx.timestamp,
                    _message
                );
            }
            HookOutcome::Panic => {
                metrics.panics.fetch_add(1, Ordering::Relaxed);
                #[cfg(feature = "observability")]
                tracing::warn!(
                    "Pre-anchor hook panicked for {} {} at timestamp {} (panic isolated; anchor will still be created)",
                    ctx.entity_type,
                    ctx.entity_id,
                    ctx.timestamp
                );
            }
            HookOutcome::Timeout => {
                metrics.timeouts.fetch_add(1, Ordering::Relaxed);
                #[cfg(feature = "observability")]
                tracing::warn!(
                    "Pre-anchor hook timed out for {} {} at timestamp {} (hook detached; anchor will still be created)",
                    ctx.entity_type,
                    ctx.entity_id,
                    ctx.timestamp
                );
            }
        }
    }

    snapshot_id
}

/// Invoke a hook on the calling thread, isolating panics via `catch_unwind`.
fn invoke_inline(hook: &PreAnchorHook, ctx: &AnchorHookContext<'_>) -> HookOutcome {
    // SAFETY (unwind-safety): the closure only *reads* borrowed data
    // (`entity_type`, `properties`); it mutates nothing that could be left in a
    // broken state if the hook panics, and the caller updates `VersionData`
    // only on a successful `Snapshot` outcome. `AssertUnwindSafe` is therefore
    // sound here.
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        hook(
            ctx.entity_type,
            ctx.entity_id,
            ctx.timestamp,
            ctx.properties,
        )
    }));
    match result {
        Ok(Ok(Some(id))) => HookOutcome::Snapshot(id),
        Ok(Ok(None)) => HookOutcome::NoSnapshot,
        Ok(Err(e)) => HookOutcome::Error(e.to_string()),
        Err(_) => HookOutcome::Panic,
    }
}

/// Invoke a hook on a detached watchdog thread, bounding the caller's wait to
/// `timeout`. On timeout the hook thread is left running (detached) and the
/// caller degrades gracefully.
fn invoke_with_timeout(
    hook: &PreAnchorHook,
    ctx: &AnchorHookContext<'_>,
    timeout: Duration,
) -> HookOutcome {
    // The worker thread needs `'static` owned data, so clone the small context.
    // This only happens for anchors (not every write) and only when a timeout
    // is configured, so the extra `PropertyMap` clone is acceptable.
    let hook_for_thread = Arc::clone(hook);
    let entity_type = ctx.entity_type.to_string();
    let entity_id = ctx.entity_id;
    let timestamp = ctx.timestamp;
    let properties = ctx.properties.clone();

    let (tx, rx) = mpsc::channel::<HookOutcome>();

    let spawn = std::thread::Builder::new()
        .name("aletheia-pre-anchor-hook".to_string())
        .spawn(move || {
            // SAFETY (unwind-safety): see `invoke_inline`. The worker owns its
            // captured data and shares nothing mutable with the caller.
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                hook_for_thread(&entity_type, entity_id, timestamp, &properties)
            }));
            let outcome = match result {
                Ok(Ok(Some(id))) => HookOutcome::Snapshot(id),
                Ok(Ok(None)) => HookOutcome::NoSnapshot,
                Ok(Err(e)) => HookOutcome::Error(e.to_string()),
                Err(_) => HookOutcome::Panic,
            };
            // If the caller already timed out and dropped `rx`, the send fails
            // harmlessly and the thread simply exits.
            let _ = tx.send(outcome);
        });

    if spawn.is_err() {
        // Could not spawn a watchdog thread (resource exhaustion). Fall back to
        // inline execution so the hook is not silently skipped; panics are still
        // isolated, though this particular invocation is not time-bounded.
        return invoke_inline(hook, ctx);
    }

    match rx.recv_timeout(timeout) {
        Ok(outcome) => outcome,
        Err(RecvTimeoutError::Timeout) => HookOutcome::Timeout,
        // Sender dropped without sending — the worker unwound past the send in a
        // way `catch_unwind` did not capture (should not happen). Treat as a
        // caught failure so the write still degrades gracefully.
        Err(RecvTimeoutError::Disconnected) => HookOutcome::Panic,
    }
}
