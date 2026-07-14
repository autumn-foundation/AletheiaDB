//! App assembly for the production autumn-web 0.5 server (Issue #3524 PR1).
//!
//! [`build_server_testapp`] wires the ported HTTP routes + the node-read proof
//! slice into one autumn app that projects them to HTTP, OpenAPI
//! (`/openapi.json`), and MCP-over-HTTP (`/mcp`) from single handler
//! definitions, installs the shared DB + auth state, and applies the Lane-B
//! security seam ([`apply_security`], [`init_state`], [`validate_startup`]).
//!
//! It builds a [`TestApp`] (the 0.5 type the parity harness drives via
//! [`TestClient`]); the production `AppBuilder` shares the same method surface
//! (`routes`/`openapi`/`mount_mcp`/`secure_mcp`/`state_initializer`), so Lane B
//! can lift this into a `run_server` with minimal change.

use crate::http_routes;
use crate::node_tools;
use crate::security::{self, SecurityConfig};
use crate::state::ServerState;
use aletheiadb::AletheiaDB;
use aletheiadb::auth::{AuthMode, AuthStore};
use autumn_web::openapi::OpenApiConfig;
use autumn_web::prelude::routes;
use autumn_web::test::{TestApp, TestClient};
use std::sync::Arc;

/// Build the unwired-but-assembled [`TestApp`]: all ported routes + the three
/// node tools, `/openapi.json`, `/mcp`, the security seam applied, and the
/// shared `db`/auth state installed. Returned unbuilt so callers can tweak.
///
/// Startup is validated ([`security::validate_startup`]) before assembly;
/// **panics** on a refused config (required mode with zero credentials), which
/// is the intended fail-fast behavior for a misconfigured server. Tests that
/// need a keyless required-mode client should insert a bootstrap key first.
#[must_use]
pub fn build_server_testapp(db: Arc<AletheiaDB>, store: Arc<AuthStore>, mode: AuthMode) -> TestApp {
    let cfg = SecurityConfig::new(store, mode);
    security::validate_startup(&cfg).expect("security startup validation");

    let server_state = ServerState::new(db);
    let init_cfg = cfg.clone();

    let app = TestApp::new()
        .routes(routes![
            http_routes::health_check,
            http_routes::create_key,
            http_routes::list_keys,
            http_routes::revoke_key,
            node_tools::get_node,
            node_tools::list_nodes,
            node_tools::count_nodes,
            node_tools::create_node,
            node_tools::update_node,
            node_tools::delete_node,
            node_tools::delete_node_cascade,
            node_tools::retract_node,
            node_tools::find_nodes_at_time,
        ])
        .openapi(OpenApiConfig::new("AletheiaDB", env!("CARGO_PKG_VERSION")))
        .mount_mcp("/mcp");

    // Lane-B security seam: `/mcp` gate (Required mode), etc.
    let app = security::apply_security(app, &cfg);

    app.state_initializer(move |app_state| {
        app_state.insert_extension(server_state);
        security::init_state(app_state, &init_cfg);
    })
}

/// Convenience: [`build_server_testapp`] then `.build()` into a [`TestClient`].
#[must_use]
pub fn build_server_client(
    db: Arc<AletheiaDB>,
    store: Arc<AuthStore>,
    mode: AuthMode,
) -> TestClient {
    build_server_testapp(db, store, mode).build()
}
