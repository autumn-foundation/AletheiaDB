//! Per-IP rate-limit core (Lane B3).
//!
//! Fills `aletheia-server::security::rate_limit`. A single, **default-off**
//! constructor, [`governor_layer`], that turns a [`SecurityConfig`] into an
//! optional [`tower_governor::GovernorLayer`] — a real `tower::Layer` that
//! mounts on an autumn-0.5 / axum-0.8 router via `.layer()`.
//!
//! # ADR-0055 supersession
//!
//! ADR-0055 concluded that a custom tower/middleware rate limiter was
//! impractical under autumn 0.4, whose sealed `IntoAppLayer` rejected arbitrary
//! `tower::Layer`s. Under autumn 0.5 that bound is lifted: an arbitrary
//! `tower::Layer` — including `tower_governor::GovernorLayer` — mounts through
//! `.layer()`. Rate limiting therefore returns as a **default-off** governor
//! layer: `governor_layer` yields `None` unless the operator opts in via
//! [`SecurityConfig::rate_limit`], so default behavior is byte-for-byte today's
//! (no layer, no `429`s).
//!
//! # v1 shape
//!
//! Keyed per **peer IP** ([`PeerIpKeyExtractor`]) with the `NoOpMiddleware`
//! (no extra rate-limit response headers beyond governor's own `retry-after` /
//! `x-ratelimit-after`). The raw over-limit response is governor's plain `429`
//! with a `retry-after` header and a text body; the B4 wiring wraps that into
//! the shared structured envelope via [`GovernorLayer::error_handler`]
//! ([`wrap_governor_error`]) — `code: "RESOURCE_EXHAUSTED"`, `retriable: true`,
//! matching the legacy `ResourceLimitExceeded` 429 shape (Issue #3561 §8,
//! MUST-FIX 8b) — while preserving the `retry-after` header.
//!
//! The mounted layer is retained (not dropped) so its unbounded per-IP keyed
//! state can be reclaimed: [`RateLimit::into_parts`] splits the mountable layer
//! from a weak [`RateLimitGc`] handle that [`spawn_gc_task`] drives on a coarse
//! interval, self-terminating when the layer is dropped (MUST-FIX 8c).

use crate::security::SecurityConfig;
use axum::Json;
use axum::body::Body;
use axum::http::{Response, StatusCode};
use axum::response::IntoResponse;
use governor::middleware::NoOpMiddleware;
use serde_json::json;
use std::sync::{Arc, Weak};
use std::time::Duration;
use tower_governor::GovernorLayer;
use tower_governor::errors::GovernorError;
use tower_governor::governor::{GovernorConfig, GovernorConfigBuilder};
use tower_governor::key_extractor::PeerIpKeyExtractor;

/// The coarse cadence at which [`spawn_gc_task`] drives [`RateLimitGc::gc`] when
/// a rate-limit layer is mounted. Retention is only ever a memory optimisation
/// (never a correctness requirement), so a slow, cheap cadence is deliberate.
pub const RATE_LIMIT_GC_INTERVAL: Duration = Duration::from_secs(60);

/// The concrete governor layer this core builds: per-peer-IP keying, no extra
/// middleware, producing `axum::body::Body` error responses (the `axum`-feature
/// `From<GovernorError> for Response<axum::body::Body>` conversion).
pub type SpikeGovernorLayer = GovernorLayer<PeerIpKeyExtractor, NoOpMiddleware, axum::body::Body>;

/// The concrete governor config this core builds — the shared, per-peer-IP
/// keyed `RateLimiter` behind both the mounted [`SpikeGovernorLayer`] and the
/// [`RateLimit::gc`] housekeeping handle.
type SpikeGovernorConfig = GovernorConfig<PeerIpKeyExtractor, NoOpMiddleware>;

/// Wrap tower_governor's raw over-limit response into AletheiaDB's shared
/// structured envelope (MUST-FIX 8b).
///
/// Without this, [`GovernorLayer`] serves its own plain-text
/// `"Too Many Requests! Wait for {n}s"` body (via `From<GovernorError>`), which
/// breaks the #3234 error contract every other surface honors. Installed via
/// [`GovernorLayer::error_handler`] — the clean tower_governor 0.8 hook (a
/// `Fn(GovernorError) -> Response<Body>`), so no extra tower layer is needed.
///
/// Only the `TooManyRequests` (429) case is rewritten, into the unified nested
/// `{error:{code, message, retriable, details}}` envelope (Issue #3234) —
/// `code: "RESOURCE_EXHAUSTED"`, `retriable: true` (rate-limit throttling is
/// transient) — preserving governor's
/// `retry-after` header so a client still learns when to retry. Non-throttle
/// governor errors (`UnableToExtractKey` → 500, custom `Other`) fall through to
/// governor's default rendering unchanged.
fn wrap_governor_error(error: GovernorError) -> Response<Body> {
    match error {
        GovernorError::TooManyRequests { wait_time, headers } => {
            // Unified nested error envelope (Issue #3234), byte-shape-identical to
            // the HTTP surface's `AletheiaHttpError::into_response`.
            let body = json!({
                "error": {
                    "code": "RESOURCE_EXHAUSTED",
                    "message": format!("rate limit exceeded; retry after {wait_time}s"),
                    "retriable": true,
                    "details": {
                        "reason": "rate_limit",
                        "retry_after_secs": wait_time,
                    },
                }
            });
            let mut resp = (StatusCode::TOO_MANY_REQUESTS, Json(body)).into_response();
            // Preserve governor's advisory headers (retry-after / x-ratelimit-*)
            // without clobbering the JSON content-type set above.
            if let Some(extra) = headers {
                for (name, value) in extra.iter() {
                    if name != axum::http::header::CONTENT_TYPE {
                        resp.headers_mut().insert(name, value.clone());
                    }
                }
            }
            resp
        }
        // Not a throttle decision — keep governor's own status/body.
        other => other.into(),
    }
}

