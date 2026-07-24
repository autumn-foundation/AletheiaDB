//! The faithfully-ported HTTP routes (Issue #3524 PR1).
//!
//! Five of the legacy surface's six routes are ported here as autumn 0.5
//! `#[get]`/`#[post]` + `#[api_doc]` handlers producing **byte-identical**
//! bodies/status codes to `src/http/handlers.rs` + `src/http/admin.rs`, by
//! reusing the exact same public serializers/envelope/error type
//! ([`aletheiadb::http::ApiResponse`], [`aletheiadb::http::AletheiaHttpError`])
//! and the same [`AuthStore`](aletheiadb::auth::AuthStore) methods.
//!
//! The sixth route — the polymorphic `POST /query` (10 operations, per-query
//! resource limits, in-flight DoS guard, provenance filtering) — is a large,
//! stateful port and is **deferred to a follow-up PR** (see the crate docs); it
//! is intentionally NOT mounted in PR1's skeleton.

use crate::security::{AdminClass, ApiKeyStore, Authorized, MetricsClass};
use aletheiadb::auth::{AuthError, Role};
use aletheiadb::http::{AletheiaHttpError, ApiResponse};
use autumn_web::prelude::{Json, get, post};
use serde::{Deserialize, Serialize};
use serde_json::json;

/// Health check response — serializes to `{"status":"healthy"}`, byte-identical
/// to the legacy `health_check` (`src/http/handlers.rs`).
#[derive(Debug, Serialize)]
pub struct HealthResponse {
    status: String,
}

/// `GET /status` — liveness probe. Classified [`MetricsClass`]: every role may
/// probe, but (in required-auth mode) only with a valid credential.
#[get("/status")]
#[api_doc(description = "Liveness probe; returns {\"status\":\"healthy\"}")]
pub async fn health_check(
    _auth: Authorized<MetricsClass>,
) -> Result<Json<HealthResponse>, AletheiaHttpError> {
    Ok(Json(HealthResponse {
        status: "healthy".to_string(),
    }))
}

/// Request body for `POST /admin/keys`.
#[derive(Debug, Deserialize)]
pub struct CreateKeyRequest {
    /// Operator-chosen display name for the new key's principal.
    pub name: String,
    /// Role granted to the new key.
    pub role: Role,
}

/// Request body for `POST /admin/keys/revoke`.
#[derive(Debug, Deserialize)]
pub struct RevokeKeyRequest {
    /// Principal id (as returned by create/list) to revoke.
    pub id: String,
}

/// `POST /admin/keys` — create a key; returns the plaintext exactly once.
#[post("/admin/keys")]
#[api_doc(description = "Create an API key (admin); returns the plaintext key once")]
pub async fn create_key(
    _auth: Authorized<AdminClass>,
    store: ApiKeyStore,
    Json(req): Json<CreateKeyRequest>,
) -> Result<Json<ApiResponse>, AletheiaHttpError> {
    let (principal, key) = store
        .0
        .create_key(&req.name, req.role)
        .map_err(|e| match e {
            // Caller-fault input (empty/overlong name) is a 400, not a 500.
            AuthError::InvalidInput(msg) => AletheiaHttpError::BadRequest(msg),
            other => AletheiaHttpError::Internal(other.to_string()),
        })?;

    Ok(Json(ApiResponse::success(json!({
        "id": principal.id,
        "name": principal.name,
        "role": principal.role,
        "key_prefix": principal.key_prefix,
        "created_at": principal.created_at,
        // Plaintext, exactly once.
        "key": &*key,
    }))))
}

/// `GET /admin/keys` — masked principal list (never key material).
#[get("/admin/keys")]
#[api_doc(description = "List API-key principals (admin); masked, never key material")]
pub async fn list_keys(
    _auth: Authorized<AdminClass>,
    store: ApiKeyStore,
) -> Result<Json<ApiResponse>, AletheiaHttpError> {
    let keys = store.0.list();
    let items: Vec<serde_json::Value> = keys
        .iter()
        .map(|p| {
            json!({
                "id": p.id,
                "name": p.name,
                "role": p.role,
                "key_prefix": p.key_prefix,
                "created_at": p.created_at,
            })
        })
        .collect();

    Ok(Json(ApiResponse::success(json!({ "keys": items }))))
}

/// `POST /admin/keys/revoke` — revoke a key by principal id (immediate effect).
#[post("/admin/keys/revoke")]
#[api_doc(description = "Revoke an API key by principal id (admin); takes effect immediately")]
pub async fn revoke_key(
    _auth: Authorized<AdminClass>,
    store: ApiKeyStore,
    Json(req): Json<RevokeKeyRequest>,
) -> Result<Json<ApiResponse>, AletheiaHttpError> {
    let existed = store
        .0
        .revoke(&req.id)
        .map_err(|e| AletheiaHttpError::Internal(e.to_string()))?;
    if !existed {
        return Err(AletheiaHttpError::NotFound(format!(
            "no key with id '{}'",
            req.id
        )));
    }

    Ok(Json(ApiResponse::success(
        json!({ "revoked": true, "id": req.id }),
    )))
}

/// `POST /admin/promote` — promote this node from replica to primary (Issue
/// #3355). Byte-identical to the legacy `promote` (`src/http/admin.rs`):
/// admin-class, deliberately EXEMPT from the replica read-only guard (it is
/// the one mutating admin op a replica must accept — rejecting it would make
/// promotion over this surface impossible), and idempotent on a node that is
/// already primary (`applier_stopped: false`).
#[post("/admin/promote")]
#[api_doc(description = "Promote this replica to primary (admin); idempotent on a primary")]
pub async fn promote(
    _auth: Authorized<AdminClass>,
    state: crate::state::ServerState,
) -> Result<Json<ApiResponse>, AletheiaHttpError> {
    let report = state
        .db_arc()
        .promote_to_primary()
        .map_err(|e| AletheiaHttpError::Internal(e.to_string()))?;

    Ok(Json(ApiResponse::success(json!({
        "role": "primary",
        "applier_stopped": report.applier_stopped,
        "last_applied_lsn": report.last_applied_lsn,
    }))))
}
