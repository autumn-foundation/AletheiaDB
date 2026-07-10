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
use axum::extract::DefaultBodyLimit;
use axum::http::{HeaderName, HeaderValue, Method};
use tower_http::cors::{AllowHeaders, AllowMethods, AllowOrigin, CorsLayer};
use tower_http::set_header::SetResponseHeaderLayer;
use tower_http::trace::TraceLayer;

// NOTE: tower-http layers (SetResponseHeaderLayer, CorsLayer, TraceLayer) are
// kept imported for `build_test_router`, which constructs a plain axum::Router
// and is not subject to autumn's `IntoAppLayer` bound.

use crate::AletheiaDB;
use crate::auth::{AuthMode, AuthStore, Role};
use crate::http::auth::{AuthState, validate_auth_startup};
use crate::http::config::{CorsConfig, ServerConfig};
use crate::http::handlers::all_routes;
use crate::http::state::AppState;

// ============================================================================
// Public entry points
// ============================================================================

/// Run the HTTP server to completion.
///
/// Creates a fresh `AletheiaDB`, wires it into shared state, assembles the
/// autumn app, and hands control to `autumn_web::app`
/// — which handles the TCP listener, graceful shutdown on SIGINT/SIGTERM, and
/// startup/shutdown tracing. Returns only after the server exits cleanly.
///
/// # Environment variables consumed
///
/// | Name | Purpose | Default |
/// |------|---------|---------|
/// | `ALETHEIADB_PORT` | Listening port | `1963` (in honor of the first Doctor Who broadcast, 23 Nov 1963) |
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
    // Install OpenTelemetry tracing from the standard OTEL_* environment
    // (Issue #3376). Disabled unless ALETHEIADB_OTEL is truthy or an OTLP
    // endpoint is configured; the guard flushes + shuts the exporter down when
    // `run_server` returns. A subscriber-already-installed error is non-fatal
    // (the host process may own the subscriber).
    #[cfg(feature = "otel")]
    let _otel_guard =
        match crate::observability::otel::init(&crate::observability::otel::OtelConfig::from_env())
        {
            Ok(guard) => guard,
            Err(e) => {
                eprintln!("WARNING: OpenTelemetry tracing not installed: {e}");
                None
            }
        };

    // Validate config before wiring anything else.
    config
        .rate_limit()
        .validate()
        .map_err(std::io::Error::other)?;

    // Assemble the auth store: persisted keys (if a path is configured or
    // derivable from data_dir), plus an optional env/config bootstrap admin
    // key (memory-only, never persisted).
    let auth_state = build_auth_state(&config).map_err(std::io::Error::other)?;
    match auth_state.mode() {
        AuthMode::Anonymous => {
            // PROMINENT warning: the operator explicitly opted out of auth.
            eprintln!(
                "WARNING: AUTHENTICATION IS DISABLED (auth mode: anonymous). \
                 Every request has full, unauthenticated access to the database. \
                 Do not expose this server to untrusted networks."
            );
            tracing::warn!(
                "authentication disabled (auth mode: anonymous); every request has full access"
            );
        }
        AuthMode::Required => {
            // Conservative default: refuse to start with zero credentials.
            validate_auth_startup(AuthMode::Required, auth_state.store())
                .map_err(std::io::Error::other)?;
        }
    }

    let db = Arc::new(build_database(&config)?);
    let our_state = AppState::new(db);
    let startup_state = our_state.clone();
    let startup_auth = auth_state.clone();
    let shutdown_state = our_state.clone();
    let persist_on_shutdown = config.data_dir().is_some();

    eprintln!(
        "Starting AletheiaDB HTTP server on {}",
        config.bind_address()
    );
    match config.data_dir() {
        Some(path) => eprintln!("Data directory: {}", path.display()),
        None => eprintln!(
            "WARNING: no data directory configured — running in-memory; state is lost on shutdown."
        ),
    }
    if config.cors().is_permissive() {
        eprintln!(
            "WARNING: CORS is configured in permissive mode (any origin allowed). \
             This is not recommended for production."
        );
    }

    // Bridge our ServerConfig into autumn's config via its AUTUMN_*__* env
    // vars (host/port/CORS/CSRF; the actuator hardening below goes through
    // a ConfigLoader because `[actuator]` has no AUTUMN_* env mapping).
    //
    // TODO: fold this env-var bridge into `HardenedConfigLoader` — it's
    // cleaner and retires the `unsafe` block.
    //
    // SAFETY: `set_var` is unsafe in edition 2024 because concurrent reads
    // from other threads are UB. These calls happen before any autumn code
    // runs (autumn's own startup reads env only inside `.run()`), and
    // AletheiaDB performs no env reads on other threads during this window.
    unsafe {
        apply_autumn_env(&config);
    }

    // Framework endpoints autumn mounts outside our AuthContext extractor
    // (they never see AletheiaDB credentials). With the sensitive actuator
    // group forced off by `HardenedConfigLoader`, what remains is
    // health/metadata only — but operators should still block them at the
    // reverse proxy if they must not be publicly reachable.
    eprintln!(
        "Note: framework endpoints served WITHOUT AletheiaDB authentication: \
         /health, /live, /ready, /startup, /actuator/health, /actuator/info, \
         /actuator/metrics, /actuator/a11y, /actuator/ui, /actuator/ui/metrics. \
         Sensitive actuator endpoints (/actuator/env, /actuator/configprops, \
         /actuator/loggers, /actuator/tasks, /actuator/jobs, /actuator/prometheus) \
         are disabled in every profile. Block /actuator and the probe paths at \
         your reverse proxy if they must not be publicly reachable."
    );

    // TODO(autumn-0.3): wire per-IP rate limiting here when autumn ships it
    // natively. See the module-level doc.
    //
    // Request tracing, metrics, and security headers are provided by autumn's
    // baseline middleware. CORS is driven by the AUTUMN_CORS__* env vars set
    // in `apply_autumn_env` above, so operator-facing `ALETHEIADB_CORS_*`
    // settings flow end-to-end into autumn's CorsLayer.

    autumn_web::app()
        .with_config_loader(HardenedConfigLoader)
        .on_startup(move |autumn_state| {
            let installed = startup_state.clone();
            let installed_auth = startup_auth.clone();
            async move {
                autumn_state.insert_extension(installed);
                autumn_state.insert_extension(installed_auth);
                Ok(())
            }
        })
        // Graceful-shutdown checkpoint. Without this, SIGTERM leaves the
        // string interner and index state behind at the last
        // mutation-threshold snapshot (defaults: 500 new strings / 5-10
        // minute cadence). A Docker-style stop/start workflow would lose
        // label strings until a checkpoint happened organically, which
        // manifests as `label: "<unknown:N>"` after restart. Flushing on
        // shutdown makes restart semantics feel like postgres: stop, start,
        // your data is exactly as you left it.
        .on_shutdown(move || {
            let db = shutdown_state.db_arc();
            let should_persist = persist_on_shutdown;
            async move {
                if !should_persist {
                    return;
                }
                match tokio::task::spawn_blocking(move || db.persist_indexes()).await {
                    Ok(Ok(())) => eprintln!("Shutdown: indexes persisted."),
                    Ok(Err(e)) => eprintln!("Shutdown: persist_indexes failed: {e}"),
                    Err(e) => eprintln!("Shutdown: persist_indexes task panicked: {e}"),
                }
            }
        })
        .routes(all_routes())
        // Reject oversized request bodies with 413 before they are buffered /
        // deserialized (payload-amplification DoS prevention — Issue #3108).
        // `DefaultBodyLimit` is axum-native and keeps the request type as
        // `axum::extract::Request`, so it satisfies autumn's `IntoAppLayer`
        // bound (unlike `RequestBodyLimitLayer`, which rewrites the body type).
        .layer(DefaultBodyLimit::max(config.max_request_body_bytes()))
        .run()
        .await;

    Ok(())
}

