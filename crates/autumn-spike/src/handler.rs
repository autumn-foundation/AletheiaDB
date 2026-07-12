//! The migrated slice: `GET /nodes/{id}`.
//!
//! A single handler annotated with autumn 0.5's flagship macros —
//! `#[get("/nodes/{id}")]` + `#[api_doc(mcp)]` — which projects one definition
//! to **three** surfaces at once: the HTTP route, an MCP-over-HTTP tool (its
//! `inputSchema` derived from the typed signature), and an OpenAPI path. The
//! body is produced by the *exact same* serializer
//! ([`aletheiadb::http::converters::node_to_query_json`]) the existing
//! `POST /query` `GetNode` operation uses, so the two are byte-identical.

use crate::auth::SpikeAuth;
use crate::error::SpikeError;
use crate::state::SpikeState;
use aletheiadb::auth::AccessClass;
use aletheiadb::core::NodeId;
use aletheiadb::http::ApiResponse;
use aletheiadb::http::converters::node_to_query_json;
// `#[api_doc(...)]` is consumed by the `#[get]` expansion, so it needs no
// separate import here.
use autumn_web::prelude::{Json, Path, get};

/// Fetch a node by id.
///
/// Cross-cutting concerns preserved on the slice: bearer auth (constant-time,
/// on-by-default), RBAC ([`AccessClass::Read`]), the structured flat error
/// envelope, and off-executor DB access via `spawn_blocking`. The result is
/// byte-identical to `POST /query {"operation":"get_node"}`.
#[get("/nodes/{id}")]
#[api_doc(description = "Fetch a node by id", mcp)]
pub async fn get_node(
    auth: SpikeAuth,
    state: SpikeState,
    Path(id): Path<u64>,
) -> Result<Json<ApiResponse>, SpikeError> {
    // Authorization before any work (mirrors the /query dispatch order).
    auth.authorize(AccessClass::Read)?;

    // Reject ids exceeding MAX_VALID_ID as INVALID_ARGUMENT (DoS guard).
    let nid = NodeId::new(id).map_err(|e| SpikeError::InvalidArgument(e.to_string()))?;

    let db = state.db_arc();
    let value = tokio::task::spawn_blocking(move || {
        let node = db
            .get_node(nid)
            .map_err(|_| SpikeError::NotFound(format!("Node {id} not found")))?;
        node_to_query_json(&node).map_err(SpikeError::Internal)
    })
    .await
    .map_err(|e| SpikeError::Internal(e.to_string()))??;

    // Reuse the exact `{success, data}` envelope the `POST /query` path emits,
    // so the response body is byte-identical (Issue #3524 parity).
    Ok(Json(ApiResponse::success(value)))
}
