// OWNED BY LANE B (security).
//
//! The custom, `x-api-key`-aware `/mcp` security gate (Issue #3524 B4, ruling F2/F4).
//!
//! # Why not the native `RequireApiToken`
//!
//! autumn 0.5's [`RequireApiToken`](autumn_web::auth::RequireApiToken) gates
//! `/mcp` behind a bearer-**only** token check
//! (`parse_bearer_token`) and, on failure, emits autumn's own
//! `application/problem+json` body — **not** AletheiaDB's
//! `{code, message, retriable, details?}` envelope. That breaks two B4
//! requirements at once: (1) AletheiaDB accepts the credential in either
//! `Authorization: Bearer` **or** `x-api-key` (mirroring the legacy HTTP/MCP
//! surfaces), and (2) every auth/authz failure must carry the shared structured
//! envelope for byte-parity (AC f).
//!
//! [`secure_mcp`](autumn_web::app::AppBuilder::secure_mcp) is **not** hard-wired
//! to `RequireApiToken`: its bound is any
//! `L: tower::Layer<axum::routing::Route>` whose service maps
//! `Request<Body> → Response<Body>` with `Error = Infallible`. So we mount this
//! [`McpSecurityLayer`] instead — a custom tower layer that:
//!
//! 1. **Authenticates** via [`extract_credential_headers`] (Bearer **or**
//!    `x-api-key`, trimmed, empty-rejected) and the shared constant-time
//!    [`AuthStore::verify`]. Any failure — missing / malformed / unknown /
//!    revoked — collapses to one **uniform** `401 UNAUTHENTICATED` envelope
//!    (identical to the legacy surface; never reveals which failure occurred).
//!    This gates the **entire** `/mcp` endpoint — `initialize`, `tools/list`,
//!    and `tools/call` are all unreachable without a valid credential in
//!    [`AuthMode::Required`] (AC e).
//! 2. On success **inserts the verified [`Principal`] into request extensions**
//!    (the F3 caching seam) and passes through.
//! 3. **Pre-checks per-tool RBAC** for `tools/call` at the envelope (ruling F4):
//!    the request body is buffered, the JSON-RPC `method`/`params.name` parsed,
//!    and [`rbac::authorize_tool`] consulted. A role that does not permit the
//!    tool's class is refused **before dispatch** with a `403 PERMISSION_DENIED`
//!    envelope carrying `details: {required_class, principal_role}` — the same
//!    shape the legacy MCP `permission_denied_error` emits. (The replayed
//!    dispatch handler's own `Authorized<C>` gate remains as defense-in-depth;
//!    see the crate/README notes on the tools/call replay boundary.)
//!
//! In [`AuthMode::Anonymous`] the layer inserts [`Principal::anonymous`] and
//! passes through unconditionally (catalog reachable — matching the legacy
//! anonymous surface).

use std::convert::Infallible;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use aletheiadb::auth::{AuthMode, AuthStore, Principal};
use aletheiadb::http::AletheiaHttpError;
use axum::Json;
use axum::body::Body;
use axum::http::{Request, Response, StatusCode};
use axum::response::IntoResponse;
use serde_json::json;

use crate::security::auth::extract_credential_headers;
use crate::security::rbac::{self, Denied};

/// Upper bound on a buffered `/mcp` JSON-RPC request body (MCP requests are
/// tiny control messages). A body larger than this is not a legitimate
/// JSON-RPC call, so it is not parsed for a tool name — authentication still
/// applies, and dispatch enforces its own limits.
const MAX_MCP_REQUEST_BYTES: usize = 2 * 1024 * 1024;

/// The uniform `401 UNAUTHENTICATED` envelope for the `/mcp` gate — reuses the
/// shared [`AletheiaHttpError::Unauthorized`] rendering so the body (and the
/// `www-authenticate: Bearer` header) is byte-identical to the HTTP surface.
#[must_use]
pub fn mcp_unauthenticated_response() -> Response<Body> {
    AletheiaHttpError::Unauthorized.into_response()
}

/// The `403 PERMISSION_DENIED` envelope for a `/mcp` per-tool RBAC refusal,
/// carrying `details: {required_class, principal_role}` (ruling F4). The
/// `error` message and `details` shape mirror the legacy MCP
/// `permission_denied_error` exactly, so an LLM/caller branches on `code` +
/// `details` without substring matching; `retriable` is `false` (a caller-fault
/// authorization denial is never retriable).
#[must_use]
pub fn mcp_permission_denied_response(denied: Denied) -> Response<Body> {
    let body = json!({
        "success": false,
        "error": format!(
            "role '{}' does not permit {} access",
            denied.principal_role, denied.required_class
        ),
        "code": "PERMISSION_DENIED",
        "retriable": false,
        "details": {
            "required_class": denied.required_class.to_string(),
            "principal_role": denied.principal_role.to_string(),
        },
    });
    (StatusCode::FORBIDDEN, Json(body)).into_response()
}

