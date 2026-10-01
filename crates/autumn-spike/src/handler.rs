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
/// error envelope, off-executor DB access via `spawn_blocking`, and — under the
/// `observability` feature — an OTel/tracing request span mirroring
/// `src/http/trace.rs` (Issue #3376), compiled out to a no-op when the feature
/// is off. Success and error bodies are byte-identical to
/// `POST /query {"operation":"get_node"}`.
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
    // Feature off → zero overhead, identical behavior.
    #[cfg(not(feature = "observability"))]
    {
        get_node_impl(auth, state, id).await
    }
    // Feature on → wrap the handler in an HTTP request root span (method +
    // route + operation) so the DB's own child spans nest underneath it, and
    // record the outcome (result count on success, error code on failure) —
    // exactly the attributes `src/http/trace.rs` records.
    #[cfg(feature = "observability")]
    {
        use tracing::Instrument as _;
        let span = tracing::info_span!(
            "http.request",
            "http.request.method" = "GET",
            "http.route" = "/nodes/{id}",
            operation = "get_node",
            result_count = tracing::field::Empty,
            "otel.status_code" = tracing::field::Empty,
            "error.code" = tracing::field::Empty,
        );
        let outcome = get_node_impl(auth, state, id)
            .instrument(span.clone())
            .await;
        match &outcome {
            Ok(_) => {
                span.record("result_count", 1_i64);
            }
            Err(e) => {
                span.record("otel.status_code", "ERROR");
                span.record("error.code", error_code(e));
            }
        }
        outcome
    }
}

/// The slice's actual logic, kept separate so the public [`get_node`] can wrap
/// it in a feature-gated span without duplicating the body.
async fn get_node_impl(
    auth: SpikeAuth,
    state: SpikeState,
    id: String,
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

/// The Issue #3234 structured code for the variants this slice produces, for
/// span error recording (mirrors `AletheiaHttpError::code_str`, which is
/// `pub(crate)`; a full port would expose it and drop this helper).
#[cfg(feature = "observability")]
fn error_code(e: &AletheiaHttpError) -> &'static str {
    match e {
        AletheiaHttpError::BadRequest(_) => "INVALID_ARGUMENT",
        AletheiaHttpError::NotFound(_) => "NOT_FOUND",
        AletheiaHttpError::Unauthorized => "UNAUTHENTICATED",
        AletheiaHttpError::PermissionDenied { .. } => "PERMISSION_DENIED",
        _ => "INTERNAL",
    }
}