/// A mounted rate limiter plus the handle needed to garbage-collect its
/// unbounded per-IP keyed state (F2).
///
/// `tower_governor` keys rate-limit state per peer IP in an internal keyed map
/// that **grows without bound**: every distinct source IP ever seen leaves a
/// residual entry, and the raw [`GovernorLayer`] exposes no way to reclaim
/// them. A long-running server keyed on attacker-controlled source IPs would
/// leak memory indefinitely. This struct pairs the [`layer`](Self::layer) with
/// [`gc`](Self::gc), which calls the underlying limiter's `retain_recent()` to
/// evict keys whose state is indistinguishable from fresh (i.e. idle long
/// enough to no longer affect any decision).
///
/// # B4 wiring contract
///
/// The [`layer`](Self::layer) is `move`d onto the router via `.layer(...)`, but
/// the `RateLimit` (holding the shared config `Arc`) must be **retained** by
/// `apply_security` so it can drive GC. B4 must call [`gc`](Self::gc)
/// periodically — e.g. from a guarded background task on a coarse interval
/// (retention is only ever a memory optimisation, never a correctness
/// requirement, so a slow cadence such as once per minute is fine). GC shares
/// the limiter with the live layer (same `Arc`), so eviction is immediately
/// reflected by in-flight requests.
pub struct RateLimit {
    /// The mounted `tower::Layer`. Move this onto the router via `.layer(..)`.
    pub layer: SpikeGovernorLayer,
    /// The shared config behind the layer, retained so [`gc`](Self::gc) can
    /// reach the same keyed limiter the mounted layer uses.
    config: Arc<SpikeGovernorConfig>,
}

impl RateLimit {
    /// Garbage-collect stale per-IP keyed state (the F2 unbounded-growth fix).
    ///
    /// Delegates to the underlying keyed `RateLimiter::retain_recent()`, which
    /// drops every key whose rate-limit state is indistinguishable from a fresh
    /// (never-seen) key — i.e. IPs idle long enough that retaining them changes
    /// no future decision. Safe to call at any time and **idempotent**: calling
    /// it again with no intervening traffic is a no-op. B4 should call this on
    /// a periodic schedule (see the type-level "B4 wiring contract").
    pub fn gc(&self) {
        self.config.limiter().retain_recent();
    }

    /// The number of "live" per-IP keys currently retained in the limiter's
    /// state store. Primarily an observability/testing hook for [`gc`](Self::gc)
    /// (this is the count that shrinks after eviction). May be an estimate under
    /// concurrent traffic, per governor's own `RateLimiter::len` contract.
    #[must_use]
    pub fn live_keys(&self) -> usize {
        self.config.limiter().len()
    }

    /// Split the mountable [`layer`](Self::layer) from a lifecycle-tied GC handle
    /// (MUST-FIX 8c). `apply_security` calls this: it moves the `layer` onto the
    /// router via `.layer(..)` and hands the [`RateLimitGc`] to [`spawn_gc_task`]
    /// so the unbounded per-IP keyed state is periodically reclaimed.
    ///
    /// The returned [`RateLimitGc`] holds only a **weak** reference to the shared
    /// config, so it never keeps the limiter alive on its own — the GC task
    /// self-terminates once the mounted layer (the last strong ref) is dropped.
    #[must_use]
    pub fn into_parts(self) -> (SpikeGovernorLayer, RateLimitGc) {
        let gc = RateLimitGc {
            config: Arc::downgrade(&self.config),
        };
        (self.layer, gc)
    }
}

/// A lifecycle-tied garbage-collection handle for a mounted rate limiter
/// (MUST-FIX 8c).
///
/// Holds a [`Weak`] to the shared keyed config: [`gc`](Self::gc) upgrades it and
/// calls `retain_recent()` when the limiter is still mounted, and reports
/// `false` once the layer has been dropped so [`spawn_gc_task`]'s loop exits
/// without leaking. This is the reference that makes the background GC task
/// tied to the app's lifecycle rather than a detached, never-ending task.
#[derive(Clone)]
pub struct RateLimitGc {
    config: Weak<SpikeGovernorConfig>,
}

