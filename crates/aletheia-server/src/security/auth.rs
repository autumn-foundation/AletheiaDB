// OWNED BY LANE B (security).
//
//! Authentication + RBAC scaffolding for the production server.
//!
//! Generalizes the spike's `SpikeAuth` into a **class-parameterized**
//! [`Authorized<C>`] extractor and a marker-type RBAC vocabulary
//! ([`AccessClassMarker`] + [`ReadClass`]/[`WriteClass`]/[`MetricsClass`]/
//! [`AdminClass`]). PR1 wires the *authentication* half (verify the
//! bearer/`x-api-key` credential against the shared constant-time
//! [`AuthStore`]); **RBAC-class enforcement is a marked TODO for Lane B**.
//!
//! Also provides [`AuthStoreTokenAdapter`] (autumn's native
//! [`ApiTokenStore`](autumn_web::auth::ApiTokenStore) backed by the shared
//! store, for `secure_mcp`) and the [`ApiKeyStore`] extractor the admin
//! key-lifecycle handlers use to reach the store.

use aletheiadb::auth::{AccessClass, AuthMode, AuthStore, Principal};
use aletheiadb::http::AletheiaHttpError;
use autumn_web::prelude::AppState;
use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use std::future::Future;
use std::marker::PhantomData;
use std::pin::Pin;
use std::sync::Arc;

/// Shared authentication state: the key store plus the configured mode.
///
/// Installed once into the app's extension bag by
/// [`init_state`](crate::security::init_state) and pulled back out by the
/// [`Authorized`] and [`ApiKeyStore`] extractors.
#[derive(Clone)]
pub struct ServerAuthState {
    store: Arc<AuthStore>,
    mode: AuthMode,
}

impl ServerAuthState {
    /// Build auth state from a shared store and mode.
    #[must_use]
    pub fn new(store: Arc<AuthStore>, mode: AuthMode) -> Self {
        Self { store, mode }
    }

    /// The shared key store.
    #[must_use]
    pub fn store(&self) -> &Arc<AuthStore> {
        &self.store
    }

    /// The configured authentication mode.
    #[must_use]
    pub fn mode(&self) -> AuthMode {
        self.mode
    }
}

/// Compile-time marker binding a zero-size type to an [`AccessClass`].
///
/// Lets a handler declare the class it needs in the type system —
/// `Authorized<ReadClass>` — so the schema surfacing and (Lane B's) RBAC gate
/// key off one source. Generalizes the spike's fixed `AccessClass::Read`.
pub trait AccessClassMarker {
    /// The access class this marker denotes.
    const CLASS: AccessClass;
}

/// Marker: [`AccessClass::Read`].
#[derive(Debug, Clone, Copy)]
pub struct ReadClass;
impl AccessClassMarker for ReadClass {
    const CLASS: AccessClass = AccessClass::Read;
}

/// Marker: [`AccessClass::Write`].
#[derive(Debug, Clone, Copy)]
pub struct WriteClass;
impl AccessClassMarker for WriteClass {
    const CLASS: AccessClass = AccessClass::Write;
}

/// Marker: [`AccessClass::Metrics`].
#[derive(Debug, Clone, Copy)]
pub struct MetricsClass;
impl AccessClassMarker for MetricsClass {
    const CLASS: AccessClass = AccessClass::Metrics;
}

/// Marker: [`AccessClass::Admin`].
#[derive(Debug, Clone, Copy)]
pub struct AdminClass;
impl AccessClassMarker for AdminClass {
    const CLASS: AccessClass = AccessClass::Admin;
}

/// Class-parameterized auth extractor: the verified [`Principal`] gated by the
/// [`AccessClass`] denoted by `C`.
///
/// Field `0` is the verified principal (public, mirroring the requested shape);
/// the [`PhantomData`] carries the class marker `C` in the type only.
///
/// PR1 performs **authentication** (uniform 401 on a missing/invalid credential
/// in required mode; anonymous mode yields a synthetic anonymous principal via
/// the store). RBAC-class enforcement is NOT yet done here.
pub struct Authorized<C>(pub Principal, PhantomData<fn() -> C>);

impl<C> Authorized<C> {
    /// The verified principal.
    #[must_use]
    pub fn principal(&self) -> &Principal {
        &self.0
    }
}