/// Parse a buffered `/mcp` JSON-RPC body and, **iff** it is a single
/// `tools/call`, return the target tool name. Any other method
/// (`initialize`, `tools/list`, notifications), a batch array, or an
/// unparseable body yields `None` — those carry no per-tool RBAC decision (the
/// catalog methods are gated by authentication alone).
fn tools_call_target(bytes: &[u8]) -> Option<String> {
    let value: serde_json::Value = serde_json::from_slice(bytes).ok()?;
    if value.get("method").and_then(|m| m.as_str()) != Some("tools/call") {
        return None;
    }
    value
        .get("params")
        .and_then(|p| p.get("name"))
        .and_then(|n| n.as_str())
        .map(str::to_owned)
}

/// Custom tower [`Layer`](tower::Layer) gating the whole `/mcp` endpoint with
/// `x-api-key`-aware authentication + per-tool RBAC (rulings F2/F4). Mounted via
/// [`secure_mcp`](autumn_web::app::AppBuilder::secure_mcp).
#[derive(Clone)]
pub struct McpSecurityLayer {
    store: Arc<AuthStore>,
    mode: AuthMode,
}

impl McpSecurityLayer {
    /// Build the gate from the shared credential store and the auth mode.
    #[must_use]
    pub fn new(store: Arc<AuthStore>, mode: AuthMode) -> Self {
        Self { store, mode }
    }
}

impl<S> tower::Layer<S> for McpSecurityLayer {
    type Service = McpSecurityService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        McpSecurityService {
            inner,
            store: Arc::clone(&self.store),
            mode: self.mode,
        }
    }
}

/// The tower [`Service`](tower::Service) produced by [`McpSecurityLayer`].
#[derive(Clone)]
pub struct McpSecurityService<S> {
    inner: S,
    store: Arc<AuthStore>,
    mode: AuthMode,
}

impl<S> tower::Service<Request<Body>> for McpSecurityService<S>
where
    S: tower::Service<Request<Body>, Response = Response<Body>, Error = Infallible>
        + Clone
        + Send
        + 'static,
    S::Future: Send + 'static,
{
    type Response = Response<Body>;
    type Error = Infallible;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: Request<Body>) -> Self::Future {
        let store = Arc::clone(&self.store);
        let mode = self.mode;
        // Clone-and-swap so the inner service driven in the async block is the
        // `poll_ready`-ed one (standard tower pattern; the swapped-in clone is
        // not driven).
        let mut inner = self.inner.clone();
        std::mem::swap(&mut self.inner, &mut inner);

        Box::pin(async move {
            // 1. Authenticate (uniform 401 on any failure in Required mode).
            let principal = match mode {
                AuthMode::Anonymous => Principal::anonymous(),
                AuthMode::Required => {
                    match extract_credential_headers(req.headers())
                        .and_then(|cred| store.verify(&cred))
                    {
                        Some(principal) => principal,
                        None => return Ok(mcp_unauthenticated_response()),
                    }
                }
            };

            // 2. Buffer the body so a `tools/call` can be RBAC-pre-checked at
            //    the envelope (F4). Authentication above needed only headers;
            //    only this per-tool decision needs the body.
            let (parts, body) = req.into_parts();
            let bytes = match axum::body::to_bytes(body, MAX_MCP_REQUEST_BYTES).await {
                Ok(bytes) => bytes,
                // An unreadable/oversized body is not a valid JSON-RPC call;
                // pass an empty body through so dispatch produces its own error
                // (auth already succeeded).
                Err(_) => axum::body::Bytes::new(),
            };

            // 3. Per-tool RBAC pre-check for tools/call (F4).
            if let Some(tool) = tools_call_target(&bytes)
                && let Err(denied) = rbac::authorize_tool(principal.role, &tool)
            {
                return Ok(mcp_permission_denied_response(denied));
            }

            // 4. Rebuild the request, cache the verified principal in
            //    extensions (F3 seam), and pass through.
            let mut req = Request::from_parts(parts, Body::from(bytes));
            req.extensions_mut().insert(principal);
            inner.call(req).await
        })
    }
}
