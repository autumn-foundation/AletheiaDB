//! The migrated slice: `GET /nodes/{id}`.
//!
//! A single handler annotated with autumn 0.5's flagship macros —
//! `#[get("/nodes/{id}")]` + `#[api_doc(mcp)]` — which projects one definition
//! to **three** surfaces at once: the HTTP route, an MCP-over-HTTP tool (its
//! `inputSchema` derived from the typed signature), and an OpenAPI path. The
//! body is produced by the *exact same* serializer
//! ([`aletheiadb::http::node_to_query_json`]) and envelope
//! ([`aletheiadb::http::ApiResponse`]) the existing `POST /query` `GetNode`
//! operation uses, so success bodies are byte-identical — and errors reuse
//! [`aletheiadb::http::AletheiaHttpError`] verbatim, so error bodies are
//! byte-identical too.

use crate::auth::SpikeAuth;
use crate::state::SpikeState;
use aletheiadb::auth::AccessClass;
use aletheiadb::core::NodeId;
use aletheiadb::http::{AletheiaHttpError, ApiResponse, node_to_query_json};
// `#[api_doc(...)]` is consumed by the `#[get]` expansion, so it needs no
// separate import here.
use autumn_web::prelude::{Json, Path, get};

/// Fetch a node by id.
///
/// Cross-cutting concerns preserved on the slice: bearer auth (constant-time,
/// on-by-default), RBAC ([`AccessClass::Read`]), the existing structured flat
/// error envelope, and off-executor DB access via `spawn_blocking`. Success and
/// error bodies are byte-identical to `POST /query {"operation":"get_node"}`.
///
/// The id is taken as `Path<String>` (not `Path<u64>`) so a non-numeric id
/// (`/nodes/abc`) renders the [`AletheiaHttpError::BadRequest`] envelope rather
/// than axum's default plain-text 400.
#[get("/nodes/{id}")]
#[api_doc(description = "Fetch a node by id", mcp)]
pub async fn get_node(
    auth: SpikeAuth,
    state: SpikeState,
    Path(id): Path<String>,
) -> Result<Json<ApiResponse>, AletheiaHttpError> {
    // Authorization before any work (mirrors the /query dispatch order).
    auth.authorize(AccessClass::Read)?;

    // Non-numeric id → INVALID_ARGUMENT (structured envelope, not axum's default
    // plain-text 400).
    let raw: u64 = id
        .parse()
        .map_err(|_| AletheiaHttpError::BadRequest(format!("invalid node id: {id:?}")))?;
    // Reject ids exceeding MAX_VALID_ID as INVALID_ARGUMENT (DoS guard) — same
    // mapping the existing `handle_get_node` uses.
    let nid = NodeId::new(raw).map_err(|e| AletheiaHttpError::BadRequest(e.to_string()))?;

    let db = state.db_arc();
    let value = tokio::task::spawn_blocking(move || {
        let node = db
            .get_node(nid)
            .map_err(|_| AletheiaHttpError::NotFound(format!("Node {raw} not found")))?;
        node_to_query_json(&node).map_err(AletheiaHttpError::Internal)
    })
    .await
    .map_err(|e| AletheiaHttpError::Internal(e.to_string()))??;

    // Reuse the exact `{success, data}` envelope the `POST /query` path emits,
    // so the response body is byte-identical (Issue #3524 parity).
    Ok(Json(ApiResponse::success(value)))
}
