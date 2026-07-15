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
//!    nested `{error:{code, message, retriable, details}}` shape the HTTP surface
//!    and the MCP tool dispatch emit (Issue #3234). (The replayed
//!    dispatch handler's own `Authorized<C>` gate remains as defense-in-depth;
//!    see the crate/README notes on the tools/call replay boundary.)
//! 4. **Fails closed** on frames it cannot RBAC-precheck (#6): an oversize
//!    buffered body (> [`MAX_MCP_REQUEST_BYTES`]) returns a `413`
//!    ([`mcp_payload_too_large_response`]) instead of being swallowed and
//!    forwarded as an empty body; a `tools/call` with no string `params.name`,
//!    or a JSON-RPC **batch array** containing a `tools/call`, returns a `400`
//!    ([`mcp_unroutable_tool_call_response`]) rather than passing through
//!    un-gated (batched tool calls must be sent as individual requests in v1).
//! 5. **Normalizes the verified credential** onto `Authorization: Bearer
//!    <token>` for the tools/call replay (see [`normalize_authorization`]),
//!    dropping the `x-api-key` header. autumn 0.5 forwards only `authorization`
//!    (its `FORWARDED_HEADERS`), so this gives `x-api-key` full parity on
//!    `tools/call` (the replayed handler re-verifies the normalized bearer)
//!    while guaranteeing the losing/unverified header never survives. Only the
//!    credential the layer actually verified is normalized.
//!
//! # Unified 403 envelope (ruling F4 / Issue #3234)
//!
//! Both the MCP-surface 403 ([`mcp_permission_denied_response`]) and the
//! HTTP-surface 403 ([`crate::security::authorize_class`] →
//! [`AletheiaHttpError::PermissionDenied`]) emit the **same** nested
//! `{error:{code, message, retriable, details}}` envelope, including
//! `details: {required_class, principal_role}`. The legacy asymmetry (a
//! message-only HTTP 403) was retired in the breaking HTTP error-envelope
//! unification, so the two surfaces are now byte-shape-identical. Because the
//! gate fails closed on every tool-executing frame it cannot identify (point 4),
//! no MCP tools/call reaches the dispatch handler's 403 path without first
//! passing the gate's detail-carrying precheck.
//!
//! In [`AuthMode::Anonymous`] the layer inserts [`Principal::anonymous`] and
//! passes through unconditionally (catalog reachable — matching the legacy
//! anonymous surface); no credential is verified, so no normalization occurs.

use std::convert::Infallible;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use aletheiadb::auth::{AuthMode, AuthStore, Principal};
use aletheiadb::http::AletheiaHttpError;
use axum::Json;
use axum::body::Body;
use axum::http::header::AUTHORIZATION;
use axum::http::{HeaderMap, HeaderValue, Request, Response, StatusCode};
use axum::response::IntoResponse;
use serde_json::json;

use crate::security::auth::extract_credential_headers;
use crate::security::rbac::{self, Denied};

/// Case-insensitive name of the `x-api-key` transport header (the alternative to
/// `Authorization: Bearer` the gate accepts and then normalizes away).
const X_API_KEY: &str = "x-api-key";

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

/// Build the unified nested error envelope `{error:{code, message, retriable,
/// details}}` (Issue #3234) shared with the HTTP surface
/// ([`AletheiaHttpError::into_response`]), so the `/mcp` gate's hand-built
/// refusals stay byte-shape-identical to the rest of the surface.
fn mcp_error_envelope(
    status: StatusCode,
    code: &str,
    message: String,
    retriable: bool,
    details: serde_json::Value,
) -> Response<Body> {
    let body = json!({
        "error": {
            "code": code,
            "message": message,
            "retriable": retriable,
            "details": details,
        }
    });
    (status, Json(body)).into_response()
}

