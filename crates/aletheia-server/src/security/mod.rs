// OWNED BY LANE B (security). Scaffold only — do not implement here.
//
//! Security seam for the production autumn-web 0.5 server (Issue #3524).
//!
//! This module tree is the **security boundary** Lane A stands up as a set of
//! compiling, minimally-behaving scaffolds and Lane B fills in. Lane A's
//! [`crate::app`] calls exactly three seam functions from here —
//! [`apply_security`], [`init_state`], and [`validate_startup`] — so Lane B can
//! flesh out authentication, RBAC-class enforcement, rate limiting, resource
//! limits, and cursor signing behind those signatures without touching the app
//! assembly again.
//!
//! Current PR1 behavior (deliberately minimal):
//! - [`apply_security`] gates `/mcp` with the native `RequireApiToken` layer in
//!   [`AuthMode::Required`] (auth-on-by-default parity), pass-through otherwise.
//! - [`init_state`] installs the shared [`ServerAuthState`] extension.
//! - [`validate_startup`] refuses required-mode startup with zero credentials.
//! - [`auth::Authorized`] **authenticates and enforces the RBAC class** via
//!   [`authorize`] (Lane B): a role that does not permit the handler's
//!   declared `C::CLASS` gets a byte-identical 403.
//! - [`resource_limits`], [`rate_limit`], [`cursor`], [`concurrency`] carry the
//!   Lane B security **primitives** — per-query timeout/row/byte caps + bounded
//!   in-flight guard (#3542 / #3550), the default-off `tower-governor`
//!   rate-limit layer (#3561 §8), signed opaque cursor tokens (#3360), and the
//!   default-off inbound HTTP + MCP-over-HTTP concurrency budgets with
//!   backpressure (#3561 §8 AC1/AC5). They are the building blocks
//!   [`apply_security`] mounts.

pub mod auth;
pub mod concurrency;
pub mod cursor;
pub mod mcp_gate;
pub mod rate_limit;
pub mod rbac;
pub mod resource_limits;

use aletheiadb::auth::{AuthMode, AuthStore};
use autumn_web::config::AutumnConfig;
use autumn_web::prelude::AppState as AutumnAppState;
use autumn_web::test::TestApp;
use std::sync::Arc;
use std::time::Duration;
use tower::ServiceBuilder;

pub use auth::{
    AccessClassMarker, AdminClass, ApiKeyStore, AuthStoreTokenAdapter, Authorized, MetricsClass,
    RateLimitSettings, ReadClass, ServerAuthState, WriteClass, authorize, authorize_class,
    extract_credential, extract_credential_headers,
};
pub use cursor::{CursorSecret, LiveCursorRegistry};
pub use mcp_gate::{
    McpSecurityLayer, mcp_payload_too_large_response, mcp_permission_denied_response,
    mcp_unauthenticated_response, mcp_unroutable_tool_call_response,
};
pub use resource_limits::{InFlightLimiter, ResourceLimits};

