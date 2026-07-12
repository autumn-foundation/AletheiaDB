//! Test/app constructors that wire the slice onto autumn 0.5.
//!
//! [`build_spike_testapp`] assembles the `GET /nodes/{id}` route, an OpenAPI
//! document at `/openapi.json`, and an MCP-over-HTTP endpoint at `/mcp` (all
//! projected from the one `#[get] + #[api_doc(mcp)]` annotation), installs the
//! shared DB + auth state as extensions, gates the whole `/mcp` endpoint behind
//! autumn's native `RequireApiToken` layer (so the catalog is not anonymously
//! reachable — auth-on-by-default parity), and mounts an arbitrary
//! `tower::Layer` — proving autumn 0.5 accepts general tower layers (the
//! sealed-`IntoAppLayer` limitation of 0.4 that ADR 0055 documents is lifted).

use crate::auth::{AuthStoreTokenAdapter, SpikeAuthState};
use crate::state::SpikeState;
use aletheiadb::AletheiaDB;
use aletheiadb::auth::{AuthMode, AuthStore};
use autumn_web::auth::RequireApiToken;
use autumn_web::openapi::OpenApiConfig;
use autumn_web::prelude::routes;
use autumn_web::test::{TestApp, TestClient};
use axum::http::{HeaderName, HeaderValue};
use std::sync::Arc;
use tower_http::set_header::SetResponseHeaderLayer;

/// Response header the demo tower layer stamps onto every response — evidence
/// that an arbitrary `tower::Layer` mounts through autumn 0.5's `.layer()`.
pub const DEMO_MIDDLEWARE_HEADER: &str = "x-spike-middleware";

/// Build a fully-wired [`TestApp`] for the slice: `GET /nodes/{id}` + `/mcp` +
/// `/openapi.json`, with the shared `db` and `auth` state installed, an
/// arbitrary tower layer mounted, and — in [`AuthMode::Required`] — the `/mcp`
/// endpoint gated by autumn's native [`RequireApiToken`] backed by the shared
/// [`AuthStore`] (via [`AuthStoreTokenAdapter`]). In [`AuthMode::Anonymous`]
/// the `/mcp` gate is intentionally skipped.
///
/// Returned unbuilt so callers can tack on per-test tweaks before `.build()`.
#[must_use]
pub fn build_spike_testapp(db: Arc<AletheiaDB>, store: Arc<AuthStore>, mode: AuthMode) -> TestApp {
    let spike_state = SpikeState::new(db);
    let auth_state = SpikeAuthState::new(store.clone(), mode);

    let mut app = TestApp::new()
        .routes(routes![crate::handler::get_node])
        .openapi(OpenApiConfig::new("AletheiaDB Spike", "0.1.0"))
        .mount_mcp("/mcp")
        // Arbitrary tower middleware via autumn's `.layer()` (ADR 0055: 0.4's
        // sealed `IntoAppLayer` rejected general tower layers; 0.5 accepts them,
        // so per-IP rate limiting — e.g. tower_governor's `GovernorLayer` — is
        // now mountable. A response-only `SetResponseHeaderLayer` stands in as
        // the compile-and-run proof that the bound is lifted).
        .layer(SetResponseHeaderLayer::if_not_present(
            HeaderName::from_static(DEMO_MIDDLEWARE_HEADER),
            HeaderValue::from_static("active"),
        ));

    // Auth-on-by-default parity for the MCP surface: gate the whole `/mcp`
    // endpoint (initialize/tools/list/tools/call) behind the native token layer
    // backed by the shared AuthStore. Anonymous mode opts out deliberately.
    if matches!(mode, AuthMode::Required) {
        let token_layer = RequireApiToken::new(Arc::new(AuthStoreTokenAdapter::new(store)));
        app = app.secure_mcp(token_layer);
    }

    app.state_initializer(move |app_state| {
        app_state.insert_extension(spike_state);
        app_state.insert_extension(auth_state);
    })
}

/// Convenience: [`build_spike_testapp`] then `.build()` into a [`TestClient`].
#[must_use]
pub fn build_spike_client(
    db: Arc<AletheiaDB>,
    store: Arc<AuthStore>,
    mode: AuthMode,
) -> TestClient {
    build_spike_testapp(db, store, mode).build()
}