/// Autumn [`ConfigLoader`](autumn_web::config::ConfigLoader) that runs the
/// default five-layer load (framework defaults → profile smart defaults →
/// `autumn.toml` → `autumn-{profile}.toml` → `AUTUMN_*` env vars) and then
/// forces security-relevant framework settings to safe values.
///
/// Autumn mounts its own routes (probes, actuator) *outside* AletheiaDB's
/// `AuthContext` extractor, so they are never authenticated by our API-key
/// layer. Its `dev` profile smart default sets `actuator.sensitive = true`,
/// which additionally exposes `/actuator/env`, `/actuator/configprops`, an
/// unauthenticated `PUT /actuator/loggers/{name}`, `/actuator/tasks`,
/// `/actuator/jobs`, and `/actuator/prometheus` — and `[actuator]` has **no**
/// `AUTUMN_*` env-var mapping, so `AUTUMN_ACTUATOR__SENSITIVE=false` cannot
/// turn it off. This loader pins `actuator.sensitive = false` in every
/// profile: the config-dump and log-mutation endpoints are never served,
/// debug build or not. The remaining always-mounted framework endpoints
/// (health probes, `/actuator/health|info|metrics|a11y|ui`) are documented in
/// `docs/guides/security-quickstart.md` with reverse-proxy guidance.
struct HardenedConfigLoader;

