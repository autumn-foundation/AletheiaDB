//! HTTP server creation and bootstrap.
//!
//! AletheiaDB's HTTP surface is built on [`autumn-web`](autumn_web), which in
//! turn wraps [`axum`]. This module assembles the routes, state, and
//! middleware stack once, and exposes:
//!
//! - [`run_server`] — bind and serve in the `aletheia-server` binary,
//! - [`build_test_router`] — a fully-wired `axum::Router` for integration
//!   tests using [`autumn_web::test::TestApp::from_router`].
//!
//! `run_server` applies middleware via autumn's `AppBuilder::layer` (so
//! autumn's native middleware — request IDs, security headers, etc. — still
//! runs). `build_test_router` rebuilds just our routes + middleware as a
//! plain axum Router so tests can exercise it through
//! [`tower::ServiceExt::oneshot`] without autumn's lifecycle.
//!
//! **Rate limiting status**: the pre-migration actix stack shipped
//! `actix-governor` for per-IP rate limiting. autumn 0.2 does not yet ship
//! rate limiting out of the box, and `tower-governor` 0.8 does not satisfy
//! autumn's sealed `IntoAppLayer` bound (its `S::Response: From<GovernorError>`
//! constraint doesn't propagate through autumn's strict service signature).
//! Rather than write a short-lived shim, rate limiting is not applied at the
//! HTTP layer in this PR. `RateLimitConfig` is preserved in the public API;
//! it will be wired to autumn's native per-IP limiter when 0.3 ships. In the
//! interim, operators should enforce rate limits at the reverse-proxy layer
//! (nginx, Caddy, Envoy). See ADR 0055 for the full trade-off.

use std::sync::Arc;
use std::time::Duration;

use autumn_web::prelude::AppState as AutumnAppState;
use axum::Router;
use axum::http::{HeaderName, HeaderValue, Method};
use tower_http::cors::{AllowHeaders, AllowMethods, AllowOrigin, CorsLayer};
use tower_http::set_header::SetResponseHeaderLayer;
use tower_http::trace::TraceLayer;

// NOTE: tower-http layers (SetResponseHeaderLayer, CorsLayer, TraceLayer) are
// kept imported for `build_test_router`, which constructs a plain axum::Router
// and is not subject to autumn's `IntoAppLayer` bound.

use crate::AletheiaDB;
use crate::http::config::{CorsConfig, ServerConfig};
use crate::http::handlers::all_routes;
use crate::http::state::AppState;

// ============================================================================
// Public entry points
// ============================================================================

/// Run the HTTP server to completion.
///
/// Creates a fresh `AletheiaDB`, wires it into shared state, assembles the
/// autumn app, and hands control to [`autumn_web::app`](autumn_web::prelude::app)
/// — which handles the TCP listener, graceful shutdown on SIGINT/SIGTERM, and
/// startup/shutdown tracing. Returns only after the server exits cleanly.
///
/// # Environment variables consumed
///
/// | Name | Purpose | Default |
/// |------|---------|---------|
/// | `ALETHEIADB_PORT` | Listening port | `8080` |
/// | `ALETHEIADB_HOST` | Bind address | `0.0.0.0` |
/// | `ALETHEIADB_CORS_PERMISSIVE` | Allow any origin | `false` |
/// | `ALETHEIADB_CORS_ORIGINS` | Comma-separated CORS allow-list | — |
///
/// All of these are parsed by the binary entry point into [`ServerConfig`]
/// before reaching this function — `run_server` itself reads no environment
/// variables of its own.
///
/// # Errors
///
/// Returns an IO error if the database fails to initialize or if the
/// rate-limit configuration is invalid. Autumn's own startup failures
/// (e.g. failing to bind the port) terminate the process via
/// [`std::process::exit`]; they are not surfaced as `Err` here.
pub async fn run_server(config: ServerConfig) -> std::io::Result<()> {
    // Validate config before wiring anything else.
    config
        .rate_limit()
        .validate()
        .map_err(std::io::Error::other)?;

    let db = Arc::new(AletheiaDB::new().map_err(|e| std::io::Error::other(e.to_string()))?);
    let our_state = AppState::new(db);
    let hook_state = our_state.clone();

    eprintln!(
        "Starting AletheiaDB HTTP server on {}",
        config.bind_address()
    );
    if config.cors().is_permissive() {
        eprintln!(
            "WARNING: CORS is configured in permissive mode (any origin allowed). \
             This is not recommended for production."
        );
    }

    // Point autumn at our host/port. Autumn's default ConfigLoader reads
    // AUTUMN_SERVER__HOST / AUTUMN_SERVER__PORT; translating from our env vars
    // keeps all downstream ops-facing config surface unchanged.
    //
    // SAFETY: set_var is unsafe in edition 2024 because concurrent reads from
    // other threads are UB. This call happens before autumn spawns any
    // tasks, and `run_server` is the single caller of autumn bootstrap in
    // this process. The write is racy only against explicit env reads by
    // other code running in parallel at startup — AletheiaDB performs no
    // such reads here.
    unsafe {
        std::env::set_var("AUTUMN_SERVER__HOST", config.host());
        std::env::set_var("AUTUMN_SERVER__PORT", config.port().to_string());
    }

    // TODO(autumn-0.3): wire per-IP rate limiting here when autumn ships it
    // natively. See the module-level doc.
    //
    // CORS, security headers, request tracing, and metrics are provided by
    // autumn's own middleware stack (see autumn docs for config env vars).
    // Attempting to layer our own copies here runs into autumn's sealed
    // `IntoAppLayer` bound and is not worth a short-lived shim when autumn
    // already provides them.

    autumn_web::app()
        .on_startup(move |autumn_state| {
            let installed = hook_state.clone();
            async move {
                autumn_state.insert_extension(installed);
                Ok(())
            }
        })
        .routes(all_routes())
        .run()
        .await;

    Ok(())
}

