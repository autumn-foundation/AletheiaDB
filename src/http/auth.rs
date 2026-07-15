//! HTTP authentication extractor and authorization context (Issue #3350).
//!
//! autumn-web's sealed `IntoAppLayer` bound blocks tower middleware, so auth
//! is enforced the same way [`AppState`](crate::http::AppState) is delivered:
//! an axum extractor ([`AuthContext`]) that every handler runs first, plus
//! per-operation authorization inside the `/query` dispatch. This works
//! identically in `run_server` and `build_test_router`.
//!
//! Credential transport: `Authorization: Bearer <key>` (preferred) or
//! `x-api-key: <key>`. All authentication failures — missing header,
//! malformed header, unknown key, revoked key — return the identical 401
//! response (uniform body, `WWW-Authenticate: Bearer` header, never echoing
//! the presented credential).

use crate::auth::{AccessClass, AuthMode, AuthStore, Principal};
use crate::http::error::AletheiaHttpError;
use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use std::sync::Arc;

/// Shared authentication state: the key store plus the configured mode.
///
/// Installed into autumn's extensions bag at startup (next to
/// [`AppState`](crate::http::AppState)) and pulled out by the
/// [`AuthContext`] extractor on every request.
#[derive(Clone)]
pub struct AuthState {
    store: Arc<AuthStore>,
    mode: AuthMode,
}

impl AuthState {
    /// Create auth state from a store and mode.
    #[must_use]
    pub fn new(store: Arc<AuthStore>, mode: AuthMode) -> Self {
        Self { store, mode }
    }

    /// The underlying key store.
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

/// The authenticated identity of the current request.
#[derive(Debug, Clone)]
enum Identity {
    /// Anonymous mode: the request is treated as fully privileged.
    Anonymous,
    /// A verified API-key principal.
    Principal(Principal),
}

/// Per-request authentication context.
///
/// Extracting this value performs *authentication* (credential → principal).
/// Handlers must then call [`AuthContext::authorize`] with the operation's
/// [`AccessClass`] before doing any database work (*authorization*).
pub struct AuthContext {
    identity: Identity,
    store: Arc<AuthStore>,
}

impl AuthContext {
    /// Authorize the current identity for `class`.
    ///
    /// # Errors
    ///
    /// Returns [`AletheiaHttpError::PermissionDenied`] (HTTP 403) when the
    /// principal's role does not allow the access class. Anonymous identities
    /// (explicit anonymous mode) are allowed everything.
    pub fn authorize(&self, class: AccessClass) -> Result<(), AletheiaHttpError> {
        match &self.identity {
            Identity::Anonymous => Ok(()),
            Identity::Principal(p) => {
                if p.role.allows(class) {
                    Ok(())
                } else {
                    Err(AletheiaHttpError::PermissionDenied {
                        required_class: class,
                        principal_role: p.role,
                    })
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

    /// The shared key store (for admin key-lifecycle handlers).
    #[must_use]
    pub fn store(&self) -> &Arc<AuthStore> {
        &self.store
    }
}

/// Pull the bearer/API-key credential out of the request headers.
///
/// Returns `None` for missing or malformed headers — every failure collapses
/// into the same uniform 401 upstream, deliberately.
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
        return Some(token.to_string());
    }
    if let Some(value) = parts.headers.get("x-api-key") {
        let token = value.to_str().ok()?.trim();
        if token.is_empty() {
            return None;
        }
        return Some(token.to_string());
    }
    None
}

impl FromRequestParts<autumn_web::prelude::AppState> for AuthContext {
    type Rejection = AletheiaHttpError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &autumn_web::prelude::AppState,
    ) -> Result<Self, Self::Rejection> {
        let auth = state
            .extension::<AuthState>()
            .map(|arc| (*arc).clone())
            .ok_or(AletheiaHttpError::StateMissing)?;

        match auth.mode() {
            AuthMode::Anonymous => Ok(Self {
                identity: Identity::Anonymous,
                store: auth.store().clone(),
            }),
            AuthMode::Required => {
                // Uniform failure: missing header, malformed header, unknown
                // key, and revoked key are indistinguishable to the caller.
                let credential =
                    extract_credential(parts).ok_or(AletheiaHttpError::Unauthorized)?;
                let principal = auth
                    .store()
                    .verify(&credential)
                    .ok_or(AletheiaHttpError::Unauthorized)?;
                Ok(Self {
                    identity: Identity::Principal(principal),
                    store: auth.store().clone(),
                })
            }
        }
    }
}

/// Startup validation for auth configuration.
///
/// [`AuthMode::Required`] (the conservative default) with zero credentials
/// available means every request would be rejected — refuse to start instead
/// so the operator notices immediately.
///
/// # Errors
///
/// Returns a human-readable message when `mode` is `Required` and `store`
/// holds no keys.
pub fn validate_auth_startup(mode: AuthMode, store: &AuthStore) -> Result<(), String> {
    match mode {
        AuthMode::Anonymous => Ok(()),
        AuthMode::Required => {
            if store.is_empty() {
                Err(
                    "authentication is required (the default) but no credentials are \
                     available: set a bootstrap admin key (ALETHEIADB_BOOTSTRAP_ADMIN_KEY \
                     or ServerConfig::builder().bootstrap_admin_key(...)), point the \
                     server at an existing persisted key store, or explicitly opt into \
                     anonymous mode (ALETHEIADB_AUTH_MODE=anonymous)"
                        .to_string(),
                )
            } else {
                Ok(())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::Role;

    #[test]
    fn validate_auth_startup_rejects_required_with_no_keys() {
        let store = AuthStore::new();
        let err = validate_auth_startup(AuthMode::Required, &store)
            .expect_err("empty store + required mode must refuse startup");
        assert!(err.contains("ALETHEIADB_BOOTSTRAP_ADMIN_KEY"));
        assert!(err.contains("anonymous"));
    }

    #[test]
    fn validate_auth_startup_accepts_required_with_a_key() {
        let store = AuthStore::new();
        store
            .insert_bootstrap_key("bootstrap-admin", Role::Admin, "some-key")
            .expect("bootstrap");
        assert!(validate_auth_startup(AuthMode::Required, &store).is_ok());
    }

    #[test]
    fn validate_auth_startup_accepts_anonymous_with_no_keys() {
        let store = AuthStore::new();
        assert!(validate_auth_startup(AuthMode::Anonymous, &store).is_ok());
    }
}