impl autumn_web::config::ConfigLoader for HardenedConfigLoader {
    async fn load(
        &self,
    ) -> Result<autumn_web::config::AutumnConfig, autumn_web::config::ConfigError> {
        let mut config = autumn_web::config::TomlEnvConfigLoader::new()
            .load()
            .await?;
        // Never serve the sensitive actuator group, regardless of profile
        // or operator TOML: these endpoints bypass AletheiaDB auth entirely.
        config.actuator.sensitive = false;
        Ok(config)
    }
}

/// Assemble the [`AuthState`] a [`ServerConfig`] implies.
///
/// - Loads the persisted key store from
///   [`ServerConfig::resolved_auth_persist_path`] when one is configured or
///   derivable, otherwise starts a memory-only store.
/// - Installs the bootstrap admin key (principal name `bootstrap-admin`,
///   role `admin`) when configured. Bootstrap keys are memory-only: they are
///   re-supplied via env/config on every start and never persisted.
///
/// Startup *refusal* (required mode with zero credentials) is enforced by
/// [`run_server`], not here — `build_test_router` deliberately skips it so
/// tests can exercise the uniform-401 behavior of a keyless required-mode
/// server.
fn build_auth_state(config: &ServerConfig) -> Result<AuthState, String> {
    let store = match config.resolved_auth_persist_path() {
        Some(path) => AuthStore::with_persistence(&path)
            .map_err(|e| format!("failed to open auth key store: {e}"))?,
        None => AuthStore::new(),
    };
    if let Some(key) = config.bootstrap_admin_key() {
        store
            .insert_bootstrap_key("bootstrap-admin", Role::Admin, key.expose())
            .map_err(|e| format!("failed to install bootstrap admin key: {e}"))?;
    }
    Ok(AuthState::new(Arc::new(store), config.auth_mode()))
}

/// Construct the [`AletheiaDB`] instance the HTTP server will share.
///
/// Resolution order:
/// 1. If `ALETHEIADB_CONFIG` or `ALETHEIADB_DATA_DIR` is set in the environment,
///    delegate to [`AletheiaDB::open_from_env`] so the server picks up the same
///    config the MCP server, CLI, and Python SDK do.
/// 2. Otherwise use [`ServerConfig::to_unified_config`] — this is the path
///    integration tests take when they pass `data_dir` programmatically without
///    touching the environment.
fn build_database(config: &ServerConfig) -> std::io::Result<AletheiaDB> {
    if crate::config::config_path_from_env().is_some()
        || crate::config::data_dir_from_env().is_some()
    {
        return AletheiaDB::open_from_env().map_err(|e| std::io::Error::other(e.to_string()));
    }
    match config.to_unified_config() {
        None => AletheiaDB::new().map_err(|e| std::io::Error::other(e.to_string())),
        Some(unified) => AletheiaDB::with_unified_config(unified)
            .map_err(|e| std::io::Error::other(e.to_string())),
    }
}