/// Security configuration for the server surface.
///
/// **Lane-B-owned.** PR1 carries only the auth essentials (the shared
/// credential store and the auth mode) — enough for the three seam functions to
/// compile and for the node-read proof slice + parity to run. Lane B extends
/// this with rate-limit, in-flight/resource, and cursor-signing configuration.
#[derive(Clone)]
pub struct SecurityConfig {
    /// The shared, constant-time-verified API-key store.
    pub store: Arc<AuthStore>,
    /// Whether authentication is required or explicitly anonymous.
    pub mode: AuthMode,
    /// Per-IP rate-limit settings (Lane B3). `None` = **off** (default, parity
    /// with today). `Some` builds the default-off `tower-governor` layer via
    /// [`rate_limit::governor_layer`]. The B4 wiring PR mounts the layer.
    pub rate_limit: Option<RateLimitSettings>,
    /// Per-query wall-clock timeout (Lane B2, #3542). Generous default (30 s)
    /// so no existing behavior changes; `Duration::ZERO` = unlimited.
    pub query_timeout: Duration,
    /// Cap on rows/entities returned by a single query (Lane B2, #3542). `0` =
    /// unlimited. Generous default (10_000) mirroring the HTTP `/query`
    /// surface's `DEFAULT_MAX_RESULT_ROWS`.
    pub max_result_rows: usize,
    /// Cap on serialized response bytes of a single query (Lane B2, #3542).
    /// `0` = unlimited. Generous default (8 MiB) mirroring
    /// `DEFAULT_MAX_RESPONSE_BYTES`.
    pub max_response_bytes: usize,
    /// Max concurrent in-flight timed queries (Lane B2 DoS guard, #3550). `0` =
    /// unbounded. The cap the [`resource_limits`] in-flight limiter enforces.
    pub max_in_flight_queries: usize,
    /// Cursor time-to-live (Lane B4, #3360 default 300 s). Wired at B4.
    pub cursor_ttl: Duration,
    /// Max concurrently-live cursors per connection (Lane B4, #3360 default
    /// 128). Wired at B4.
    pub max_live_cursors_per_conn: usize,
    /// App-wide HTTP concurrency budget (Issue #3561 §8 AC1). `None` = **off**
    /// (default, parity with today); `Some(n)` mounts a global
    /// [`tower::limit::GlobalConcurrencyLimitLayer`] admitting at most `n`
    /// concurrently-processed requests and back-pressuring the rest. `Some(0)`
    /// is treated as off (see [`concurrency::app_concurrency_layer`]).
    pub max_concurrent_requests: Option<usize>,
    /// Concurrency budget for MCP-over-HTTP sessions on `/mcp` (Issue #3561 §8
    /// AC5). `None` = **off** (default, parity); `Some(n)` composes a global
    /// concurrency limit over the `/mcp` security gate so at most `n` MCP
    /// sessions are processed concurrently, back-pressuring the rest.
    pub max_mcp_sessions: Option<usize>,
    /// Max accepted request body size, in bytes (Issue #3561 §8 AC3 / #3108).
    /// Applied app-wide via [`axum::extract::DefaultBodyLimit`]; an over-limit
    /// body is rejected `413` before it is buffered. Default 2 MiB, matching the
    /// legacy HTTP surface's `DEFAULT_MAX_REQUEST_BODY_BYTES` (and axum's own
    /// implicit default), so making it explicit changes no client behavior.
    pub max_request_body_bytes: usize,
}

/// Default app-wide request-body cap (2 MiB), matching the legacy HTTP surface's
/// `DEFAULT_MAX_REQUEST_BODY_BYTES` (Issue #3108) and axum's implicit default.
pub const DEFAULT_MAX_REQUEST_BODY_BYTES: usize = 2 * 1024 * 1024;

impl SecurityConfig {
    /// Build a config from a shared store and mode, with the not-yet-wired
    /// security primitives at their conservative defaults (rate limiting off,
    /// 30 s query timeout, 10_000-row / 8-MiB caps, in-flight cap 64, cursor
    /// TTL 300 s, cursor cap 128).
    #[must_use]
    pub fn new(store: Arc<AuthStore>, mode: AuthMode) -> Self {
        Self {
            store,
            mode,
            rate_limit: None,
            query_timeout: Duration::from_millis(30_000),
            max_result_rows: 10_000,
            max_response_bytes: 8 * 1024 * 1024,
            max_in_flight_queries: 64,
            cursor_ttl: Duration::from_secs(300),
            max_live_cursors_per_conn: 128,
            max_concurrent_requests: None,
            max_mcp_sessions: None,
            max_request_body_bytes: DEFAULT_MAX_REQUEST_BODY_BYTES,
        }
    }

    /// Enable the default-off per-IP rate-limit layer (builder-style, for
    /// wiring/tests). `None` restores the default (off).
    #[must_use]
    pub fn with_rate_limit(mut self, settings: Option<RateLimitSettings>) -> Self {
        self.rate_limit = settings;
        self
    }

    /// Override the per-query wall-clock timeout (`Duration::ZERO` = unlimited).
    #[must_use]
    pub fn with_query_timeout(mut self, timeout: Duration) -> Self {
        self.query_timeout = timeout;
        self
    }

    /// Override the per-query row cap (`0` = unlimited).
    #[must_use]
    pub fn with_max_result_rows(mut self, rows: usize) -> Self {
        self.max_result_rows = rows;
        self
    }

    /// Override the per-query serialized-byte cap (`0` = unlimited).
    #[must_use]
    pub fn with_max_response_bytes(mut self, bytes: usize) -> Self {
        self.max_response_bytes = bytes;
        self
    }

    /// Override the concurrent in-flight-query cap (`0` = unbounded).
    #[must_use]
    pub fn with_max_in_flight_queries(mut self, cap: usize) -> Self {
        self.max_in_flight_queries = cap;
        self
    }

    /// Set the app-wide HTTP concurrency budget (AC1). `None`/`Some(0)` = off.
    #[must_use]
    pub fn with_max_concurrent_requests(mut self, cap: Option<usize>) -> Self {
        self.max_concurrent_requests = cap;
        self
    }

