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
//! - [`resource_limits`], [`rate_limit`], [`cursor`] carry the Lane B security
//!   **primitives** — per-query timeout/row/byte caps + bounded in-flight guard
//!   (#3542 / #3550), the default-off `tower-governor` rate-limit layer (#3561
//!   §8), and signed opaque cursor tokens (#3360). They are the building blocks
//!   the B4 wiring PR mounts via [`apply_security`]; PR1 behavior is unchanged
//!   until then (rate limiting off, generous caps, no cursor validation wired).

pub mod auth;
pub mod cursor;
pub mod rate_limit;
pub mod rbac;
pub mod resource_limits;

use aletheiadb::auth::{AuthMode, AuthStore};
use autumn_web::auth::RequireApiToken;
use autumn_web::prelude::AppState as AutumnAppState;
use autumn_web::test::TestApp;
use std::sync::Arc;
use std::time::Duration;

pub use auth::{
    AccessClassMarker, AdminClass, ApiKeyStore, AuthStoreTokenAdapter, Authorized, MetricsClass,
    RateLimitSettings, ReadClass, ServerAuthState, WriteClass, authorize, authorize_class,
    extract_credential,
};

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
}

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
        }
    }
}

/// Apply the security layer stack to the app builder.
///
/// **Seam (Lane A calls, Lane B extends).** PR1 mirrors the spike: in
/// [`AuthMode::Required`] the whole `/mcp` endpoint is gated behind autumn's
/// native [`RequireApiToken`] backed by the shared [`AuthStore`] (via
/// [`AuthStoreTokenAdapter`]), so the MCP catalog is not anonymously reachable.
/// [`AuthMode::Anonymous`] skips the gate deliberately.
///
/// TODO(Lane B): add the `tower-governor` rate-limit layer and the in-flight
/// `ConcurrencyLimit` backpressure layer here (both no-ops in PR1).
#[must_use]
pub fn apply_security(app: TestApp, cfg: &SecurityConfig) -> TestApp {
    match cfg.mode {
        AuthMode::Required => {
            let token_layer =
                RequireApiToken::new(Arc::new(AuthStoreTokenAdapter::new(cfg.store.clone())));
            app.secure_mcp(token_layer)
        }
        AuthMode::Anonymous => app,
    }
}

/// Insert security-related app state (the auth extension).
///
/// **Seam (Lane A calls, Lane B extends).** Mirrors the spike's
/// `insert_extension(auth_state)`. Called from the app's `state_initializer`,
/// which hands out a shared `&AppState` whose `insert_extension` takes `&self`.
pub fn init_state(app_state: &AutumnAppState, cfg: &SecurityConfig) {
    app_state.insert_extension(ServerAuthState::new(cfg.store.clone(), cfg.mode));
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
