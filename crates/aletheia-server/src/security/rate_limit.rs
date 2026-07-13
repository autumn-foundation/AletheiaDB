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
//! with a `retry-after` header and a text body; B4 wiring wraps that into the
//! #3234 `{code: "UNAVAILABLE", retriable: true, ...}` envelope (Issue #3561 §8).

use crate::security::SecurityConfig;
use governor::middleware::NoOpMiddleware;
use std::sync::Arc;
use std::time::Duration;
use tower_governor::GovernorLayer;
use tower_governor::governor::{GovernorConfig, GovernorConfigBuilder};
use tower_governor::key_extractor::PeerIpKeyExtractor;

/// The concrete governor layer this core builds: per-peer-IP keying, no extra
/// middleware, producing `axum::body::Body` error responses (the `axum`-feature
/// `From<GovernorError> for Response<axum::body::Body>` conversion).
pub type SpikeGovernorLayer = GovernorLayer<PeerIpKeyExtractor, NoOpMiddleware, axum::body::Body>;

/// The concrete governor config this core builds — the shared, per-peer-IP
/// keyed `RateLimiter` behind both the mounted [`SpikeGovernorLayer`] and the
/// [`RateLimit::gc`] housekeeping handle.
type SpikeGovernorConfig = GovernorConfig<PeerIpKeyExtractor, NoOpMiddleware>;

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
    let layer = GovernorLayer::new(config.clone());
    Some(RateLimit { layer, config })
}