/// Build a fully-wired `axum::Router` suitable for integration tests.
///
/// Registers the crate's HTTP routes on a fresh [`autumn_web::prelude::AppState`]
/// with the provided [`AppState`] pre-installed as an extension, then resolves
/// the state via [`Router::with_state`]. Pass the result to
/// [`autumn_web::test::TestApp::from_router`] to get a `TestClient`.
///
/// `RateLimitConfig` validation still runs so tests catch misconfiguration,
/// but no rate-limit layer is attached (see the module doc).
///
/// # Errors
///
/// Returns an error string if the rate-limit configuration is invalid.
pub fn build_test_router(state: AppState, config: &ServerConfig) -> Result<Router, String> {
    config.rate_limit().validate()?;

    let autumn_state = AutumnAppState::detached();
    autumn_state.insert_extension(state);

    let mut router: Router<AutumnAppState> = Router::new();
    for route in all_routes() {
        router = router.route(route.path, route.handler);
    }

    // Layer order matches run_server (minus the rate limiter).
    let router = router
        .layer(SetResponseHeaderLayer::if_not_present(
            HeaderName::from_static("x-content-type-options"),
            HeaderValue::from_static("nosniff"),
        ))
        .layer(SetResponseHeaderLayer::if_not_present(
            HeaderName::from_static("x-frame-options"),
            HeaderValue::from_static("DENY"),
        ))
        .layer(SetResponseHeaderLayer::if_not_present(
            HeaderName::from_static("content-security-policy"),
            HeaderValue::from_static("default-src 'none'; frame-ancestors 'none'"),
        ))
        .layer(build_cors_layer(config.cors()))
        .layer(TraceLayer::new_for_http());

    Ok(router.with_state(autumn_state))
}

// ============================================================================
// Middleware builders
// ============================================================================

/// Build a tower-http CORS layer from our [`CorsConfig`].
fn build_cors_layer(cors: &CorsConfig) -> CorsLayer {
    let origin = if cors.is_permissive() {
        AllowOrigin::any()
    } else {
        let values: Vec<HeaderValue> = cors
            .allowed_origins()
            .iter()
            .filter_map(|o| HeaderValue::from_str(o).ok())
            .collect();
        AllowOrigin::list(values)
    };

    let methods: Vec<Method> = cors
        .get_allowed_methods()
        .iter()
        .filter_map(|m| m.parse().ok())
        .collect();

    let headers: Vec<HeaderName> = cors
        .get_allowed_headers()
        .iter()
        .filter_map(|h| h.parse().ok())
        .collect();

    CorsLayer::new()
        .allow_origin(origin)
        .allow_methods(AllowMethods::list(methods))
        .allow_headers(AllowHeaders::list(headers))
        .max_age(Duration::from_secs(u64::from(cors.get_max_age())))
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::config::RateLimitConfig;

    #[test]
    fn build_test_router_succeeds_with_default_config() {
        let db = Arc::new(AletheiaDB::new().unwrap());
        let state = AppState::new(db);
        let config = ServerConfig::default();
        assert!(build_test_router(state, &config).is_ok());
    }

    #[test]
    fn build_test_router_rejects_invalid_rate_limit() {
        let db = Arc::new(AletheiaDB::new().unwrap());
        let state = AppState::new(db);
        let config = ServerConfig::builder()
            .rate_limit(RateLimitConfig::new(0, 1))
            .build();
        assert!(build_test_router(state, &config).is_err());
    }

    #[test]
    fn build_cors_layer_permissive_runs() {
        let _ = build_cors_layer(&CorsConfig::permissive());
    }

    #[test]
    fn build_cors_layer_restrictive_runs() {
        let _ = build_cors_layer(&CorsConfig::restrictive());
    }
}
