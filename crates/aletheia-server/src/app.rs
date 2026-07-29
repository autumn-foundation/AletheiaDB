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

use crate::changefeed_stream;
use crate::constraints_lineage_audit_tools;
use crate::crypto_shred_tools;
use crate::deferred_batch_tools;
use crate::edge_tools;
use crate::http_routes;
use crate::metrics_exposition;
use crate::namespace_tools;
use crate::node_tools;
use crate::schema_batch_tools;
use crate::security::{self, SecurityConfig};
use crate::state::ServerState;
use crate::traverse_temporal_tools;
use crate::vector_query_tools;
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
    try_build_server_testapp(db, SecurityConfig::new(store, mode))
        .expect("security startup validation")
}

/// Fallible assembly — the proving-ground stand-in for the production
/// `on_startup` refusal (ruling: startup wiring).
///
/// `validate_startup(&cfg)` runs **before** any routing is assembled; a refused
/// config (required mode with zero credentials) returns `Err(message)` rather
/// than assembling a server every request would 401. In the production
/// `AppBuilder`, this same `validate_startup` is wired into `.on_startup(..)`,
/// whose `Err` aborts startup with the propagated message; `TestApp` has no
/// `on_startup` hook, so the proving ground surfaces the identical refusal here
/// (and `build_server_testapp` turns it into a fail-fast panic).
///
/// # Errors
///
/// Returns the human-readable refusal message from
/// [`security::validate_startup`] when the config is refused.
pub fn try_build_server_testapp(
    db: Arc<AletheiaDB>,
    cfg: SecurityConfig,
) -> Result<TestApp, String> {
    security::validate_startup(&cfg)?;

    let server_state = ServerState::new(db);
    let init_cfg = cfg.clone();

    let app = TestApp::new()
        .routes(all_routes())
        .openapi(OpenApiConfig::new("AletheiaDB", env!("CARGO_PKG_VERSION")))
        .mount_mcp("/mcp");

    // Lane-B security seam: custom `/mcp` gate + default-off rate limiter.
    let app = security::apply_security(app, &cfg);

    Ok(app.state_initializer(move |app_state| {
        app_state.insert_extension(server_state);
        security::init_state(app_state, &init_cfg);
    }))
}

/// Every route this server serves, in one place.
///
/// The single source of truth for **both** the [`TestApp`] proving ground and
/// the production daemon ([`crate::daemon::run_server`], Issue #2905): a route
/// that exists on one and not the other would mean the parity suite is testing
/// a surface nobody runs.
#[must_use]
pub fn all_routes() -> Vec<autumn_web::Route> {
    routes![
        http_routes::health_check,
        metrics_exposition::metrics,
        http_routes::create_key,
        http_routes::list_keys,
        http_routes::revoke_key,
        http_routes::promote,
        node_tools::get_node,
        node_tools::list_nodes,
        node_tools::count_nodes,
        node_tools::create_node,
        node_tools::update_node,
        node_tools::delete_node,
        node_tools::delete_node_cascade,
        node_tools::retract_node,
        node_tools::create_node_with_embedding,
        node_tools::update_node_embedding,
        node_tools::find_nodes_at_time,
        edge_tools::get_edge,
        edge_tools::list_edges,
        edge_tools::count_edges,
        edge_tools::get_outgoing_edges,
        edge_tools::get_incoming_edges,
        edge_tools::create_edge,
        edge_tools::update_edge,
        edge_tools::delete_edge,
        edge_tools::retract_edge,
        traverse_temporal_tools::traverse,
        traverse_temporal_tools::get_node_history,
        traverse_temporal_tools::get_edge_history,
        traverse_temporal_tools::get_node_at_time,
        traverse_temporal_tools::get_edge_at_time,
        traverse_temporal_tools::list_changes,
        changefeed_stream::await_changes,
        changefeed_stream::changes_stream,
        traverse_temporal_tools::get_node_at_valid_time,
        traverse_temporal_tools::get_node_at_transaction_time,
        traverse_temporal_tools::diff_node_versions,
        traverse_temporal_tools::get_edge_at_valid_time,
        traverse_temporal_tools::get_edge_at_transaction_time,
        traverse_temporal_tools::diff_edge_versions,
        traverse_temporal_tools::get_belief_revisions,
        deferred_batch_tools::create_drift_monitor,
        deferred_batch_tools::list_drift_monitors,
        deferred_batch_tools::delete_drift_monitor,
        deferred_batch_tools::query_drift_alarms,
        deferred_batch_tools::resolve_drift_alarm,
        deferred_batch_tools::contradiction_genealogy,
        deferred_batch_tools::find_contradictions,
        deferred_batch_tools::counterfactual_replay,
        deferred_batch_tools::trust_breakdown,
        deferred_batch_tools::list_trust_policies,
        vector_query_tools::find_similar,
        vector_query_tools::hybrid_query,
        vector_query_tools::query,
        vector_query_tools::list_vector_indexes,
        vector_query_tools::embed_query,
        vector_query_tools::embed_text,
        vector_query_tools::semantic_search,
        vector_query_tools::semantic_path,
        vector_query_tools::concept_analogy,
        vector_query_tools::concept_mean,
        vector_query_tools::find_duplicate_candidates,
        vector_query_tools::semantic_horizon,
        vector_query_tools::context_aspects,
        schema_batch_tools::get_schema,
        schema_batch_tools::temporal_extent,
        schema_batch_tools::database_stats,
        schema_batch_tools::apply_batch,
        constraints_lineage_audit_tools::enable_vector_index,
        constraints_lineage_audit_tools::enable_unique_constraint,
        constraints_lineage_audit_tools::list_unique_constraints,
        constraints_lineage_audit_tools::lineage_upstream,
        constraints_lineage_audit_tools::lineage_downstream,
        constraints_lineage_audit_tools::audit_export,
        constraints_lineage_audit_tools::verify_chain,
        constraints_lineage_audit_tools::export_chain_head,
        namespace_tools::create_namespace,
        namespace_tools::list_namespaces,
        namespace_tools::describe_namespace,
        crypto_shred_tools::designate_subject,
        crypto_shred_tools::erase_subject,
    ]
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

/// Assemble a [`TestClient`] from a fully-specified [`SecurityConfig`] (custom
/// resource caps, rate-limit opt-in, cursor budgets). **Panics** on a refused
/// startup config (see [`try_build_server_testapp`]).
#[must_use]
pub fn build_server_client_with_config(db: Arc<AletheiaDB>, cfg: SecurityConfig) -> TestClient {
    try_build_server_testapp(db, cfg)
        .expect("security startup validation")
        .build()
}

/// Fallible [`build_server_client_with_config`] — returns the startup-refusal
/// message instead of panicking.
///
/// # Errors
///
/// Returns the refusal message from [`security::validate_startup`].
pub fn try_build_server_client(
    db: Arc<AletheiaDB>,
    cfg: SecurityConfig,
) -> Result<TestClient, String> {
    Ok(try_build_server_testapp(db, cfg)?.build())
}
