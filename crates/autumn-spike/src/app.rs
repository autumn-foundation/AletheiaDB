//! Test/app constructors that wire the slice onto autumn 0.5.
//!
//! [`build_spike_testapp`] assembles the `GET /nodes/{id}` route, an OpenAPI
//! document at `/openapi.json`, and an MCP-over-HTTP endpoint at `/mcp` (all
//! projected from the one `#[get] + #[api_doc(mcp)]` annotation), installs the
//! shared DB + auth state as extensions, and mounts an arbitrary tower-http
//! middleware layer — proving autumn 0.5 accepts general tower layers (the
//! sealed-`IntoAppLayer` limitation of 0.4 that ADR 0055 documents is lifted).

use crate::auth::SpikeAuthState;
use crate::state::SpikeState;
use aletheiadb::AletheiaDB;
use aletheiadb::auth::{AuthMode, AuthStore};
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
/// `/openapi.json`, with the shared `db` and `auth` state installed and a demo
/// tower-http layer mounted.
///
/// Returned unbuilt so callers can tack on per-test tweaks (e.g. `.secure_mcp`)
/// before `.build()`.
#[must_use]
pub fn build_spike_testapp(db: Arc<AletheiaDB>, store: Arc<AuthStore>, mode: AuthMode) -> TestApp {
    let spike_state = SpikeState::new(db);
    let auth_state = SpikeAuthState::new(store, mode);

    TestApp::new()
        .routes(routes![crate::handler::get_node])
        .openapi(OpenApiConfig::new("AletheiaDB Spike", "0.1.0"))
        .mount_mcp("/mcp")
        // Arbitrary tower middleware via autumn's `.layer()` (ADR 0055: 0.4's
        // sealed `IntoAppLayer` rejected general tower layers; 0.5 accepts them,
        // so per-IP rate limiting — e.g. tower_governor's `GovernorLayer` — is
        // now mountable. A `SetResponseHeaderLayer` stands in as the proof).
        .layer(SetResponseHeaderLayer::if_not_present(
            HeaderName::from_static(DEMO_MIDDLEWARE_HEADER),
            HeaderValue::from_static("active"),
        ))
        .state_initializer(move |app_state| {
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