/// Set the `AUTUMN_SERVER__*` and `AUTUMN_CORS__*` environment variables
/// autumn's config loader reads, based on our [`ServerConfig`].
///
/// # Safety
///
/// Calls [`std::env::set_var`], which is `unsafe` under Rust edition 2024
/// because concurrent reads from other threads result in UB. Callers must
/// ensure no other thread is reading environment variables concurrently.
/// [`run_server`] calls this exactly once, before handing control to autumn.
unsafe fn apply_autumn_env(config: &ServerConfig) {
    // SAFETY: see function-level docs; ordering is the caller's responsibility.
    unsafe {
        std::env::set_var("AUTUMN_SERVER__HOST", config.host());
        std::env::set_var("AUTUMN_SERVER__PORT", config.port().to_string());

        let cors = config.cors();
        // `permissive` maps to wildcard origin. `restrictive` with zero explicit
        // origins would disable CORS entirely, which is fine.
        let origins: Vec<&str> = if cors.is_permissive() {
            vec!["*"]
        } else {
            cors.allowed_origins().iter().map(String::as_str).collect()
        };
        std::env::set_var("AUTUMN_CORS__ALLOWED_ORIGINS", origins.join(","));
        std::env::set_var(
            "AUTUMN_CORS__ALLOWED_METHODS",
            cors.get_allowed_methods().join(","),
        );
        std::env::set_var(
            "AUTUMN_CORS__ALLOWED_HEADERS",
            cors.get_allowed_headers().join(","),
        );
        std::env::set_var("AUTUMN_CORS__MAX_AGE_SECS", cors.get_max_age().to_string());

        // Disable CSRF protection. autumn's `prod` profile enables it by
        // default (appropriate for session-authenticated browser forms),
        // but AletheiaDB's `/query` endpoint is a token-less JSON API —
        // no sessions, no forms, nothing a cross-site POST could
        // impersonate. CSRF protection adds no security here and only
        // breaks legitimate POSTs. When the dashboard PR lands with
        // session-backed routes, it should re-enable CSRF scoped to those
        // routes rather than flipping this flag globally.
        std::env::set_var("AUTUMN_SECURITY__CSRF__ENABLED", "false");
    }
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
/// Auth state is derived from `config` (mode, bootstrap key, persist path),
/// exactly as `run_server` derives it — but *without* `run_server`'s
/// "required mode needs at least one credential" startup refusal, so tests
/// can exercise the uniform 401 of a keyless required-mode server. Use
/// [`build_test_router_with_auth`] to inject a pre-populated
/// [`AuthState`]/[`AuthStore`] instead.
///
/// # Errors
///
/// Returns an error string if the rate-limit or auth configuration is invalid.
pub fn build_test_router(state: AppState, config: &ServerConfig) -> Result<Router, String> {
    let auth_state = build_auth_state(config)?;
    build_test_router_with_auth(state, auth_state, config)
}

/// [`build_test_router`] with an explicit [`AuthState`] — for tests that
/// create/revoke keys against a shared [`AuthStore`] handle.
///
/// # Errors
///
/// Returns an error string if the rate-limit configuration is invalid.
pub fn build_test_router_with_auth(
    state: AppState,
    auth_state: AuthState,
    config: &ServerConfig,
) -> Result<Router, String> {
    config.rate_limit().validate()?;

    let autumn_state = AutumnAppState::detached();
    autumn_state.insert_extension(state);
    autumn_state.insert_extension(auth_state);

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
        .layer(TraceLayer::new_for_http())
        // Reject oversized request bodies with 413 before they are buffered /
        // deserialized, bounding the memory a single request can force us to
        // allocate (payload-amplification DoS prevention — Issue #3108).
        .layer(DefaultBodyLimit::max(config.max_request_body_bytes()));

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
