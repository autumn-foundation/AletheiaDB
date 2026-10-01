//! Inbound-connection concurrency budgets (Issue #3561 §8, AC1 / AC5).
//!
//! AletheiaDB is **embedded** — a single process holds one `Arc`, so there is
//! deliberately **no outbound DB connection pool** (§8 explicit non-goal). The
//! only "pool" concern is **inbound** HTTP / MCP-over-HTTP client concurrency,
//! and this module carries the two `tower::limit` concurrency budgets that bound
//! it with **backpressure** (queue, not reject — distinct from the load-shedding
//! [`InFlightLimiter`](crate::security::resource_limits::InFlightLimiter), which
//! returns a `503` at capacity):
//!
//! 1. [`app_concurrency_layer`] — an app-wide
//!    [`GlobalConcurrencyLimitLayer`] bounding the number of HTTP requests being
//!    processed concurrently across the whole surface (AC1). Sourced from
//!    [`SecurityConfig::max_concurrent_requests`].
//! 2. [`mcp_session_layer`] — the same primitive scoped to the `/mcp` endpoint
//!    (composed with the [`McpSecurityLayer`](crate::security::mcp_gate::McpSecurityLayer)
//!    via [`secure_mcp`](autumn_web::app::AppBuilder::secure_mcp)), bounding the
//!    number of concurrent MCP-over-HTTP sessions — where multi-agent load
//!    actually lands (AC5). Sourced from [`SecurityConfig::max_mcp_sessions`].
//!
//! # Global, not per-service
//!
//! Both use [`GlobalConcurrencyLimitLayer`] rather than
//! [`ConcurrencyLimitLayer`](tower::limit::ConcurrencyLimitLayer): axum clones
//! the wrapped service per connection, and the *global* variant holds the
//! permit semaphore in the layer itself (an `Arc<Semaphore>`), so every cloned
//! service shares one budget. A per-service `ConcurrencyLimitLayer` would give
//! each connection its own independent limit — not a surface-wide cap.
//!
//! # Default-off (parity)
//!
//! Both constructors return `None` unless the operator opts in (the cap is
//! `Some(n)` with `n > 0`), mirroring the default-off `tower-governor` rate
//! limiter ([`governor_layer`](crate::security::rate_limit::governor_layer)): by
//! default no layer is mounted and behavior is byte-for-byte today's (no
//! backpressure). A configured `Some(0)` is treated as "no budget" (`None`) —
//! a zero-permit semaphore would wedge every request, never a useful config.

use crate::security::SecurityConfig;
use tower::limit::GlobalConcurrencyLimitLayer;

/// Build a **global** (surface-wide) concurrency-limit layer from a permit cap,
/// **default-off**.
///
/// Returns `None` for `None`/`Some(0)` (no layer, today's behavior); otherwise a
/// [`GlobalConcurrencyLimitLayer`] whose shared semaphore admits at most `cap`
/// concurrently-processed requests and back-pressures (parks in `poll_ready`)
/// the rest until a permit frees.
#[must_use]
fn concurrency_layer(cap: Option<usize>) -> Option<GlobalConcurrencyLimitLayer> {
    match cap {
        Some(n) if n > 0 => Some(GlobalConcurrencyLimitLayer::new(n)),
        _ => None,
    }
}

/// The app-wide HTTP concurrency budget (AC1), from
/// [`SecurityConfig::max_concurrent_requests`]. `None` (default) mounts no layer.
#[must_use]
pub fn app_concurrency_layer(cfg: &SecurityConfig) -> Option<GlobalConcurrencyLimitLayer> {
    concurrency_layer(cfg.max_concurrent_requests)
}

/// The `/mcp`-scoped MCP-over-HTTP session budget (AC5), from
/// [`SecurityConfig::max_mcp_sessions`]. `None` (default) mounts no layer, so the
/// `/mcp` endpoint carries only its security gate, unchanged.
#[must_use]
pub fn mcp_session_layer(cfg: &SecurityConfig) -> Option<GlobalConcurrencyLimitLayer> {
    concurrency_layer(cfg.max_mcp_sessions)
}