/// The `403 PERMISSION_DENIED` envelope for a `/mcp` per-tool RBAC refusal,
/// carrying `details: {required_class, principal_role}` (ruling F4). The
/// `error` message and `details` shape mirror the MCP tool dispatch and the
/// HTTP surface exactly (the unified nested envelope, Issue #3234), so an
/// LLM/caller branches on `code` + `details` without substring matching;
/// `retriable` is `false` (a caller-fault authorization denial is never
/// retriable).
#[must_use]
pub fn mcp_permission_denied_response(denied: Denied) -> Response<Body> {
    mcp_error_envelope(
        StatusCode::FORBIDDEN,
        "PERMISSION_DENIED",
        format!(
            "role '{}' does not permit {} access",
            denied.principal_role, denied.required_class
        ),
        false,
        json!({
            "required_class": denied.required_class.to_string(),
            "principal_role": denied.principal_role.to_string(),
        }),
    )
}

/// The `413 RESOURCE_EXHAUSTED` envelope for an over-limit buffered `/mcp` body
/// (SHOULD-FIX #2). A body larger than [`MAX_MCP_REQUEST_BYTES`] is refused
/// fail-closed — never swallowed and forwarded as an empty body — so an
/// oversize request cannot silently reach dispatch. Non-retriable (a caller
/// fault), mirroring the shared `ResultBytes` 413.
#[must_use]
pub fn mcp_payload_too_large_response() -> Response<Body> {
    mcp_error_envelope(
        StatusCode::PAYLOAD_TOO_LARGE,
        "RESOURCE_EXHAUSTED",
        format!("request body exceeds the {MAX_MCP_REQUEST_BYTES}-byte /mcp limit"),
        false,
        json!({
            "reason": "request_body_too_large",
            "limit": MAX_MCP_REQUEST_BYTES,
        }),
    )
}

/// The `400 INVALID_ARGUMENT` envelope for a tool-executing frame the gate
/// cannot RBAC-precheck (#6): a `tools/call` with no resolvable string
/// `params.name`, or a JSON-RPC **batch array** carrying a `tools/call`. Rather
/// than pass such a frame through un-checked (potential under-gating), the gate
/// refuses it fail-closed. Non-retriable.
#[must_use]
pub fn mcp_unroutable_tool_call_response(reason: &'static str) -> Response<Body> {
    mcp_error_envelope(
        StatusCode::BAD_REQUEST,
        "INVALID_ARGUMENT",
        format!("unroutable tools/call frame refused: {reason}"),
        false,
        json!({ "reason": reason }),
    )
}

