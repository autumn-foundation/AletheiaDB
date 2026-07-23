//! Admin key-lifecycle endpoints (Issue #3350) plus manual promotion (Issue
//! #3355, Slice C). All classified [`AccessClass::Admin`].
//!
//! - `POST /admin/keys`        — create a key; returns the plaintext exactly once.
//! - `GET  /admin/keys`        — masked list (id, name, role, prefix; never key material).
//! - `POST /admin/keys/revoke` — revoke by principal id (effective immediately).
//! - `POST /admin/promote`     — promote this node from replica to primary.
//!
//! Revocation uses `POST` + a JSON body rather than a `DELETE` path parameter
//! so the route shape stays uniform with the rest of the JSON API and avoids
//! leaning on autumn's attribute-macro path-parameter support.

use crate::auth::{AccessClass, AuthError, Role};
use crate::http::auth::AuthContext;
use crate::http::error::AletheiaHttpError;
use crate::http::handlers::ApiResponse;
use crate::http::state::AppState;
use autumn_web::Route;
use autumn_web::prelude::{get, post, routes};
use axum::Json;
use serde::Deserialize;
use serde_json::json;

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

/// Create a new API key. The plaintext key is returned exactly once, here;
/// it is never stored and cannot be retrieved again.
#[post("/admin/keys")]
pub async fn create_key(
    auth: AuthContext,
    state: AppState,
    Json(req): Json<CreateKeyRequest>,
) -> Result<Json<ApiResponse>, AletheiaHttpError> {
    auth.authorize(AccessClass::Admin)?;
    // Read-only replica enforcement (Issue #3355, Slice A): key lifecycle
    // mutates the server's local auth store, so it is a write-class admin
    // operation and follows the same replica refusal as graph writes.
    if state.db().is_replica() {
        return Err(AletheiaHttpError::read_only_replica());
    }

    let (principal, key) = auth
        .store()
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

/// List all principals. Masked by construction: [`crate::auth::Principal`]
/// never holds key material, only the display prefix.
#[get("/admin/keys")]
pub async fn list_keys(auth: AuthContext) -> Result<Json<ApiResponse>, AletheiaHttpError> {
    auth.authorize(AccessClass::Admin)?;

    let keys = auth.store().list();
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

/// Revoke a key by principal id. Takes effect for subsequent requests
/// immediately. Revoking your own key is allowed (the current request
/// already authenticated).
#[post("/admin/keys/revoke")]
pub async fn revoke_key(
    auth: AuthContext,
    state: AppState,
    Json(req): Json<RevokeKeyRequest>,
) -> Result<Json<ApiResponse>, AletheiaHttpError> {
    auth.authorize(AccessClass::Admin)?;
    // Read-only replica enforcement (Issue #3355, Slice A): see `create_key`.
    if state.db().is_replica() {
        return Err(AletheiaHttpError::read_only_replica());
    }

    let existed = auth
        .store()
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

/// Promote this node from a read-only replica to a writable primary (Issue
/// #3355, Slice C operator surface).
///
/// Deliberately does **NOT** perform the `state.db().is_replica()` write-class
/// refusal that [`create_key`]/[`revoke_key`] above apply: promotion is the
/// one mutating admin operation a replica MUST accept -- refusing it because
/// the node is a replica would make it impossible to ever promote one over
/// this surface. Every other write path keeps rejecting on a replica; this is
/// the single, deliberate, documented exception.
///
/// Calls [`crate::db::AletheiaDB::promote_to_primary`] and reports its
/// [`crate::db::PromotionReport`]. Idempotent: promoting an already-primary
/// node succeeds as a no-op (`applier_stopped: false`).
#[post("/admin/promote")]
pub async fn promote(
    auth: AuthContext,
    state: AppState,
) -> Result<Json<ApiResponse>, AletheiaHttpError> {
    auth.authorize(AccessClass::Admin)?;

    let report = state
        .db()
        .promote_to_primary()
        .map_err(|e| AletheiaHttpError::Internal(e.to_string()))?;

    Ok(Json(ApiResponse::success(json!({
        "role": "primary",
        "applier_stopped": report.applier_stopped,
        "last_applied_lsn": report.last_applied_lsn,
    }))))
}

/// The admin route set, appended to [`crate::http::handlers::all_routes`].
///
/// Kept in this module so the `routes![...]` macro resolves the companion
/// items generated by `#[get]` / `#[post]` in the scope where the handlers
/// are declared.
#[must_use]
pub fn admin_routes() -> Vec<Route> {
    routes![create_key, list_keys, revoke_key, promote]
}
