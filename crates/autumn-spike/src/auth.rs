//! Authentication + RBAC for the spike, reusing AletheiaDB's shared core.
//!
//! Two collaborating pieces, both delegating to the *same*
//! [`aletheiadb::auth::AuthStore`] (SHA-256 hashes, constant-time verify):
//!
//! 1. [`SpikeAuth`] — a `FromRequestParts` extractor that authenticates the
//!    bearer/`x-api-key` credential and yields the verified [`Principal`], then
//!    [`authorize`](SpikeAuth::authorize)s an [`AccessClass`]. This is the RBAC
//!    path: autumn's native token layer only proves a token is *valid*, it
//!    carries no role, so role→class enforcement lives here (the "thin RBAC
//!    adapter"). Bound to autumn **0.5**'s `AppState`.
//!
//! 2. [`AuthStoreTokenAdapter`] — implements autumn's
//!    [`ApiTokenStore`](autumn_web::auth::ApiTokenStore) by delegating `verify`
//!    to the shared `AuthStore`, so autumn's native
//!    [`RequireApiToken`](autumn_web::auth::RequireApiToken) tower layer (used
//!    with `secure_mcp`) authenticates against AletheiaDB's real credential
//!    store. Demonstrates that the sealed-`IntoAppLayer` limitation of 0.4
//!    (ADR 0055) is lifted in 0.5.

use aletheiadb::auth::{AccessClass, AuthMode, AuthStore, Principal};
use aletheiadb::http::AletheiaHttpError;
use autumn_web::prelude::AppState;
use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

/// Shared authentication state: the key store plus the configured mode.
///
/// Installed into the app's extension bag at startup, next to
/// [`SpikeState`](crate::state::SpikeState).
#[derive(Clone)]
pub struct SpikeAuthState {
    store: Arc<AuthStore>,
    mode: AuthMode,
}

impl SpikeAuthState {
    /// Build auth state from a shared store and a mode.
    #[must_use]
    pub fn new(store: Arc<AuthStore>, mode: AuthMode) -> Self {
        Self { store, mode }
    }

    /// The shared key store.
    #[must_use]
    pub fn store(&self) -> &Arc<AuthStore> {
        &self.store
    }
}

/// The authenticated identity for the current request.
#[derive(Debug, Clone)]
enum Identity {
    /// Explicit anonymous mode: treated as fully privileged.
    Anonymous,
    /// A verified API-key principal.
    Principal(Principal),
}

/// Per-request authentication context (authentication done; call
/// [`authorize`](Self::authorize) for authorization).
pub struct SpikeAuth {
    identity: Identity,
}

impl SpikeAuth {
    /// Authorize the current identity for `class`.
    ///
    /// # Errors
    ///
    /// Returns [`AletheiaHttpError::PermissionDenied`] (HTTP 403) when the
    /// principal's role does not allow the class — with the **same** message
    /// the existing `/query` surface produces (`src/http/auth.rs`), so the body
    /// is byte-identical. Anonymous identities are allowed everything.
    pub fn authorize(&self, class: AccessClass) -> Result<(), AletheiaHttpError> {
        match &self.identity {
            Identity::Anonymous => Ok(()),
            Identity::Principal(p) => {
                if p.role.allows(class) {
                    Ok(())
                } else {
                    Err(AletheiaHttpError::PermissionDenied(format!(
                        "role '{}' does not permit {} access",
                        p.role, class
                    )))
                }
            }
        }
    }

    /// The verified principal, or `None` in anonymous mode.
    #[must_use]
    pub fn principal(&self) -> Option<&Principal> {
        match &self.identity {
            Identity::Anonymous => None,
            Identity::Principal(p) => Some(p),
        }
    }
}

/// Extract the bearer/`x-api-key` credential from request headers.
///
/// Returns `None` for missing or malformed headers — every failure collapses
/// into the same uniform 401 upstream, deliberately (no missing/unknown/revoked
/// distinction).
fn extract_credential(parts: &Parts) -> Option<String> {
    if let Some(value) = parts.headers.get(axum::http::header::AUTHORIZATION) {
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
    if let Some(value) = parts.headers.get("x-api-key") {
        let token = value.to_str().ok()?.trim();
        if token.is_empty() {
            return None;
        }
        return Some(token.to_owned());
    }
    None
}

impl FromRequestParts<AppState> for SpikeAuth {
    type Rejection = AletheiaHttpError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let auth = state
            .extension::<SpikeAuthState>()
            .map(|arc| (*arc).clone())
            .ok_or(AletheiaHttpError::StateMissing)?;

        match auth.mode {
            AuthMode::Anonymous => Ok(Self {
                identity: Identity::Anonymous,
            }),
            AuthMode::Required => {
                // Uniform failure: missing / malformed / unknown / revoked are
                // indistinguishable to the caller (byte-identical 401 to the
                // existing surface).
                let credential =
                    extract_credential(parts).ok_or(AletheiaHttpError::Unauthorized)?;
                let principal = auth
                    .store
                    .verify(&credential)
                    .ok_or(AletheiaHttpError::Unauthorized)?;
                Ok(Self {
                    identity: Identity::Principal(principal),
                })
            }
        }
    }
}

/// Adapts AletheiaDB's [`AuthStore`] to autumn's
/// [`ApiTokenStore`](autumn_web::auth::ApiTokenStore) trait, so autumn's native
/// [`RequireApiToken`](autumn_web::auth::RequireApiToken) tower layer verifies
/// tokens against the same constant-time-verified store the extractor uses.
///
/// Only [`verify`](autumn_web::auth::ApiTokenStore::verify) is meaningful for
/// `RequireApiToken`; key lifecycle (`issue`/`revoke`) is owned by AletheiaDB's
/// `/admin/keys*` endpoints, so those return an "unsupported" error here.
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
                "token issuance is owned by AletheiaDB's /admin/keys endpoints, \
                 not the spike adapter",
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
                "token revocation is owned by AletheiaDB's /admin/keys endpoints, \
                 not the spike adapter",
            ))
        })
    }
}