impl<C: AccessClassMarker> FromRequestParts<AppState> for Authorized<C> {
    type Rejection = AletheiaHttpError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let auth = state
            .extension::<ServerAuthState>()
            .map(|arc| (*arc).clone())
            .ok_or(AletheiaHttpError::StateMissing)?;

        // F3 (Issue #3524 B4): principal caching. If a preceding tower layer in
        // this same request pipeline (e.g. the `/mcp` security gate) already
        // authenticated the credential and inserted the verified [`Principal`]
        // into request extensions, reuse it verbatim instead of performing a
        // second `AuthStore::verify` — the class check below still runs, so
        // behavior is identical, only the redundant constant-time scan is
        // avoided. When no layer precedes the extractor (the direct HTTP route
        // path, which is deliberately NOT fronted by a redundant auth layer so
        // `/openapi.json` stays public), extensions carry no `Principal` and we
        // authenticate exactly as before — one verify, revocation-immediate.
        let principal = if let Some(cached) = parts.extensions.get::<Principal>() {
            cached.clone()
        } else {
            match auth.mode() {
                // Anonymous mode: no credential required. Authorizes with the
                // synthetic full-access principal (anonymous == fully privileged).
                AuthMode::Anonymous => Principal::anonymous(),
                AuthMode::Required => {
                    // Uniform failure: missing / malformed / unknown / revoked
                    // are indistinguishable (byte-identical 401 to legacy).
                    let credential =
                        extract_credential(parts).ok_or(AletheiaHttpError::Unauthorized)?;
                    auth.store()
                        .verify(&credential)
                        .ok_or(AletheiaHttpError::Unauthorized)?
                }
            }
        };

        // RBAC-class enforcement: reject with a byte-identical 403 unless the
        // authenticated principal's role permits `C::CLASS`. In anonymous mode
        // `Principal::anonymous()` carries `Role::Admin`, so every class passes —
        // matching anonymous mode's documented full access.
        authorize::<C>(&principal)?;
        Ok(Self(principal, PhantomData))
    }
}

/// Extractor handing out the shared [`AuthStore`] for the admin key-lifecycle
/// handlers (create/list/revoke), which operate on the store directly rather
/// than a principal. Reads the same [`ServerAuthState`] extension.
pub struct ApiKeyStore(pub Arc<AuthStore>);

impl FromRequestParts<AppState> for ApiKeyStore {
    type Rejection = AletheiaHttpError;

    async fn from_request_parts(
        _parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let auth = state
            .extension::<ServerAuthState>()
            .map(|arc| (*arc).clone())
            .ok_or(AletheiaHttpError::StateMissing)?;
        Ok(Self(auth.store().clone()))
    }
}

/// Per-IP rate-limit settings for the default-**off** `tower-governor` layer
/// (Lane B3). Present (`Some`) on [`SecurityConfig`](crate::security::SecurityConfig)
/// only when the operator opts in; the effective switch for
/// [`governor_layer`](crate::security::rate_limit::governor_layer) is
/// `SecurityConfig::rate_limit.is_some()`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RateLimitSettings {
    /// Sustained requests per second replenished per key (per peer IP).
    pub rps: u64,
    /// Burst bucket size — the number of requests allowed to arrive at once
    /// before the per-second replenishment throttles them.
    pub burst: u32,
}

impl RateLimitSettings {
    /// Build settings from a sustained rate and a burst allowance.
    #[must_use]
    pub fn new(rps: u64, burst: u32) -> Self {
        Self { rps, burst }
    }
}

/// Authorize an authenticated `principal` for an [`AccessClass`].
///
/// The per-request RBAC decision. On refusal returns
/// [`AletheiaHttpError::PermissionDenied`] (HTTP 403 / `PERMISSION_DENIED`,
/// non-retriable) with the **byte-identical** legacy message
/// `"role '{role}' does not permit {class} access"`, so 403 bodies stay
/// identical to the existing `/query` surface.
///
/// # Errors
///
/// Returns [`AletheiaHttpError::PermissionDenied`] when `principal.role` does
/// not permit `class`.
pub fn authorize_class(principal: &Principal, class: AccessClass) -> Result<(), AletheiaHttpError> {
    if principal.role.allows(class) {
        Ok(())
    } else {
        Err(AletheiaHttpError::PermissionDenied(format!(
            "role '{}' does not permit {} access",
            principal.role, class
        )))
    }
}