impl RateLimitGc {
    /// Reclaim stale per-IP keyed state if the limiter is still live.
    ///
    /// Returns `true` when the underlying limiter was reachable and swept
    /// (`retain_recent()`), `false` once every strong reference (i.e. the
    /// mounted layer) has been dropped — the signal [`spawn_gc_task`] uses to
    /// stop.
    #[must_use]
    pub fn gc(&self) -> bool {
        match self.config.upgrade() {
            Some(config) => {
                config.limiter().retain_recent();
                true
            }
            None => false,
        }
    }

    /// Live per-IP key count if the limiter is still mounted (observability).
    #[must_use]
    pub fn live_keys(&self) -> Option<usize> {
        self.config.upgrade().map(|c| c.limiter().len())
    }
}

/// Spawn the periodic GC task that drives [`RateLimitGc::gc`] (MUST-FIX 8c).
///
/// Started by `apply_security` only when a rate-limit layer is mounted. The task
/// ticks every `interval` and calls `gc()`; because the handle is weak, the very
/// first tick after the mounted layer is dropped observes the upgrade fail and
/// **breaks the loop** — so the task is tied to the app's lifecycle and never
/// leaks or duplicates (one task per mounted layer, self-terminating).
///
/// Returns `None` when called outside a Tokio runtime (no task is spawned);
/// otherwise the [`JoinHandle`](tokio::task::JoinHandle) (primarily for tests —
/// production fire-and-forgets it, relying on the weak-handle self-termination).
#[must_use = "in production the handle is intentionally dropped; tests should await it"]
pub fn spawn_gc_task(
    handle: RateLimitGc,
    interval: Duration,
) -> Option<tokio::task::JoinHandle<()>> {
    if tokio::runtime::Handle::try_current().is_err() {
        return None;
    }
    Some(tokio::spawn(async move {
        let mut ticker = tokio::time::interval(interval);
        // The first tick fires immediately; consume it so the first *sweep*
        // happens one interval in (nothing to reclaim at t=0 anyway).
        ticker.tick().await;
        loop {
            ticker.tick().await;
            if !handle.gc() {
                // The mounted layer was dropped: stop (no leak).
                break;
            }
        }
    }))
}

/// Build the per-IP rate-limit layer from a [`SecurityConfig`], **default-off**.
///
/// Returns `None` (no layer, today's behavior) unless
/// [`SecurityConfig::rate_limit`] is `Some`. When enabled it builds a
/// [`GovernorConfigBuilder`] with the configured sustained rate
/// ([`RateLimitSettings::rps`](crate::security::RateLimitSettings::rps)) and
/// burst allowance
/// ([`RateLimitSettings::burst`](crate::security::RateLimitSettings::burst)),
/// returning `None` if the resulting quota is invalid (e.g. a zero rate).
///
/// The returned [`RateLimit`] carries both the mountable [`RateLimit::layer`]
/// and the [`RateLimit::gc`] handle B4 must drive to bound the per-IP keyed
/// state (F2). Default-off is preserved: this is still `Option`, still `None`
/// unless the operator opts in.
#[must_use]
pub fn governor_layer(cfg: &SecurityConfig) -> Option<RateLimit> {
    let settings = cfg.rate_limit?;
    // Map the sustained rate `rps` (requests/second) onto governor's per-cell
    // *replenishment period* correctly. `GovernorConfigBuilder::per_second(n)`
    // is a misnomer: it sets the period to `n` SECONDS (rate = 1/n req/s), so
    // `per_second(rps)` inverts the intent — `rps=100` would mean one request
    // every 100s. The correct period between replenished cells is `1/rps`
    // seconds = `1_000_000_000 / rps` nanoseconds. `rps == 0` is an invalid
    // (zero) rate → no layer.
    if settings.rps == 0 {
        return None;
    }
    let period = Duration::from_nanos(1_000_000_000 / settings.rps);
    let mut builder = GovernorConfigBuilder::default();
    builder.period(period).burst_size(settings.burst);
    // Retain the config `Arc` alongside the layer: the mounted layer and the
    // `gc()` handle share the SAME keyed limiter, so eviction reaches the live
    // state. `GovernorLayer::new` accepts `impl Into<Arc<GovernorConfig>>`; an
    // `Arc` is passed through by identity.
    let config = Arc::new(builder.finish()?);
    // Wrap the raw governor 429 into the shared structured envelope (8b) via the
    // tower_governor 0.8 `error_handler` hook.
    let layer = GovernorLayer::new(config.clone()).error_handler(wrap_governor_error);
    Some(RateLimit { layer, config })
}