    /// Set the MCP-over-HTTP session concurrency budget (AC5). `None`/`Some(0)`
    /// = off.
    #[must_use]
    pub fn with_max_mcp_sessions(mut self, cap: Option<usize>) -> Self {
        self.max_mcp_sessions = cap;
        self
    }

    /// Override the app-wide request-body cap in bytes (AC3, `DefaultBodyLimit`).
    #[must_use]
    pub fn with_max_request_body_bytes(mut self, bytes: usize) -> Self {
        self.max_request_body_bytes = bytes;
        self
    }
}

/// Shared server-side security **primitives** installed into app state by
/// [`init_state`] and pulled back by the resource-limited read handlers and
/// (future) cursor-paged handlers.
///
/// Distinct from [`ServerAuthState`] (which carries the credential store +
/// mode): this carries the per-query resource caps ([`ResourceLimits`]), the
/// process-wide bounded in-flight guard ([`InFlightLimiter`], #3550), the
/// per-process cursor-signing secret ([`CursorSecret`], #3360) and the
/// per-connection cursor cap registry ([`LiveCursorRegistry`]). Cloneable
/// (every field is `Arc`-backed or `Copy`) so it lives in the extension bag.
#[derive(Clone)]
pub struct ServerSecurityState {
    limits: ResourceLimits,
    in_flight: InFlightLimiter,
    cursor_secret: Arc<CursorSecret>,
    cursor_registry: LiveCursorRegistry,
}

impl ServerSecurityState {
    /// Build the primitives from a [`SecurityConfig`]: a fresh in-flight guard
    /// and cursor registry at the configured caps, and a fresh random
    /// per-process cursor secret.
    #[must_use]
    pub fn from_config(cfg: &SecurityConfig) -> Self {
        Self {
            limits: ResourceLimits::from_config(cfg),
            in_flight: InFlightLimiter::from_config(cfg),
            cursor_secret: Arc::new(CursorSecret::random()),
            cursor_registry: LiveCursorRegistry::new(cfg.max_live_cursors_per_conn),
        }
    }

    /// The effective per-query resource caps.
    #[must_use]
    pub fn limits(&self) -> ResourceLimits {
        self.limits
    }

    /// The bounded in-flight-query admission guard.
    #[must_use]
    pub fn in_flight(&self) -> &InFlightLimiter {
        &self.in_flight
    }

    /// The per-process cursor-signing secret.
    #[must_use]
    pub fn cursor_secret(&self) -> &Arc<CursorSecret> {
        &self.cursor_secret
    }

    /// The per-connection live-cursor cap registry.
    #[must_use]
    pub fn cursor_registry(&self) -> &LiveCursorRegistry {
        &self.cursor_registry
    }
}

impl axum::extract::FromRequestParts<AutumnAppState> for ServerSecurityState {
    type Rejection = aletheiadb::http::AletheiaHttpError;

    async fn from_request_parts(
        _parts: &mut axum::http::request::Parts,
        state: &AutumnAppState,
    ) -> Result<Self, Self::Rejection> {
        state
            .extension::<ServerSecurityState>()
            .map(|arc| (*arc).clone())
            .ok_or(aletheiadb::http::AletheiaHttpError::StateMissing)
    }
}