/// Marker-generic wrapper over [`authorize_class`] — the shape the
/// [`Authorized<C>`] extractor calls. Reads the required class from the type
/// parameter's [`AccessClassMarker::CLASS`], so a handler's declared class
/// (`Authorized<WriteClass>`) is the one enforced.
///
/// # Errors
///
/// Returns [`AletheiaHttpError::PermissionDenied`] when `principal.role` does
/// not permit `C::CLASS`.
pub fn authorize<C: AccessClassMarker>(principal: &Principal) -> Result<(), AletheiaHttpError> {
    authorize_class(principal, C::CLASS)
}

/// Extract the bearer/`x-api-key` credential from request headers.
///
/// Returns `None` for missing or malformed headers — every failure collapses
/// into the same uniform 401 upstream, deliberately. The bearer scheme match is
/// ASCII-case-insensitive; a whitespace-only or empty credential is rejected.
#[must_use = "the extracted credential must be verified against the auth store"]
pub fn extract_credential(parts: &Parts) -> Option<String> {
    extract_credential_headers(&parts.headers)
}

/// Header-map form of [`extract_credential`] — the credential extractor a tower
/// `Service` uses, which holds an `http::Request` (and thus a `&HeaderMap`)
/// rather than an extractor's `&Parts`. Same contract: `Authorization: Bearer`
/// (ASCII-case-insensitive scheme) first, then `x-api-key`; both trimmed, empty
/// rejected; any malformed header collapses to `None` (the uniform-401 upstream).
///
/// [`extract_credential`] delegates here so the two entry points can never
/// diverge on what counts as a valid credential.
#[must_use = "the extracted credential must be verified against the auth store"]
pub fn extract_credential_headers(headers: &axum::http::HeaderMap) -> Option<String> {
    if let Some(value) = headers.get(axum::http::header::AUTHORIZATION) {
        let s = value.to_str().ok()?;
        let (scheme, rest) = s.split_once(' ')?;
        if !scheme.eq_ignore_ascii_case("bearer") {
            return None;
        }
        let token = rest.trim();
        if token.is_empty() {
            return None;
        }
        return Some(token.to_owned());
    }
    if let Some(value) = headers.get("x-api-key") {
        let token = value.to_str().ok()?.trim();
        if token.is_empty() {
            return None;
        }
        return Some(token.to_owned());
    }
    None
}

/// Adapts AletheiaDB's [`AuthStore`] to autumn's
/// [`ApiTokenStore`](autumn_web::auth::ApiTokenStore) so autumn's native
/// [`RequireApiToken`](autumn_web::auth::RequireApiToken) layer (used by
/// `secure_mcp`) verifies tokens against the same constant-time-verified store
/// the extractors use. Only `verify` is meaningful; key lifecycle is owned by
/// the `/admin/keys*` endpoints, so `issue`/`revoke` return an "unsupported"
/// error.
pub struct AuthStoreTokenAdapter {
    store: Arc<AuthStore>,
}

impl AuthStoreTokenAdapter {
    /// Wrap a shared [`AuthStore`].
    #[must_use]
    pub fn new(store: Arc<AuthStore>) -> Self {
        Self { store }
    }
}

impl autumn_web::auth::ApiTokenStore for AuthStoreTokenAdapter {
    fn issue<'a>(
        &'a self,
        _principal_id: &'a str,
    ) -> Pin<Box<dyn Future<Output = autumn_web::AutumnResult<String>> + Send + 'a>> {
        Box::pin(async move {
            Err(autumn_web::AutumnError::internal_server_error_msg(
                "token issuance is owned by AletheiaDB's /admin/keys endpoints",
            ))
        })
    }

    fn verify<'a>(
        &'a self,
        raw_token: &'a str,
    ) -> Pin<Box<dyn Future<Output = autumn_web::AutumnResult<Option<String>>> + Send + 'a>> {
        // Constant-time verify via the shared store; return the principal id.
        Box::pin(async move { Ok(self.store.verify(raw_token).map(|p| p.id)) })
    }

    fn revoke<'a>(
        &'a self,
        _raw_token: &'a str,
    ) -> Pin<Box<dyn Future<Output = autumn_web::AutumnResult<()>> + Send + 'a>> {
        Box::pin(async move {
            Err(autumn_web::AutumnError::internal_server_error_msg(
                "token revocation is owned by AletheiaDB's /admin/keys endpoints",
            ))
        })
    }
}