/// The classification of a buffered `/mcp` JSON-RPC frame for RBAC prechecking.
enum McpFrame {
    /// Not a tool-executing frame (`initialize`, `tools/list`, a notification, a
    /// batch with no `tools/call`, or an unparseable/empty body) — gated by
    /// authentication alone, no per-tool RBAC decision.
    PassThrough,
    /// A single `tools/call` naming this tool — RBAC-prechecked at the envelope.
    ToolCall(String),
    /// A tool-executing frame the gate cannot identify: a `tools/call` with no
    /// string `params.name`, or a batch array containing a `tools/call`. Refused
    /// fail-closed (#6) with the carried reason.
    Unroutable(&'static str),
}

/// Classify a buffered `/mcp` body for the per-tool RBAC precheck.
///
/// A single JSON-RPC object with `method == "tools/call"` yields
/// [`McpFrame::ToolCall`] (or [`McpFrame::Unroutable`] if its `params.name` is
/// missing/non-string). A **batch array** containing any `tools/call` element is
/// [`McpFrame::Unroutable`] — the envelope precheck cannot express multiple
/// per-tool decisions, so the gate refuses it fail-closed rather than risk
/// under-gating. Everything else is [`McpFrame::PassThrough`].
fn classify_mcp_frame(bytes: &[u8]) -> McpFrame {
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(bytes) else {
        // Unparseable/empty: not a routable tool call; authentication already
        // applied, and dispatch will produce its own JSON-RPC error.
        return McpFrame::PassThrough;
    };

    // JSON-RPC batch: an array of frames. If any is a tools/call, refuse
    // fail-closed (the per-tool RBAC precheck cannot fan out over a batch).
    if let Some(elements) = value.as_array() {
        let has_tool_call = elements
            .iter()
            .any(|e| e.get("method").and_then(|m| m.as_str()) == Some("tools/call"));
        return if has_tool_call {
            McpFrame::Unroutable("batched tools/call must be sent as individual requests")
        } else {
            McpFrame::PassThrough
        };
    }

    if value.get("method").and_then(|m| m.as_str()) != Some("tools/call") {
        return McpFrame::PassThrough;
    }
    match value
        .get("params")
        .and_then(|p| p.get("name"))
        .and_then(|n| n.as_str())
    {
        Some(name) => McpFrame::ToolCall(name.to_owned()),
        None => McpFrame::Unroutable("tools/call is missing a string params.name"),
    }
}

/// Normalize the verified credential onto `Authorization: Bearer <token>` for
/// the tools/call replay, and drop the `x-api-key` header.
///
/// autumn 0.5 forwards only `authorization` (its `FORWARDED_HEADERS`) into the
/// replayed dispatch, so an `x-api-key`-authenticated caller would otherwise
/// lose its credential at the replay boundary. Rewriting the header the gate
/// **actually verified** into a bearer gives `x-api-key` full parity while
/// ensuring the loser/unverified header never survives into the replay. Only
/// called with a credential the layer verified; the token is a valid header
/// value (it came from one), so the rewrite cannot fail in practice — an
/// unexpected non-header token is dropped rather than forwarded unverified.
fn normalize_authorization(headers: &mut HeaderMap, verified_token: &str) {
    match HeaderValue::try_from(format!("Bearer {verified_token}")) {
        Ok(value) => {
            headers.insert(AUTHORIZATION, value);
        }
        Err(_) => {
            // Unreachable in practice; fail closed by removing any credential
            // rather than forwarding an unverified one.
            headers.remove(AUTHORIZATION);
        }
    }
    headers.remove(X_API_KEY);
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
            //    Retain the *verified* token so it can be normalized onto the
            //    `authorization` header for the tools/call replay (below).
            let (principal, verified_token) = match mode {
                AuthMode::Anonymous => (Principal::anonymous(), None),
                AuthMode::Required => match extract_credential_headers(req.headers()) {
                    Some(credential) => match store.verify(&credential) {
                        Some(principal) => (principal, Some(credential)),
                        None => return Ok(mcp_unauthenticated_response()),
                    },
                    None => return Ok(mcp_unauthenticated_response()),
                },
            };

            // 2. Buffer the body so a `tools/call` can be RBAC-pre-checked at
            //    the envelope (F4). Authentication above needed only headers;
            //    only this per-tool decision needs the body. An oversize body is
            //    refused fail-closed with a 413 envelope (#2) — never swallowed
            //    and forwarded empty.
            let (mut parts, body) = req.into_parts();
            let bytes = match axum::body::to_bytes(body, MAX_MCP_REQUEST_BYTES).await {
                Ok(bytes) => bytes,
                Err(_) => return Ok(mcp_payload_too_large_response()),
            };

            // 3. Classify the frame and enforce per-tool RBAC (F4) / fail-close
            //    unroutable tool-executing frames (#6).
            match classify_mcp_frame(&bytes) {
                McpFrame::ToolCall(tool) => {
                    if let Err(denied) = rbac::authorize_tool(principal.role, &tool) {
                        return Ok(mcp_permission_denied_response(denied));
                    }
                }
                McpFrame::Unroutable(reason) => {
                    return Ok(mcp_unroutable_tool_call_response(reason));
                }
                McpFrame::PassThrough => {}
            }

            // 4. Normalize the verified credential onto `authorization: Bearer`
            //    for the replay (x-api-key parity), stripping the loser header so
            //    only the verified credential survives. No-op in anonymous mode
            //    (no verified credential).
            if let Some(token) = verified_token.as_deref() {
                normalize_authorization(&mut parts.headers, token);
            }

            // 5. Rebuild the request, cache the verified principal in
            //    extensions (F3 seam), and pass through.
            let mut req = Request::from_parts(parts, Body::from(bytes));
            req.extensions_mut().insert(principal);
            inner.call(req).await
        })
    }
}