/// Apply the security layer stack to the app builder (B4 wiring).
///
/// **Seam (Lane A calls, Lane B fills).** Two mounts:
///
/// 1. **`/mcp` gate (F2/F4).** The whole `/mcp` endpoint — `initialize`,
///    `tools/list`, `tools/call` — is gated behind the custom
///    [`McpSecurityLayer`] via
///    [`secure_mcp`](autumn_web::app::AppBuilder::secure_mcp) (whose bound
///    accepts any `tower::Layer<Route>`, not just `RequireApiToken`). Unlike the
///    native bearer-only `RequireApiToken`, this layer accepts Bearer **or**
///    `x-api-key`, emits the shared `{code, message, retriable, details?}`
///    envelope on failure (uniform 401), and RBAC-pre-checks each `tools/call`
///    at the envelope (403 with `details`). It is mounted in **both** modes: in
///    [`AuthMode::Anonymous`] it inserts the anonymous principal and passes
///    through (catalog reachable), matching the legacy anonymous surface.
/// 2. **Rate-limit layer (F, default-off).** [`rate_limit::governor_layer`]
///    returns `None` unless [`SecurityConfig::rate_limit`] is `Some`, so by
///    default no layer is mounted and behavior is byte-for-byte today's. When
///    the operator opts in, the per-IP `tower-governor` layer is mounted
///    app-wide via `.layer()`.
///
/// The per-query resource caps, in-flight guard, and cursor primitives are
/// **state**, not layers — [`init_state`] installs them for the handlers to
/// enforce per-request.
#[must_use]
pub fn apply_security(app: TestApp, cfg: &SecurityConfig) -> TestApp {
    // 1. Custom /mcp gate (both modes; branches on mode internally), optionally
    //    fronted by the MCP-over-HTTP session concurrency budget (AC5). When a
    //    budget is set, a global concurrency limit is composed OUTSIDE the gate
    //    (permit acquired before any auth work), so at most `max_mcp_sessions`
    //    MCP requests are processed concurrently, back-pressuring the rest.
    let mcp_auth = McpSecurityLayer::new(cfg.store.clone(), cfg.mode);
    let app = match concurrency::mcp_session_layer(cfg) {
        Some(budget) => app.secure_mcp(ServiceBuilder::new().layer(budget).layer(mcp_auth)),
        None => app.secure_mcp(mcp_auth),
    };

    // 2. App-wide request-body cap (AC3 / #3108): reject an over-limit body with
    //    `413` before it is buffered/deserialized. autumn already mounts a global
    //    `DefaultBodyLimit` from `security.upload.max_request_size_bytes` (an
    //    outer `.layer(DefaultBodyLimit)` here would be overridden by that inner
    //    one), so the effective, non-overridden knob is that config value — set
    //    it from `max_request_body_bytes` so the operator-configurable cap
    //    actually takes effect on the assembled surface. (autumn merges `/mcp`
    //    after this middleware, so `/mcp` keeps its own built-in 2 MiB limit —
    //    the gate buffers to that bound independently.)
    let mut autumn_cfg = AutumnConfig::default();
    autumn_cfg.security.upload.max_request_size_bytes = cfg.max_request_body_bytes;
    let app = app.config(autumn_cfg);

    // 3. App-wide HTTP concurrency budget (AC1), default-off. When enabled, a
    //    global concurrency limit back-pressures requests over the cap surface-
    //    wide (queue, not reject — the load-shedding 503 path is the separate
    //    per-query `InFlightLimiter`).
    let app = match concurrency::app_concurrency_layer(cfg) {
        Some(layer) => app.layer(layer),
        None => app,
    };

    // 4. Default-off per-IP rate limiter, mounted app-wide only when enabled.
    //    When enabled, retain the `RateLimit`'s GC handle and drive it from a
    //    lifecycle-tied background task so the per-IP keyed state cannot grow
    //    without bound (MUST-FIX 8c). The task holds only a weak handle, so it
    //    self-terminates when the mounted layer is dropped (no leak/duplicate).
    match rate_limit::governor_layer(cfg) {
        Some(rl) => {
            let (layer, gc) = rl.into_parts();
            let _gc_task = rate_limit::spawn_gc_task(gc, rate_limit::RATE_LIMIT_GC_INTERVAL);
            app.layer(layer)
        }
        None => app,
    }
}

/// Insert security-related app state (the auth extension).
///
/// **Seam (Lane A calls, Lane B extends).** Mirrors the spike's
/// `insert_extension(auth_state)`. Called from the app's `state_initializer`,
/// which hands out a shared `&AppState` whose `insert_extension` takes `&self`.
pub fn init_state(app_state: &AutumnAppState, cfg: &SecurityConfig) {
    app_state.insert_extension(ServerAuthState::new(cfg.store.clone(), cfg.mode));
    // B4: install the resource-limit + cursor primitives so the read handlers
    // (row/byte/timeout/in-flight enforcement) and cursor-paged handlers can
    // pull them per-request.
    app_state.insert_extension(ServerSecurityState::from_config(cfg));
}

/// Validate security configuration at startup.
///
/// **Seam (Lane A calls, Lane B extends).** Refuse to start in
/// [`AuthMode::Required`] with zero credentials — every request would be
/// rejected — mirroring `validate_mcp_auth_startup` / `validate_auth_startup`.
///
/// # Errors
///
/// Returns a human-readable message when `mode` is `Required` and the store is
/// empty.
pub fn validate_startup(cfg: &SecurityConfig) -> Result<(), String> {
    match cfg.mode {
        AuthMode::Anonymous => Ok(()),
        AuthMode::Required if cfg.store.is_empty() => Err(
            "authentication is required (the default) but no credentials are available: \
             provide a bootstrap key or explicitly opt into anonymous mode"
                .to_string(),
        ),
        AuthMode::Required => Ok(()),
    }
}
