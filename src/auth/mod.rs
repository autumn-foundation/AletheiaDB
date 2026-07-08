//! Authentication and role-based access control core (Issue #3350, Phase 1).
//!
//! This module is transport-agnostic: it knows nothing about HTTP or MCP.
//! It provides:
//!
//! - [`Role`] — the four operator-facing roles (`admin`, `writer`, `reader`,
//!   `metrics`) and the [`Role::allows`] permission matrix over
//!   [`AccessClass`]es.
//! - [`Principal`] — a named identity bound to a role. Never holds key
//!   material; listing principals is masked by construction.
//! - [`AuthStore`] — a thread-safe store of API keys, keyed by the SHA-256
//!   digest of the full key string. Plaintext keys are returned exactly once
//!   at creation and never stored. Optional JSON file persistence writes
//!   only digests (atomic write, `0600` on unix).
//! - [`AuthMode`] — whether a serving surface requires credentials
//!   ([`AuthMode::Required`], the default) or has been explicitly opted into
//!   anonymous operation ([`AuthMode::Anonymous`]).
//!
//! Key format: `aletheia_sk_` followed by the base64url (no padding)
//! encoding of 32 bytes from the OS CSPRNG.
//!
//! Enforcement (extractors, per-operation authorization) lives in the
//! transport layers: `crate::http` today, `crate::mcp` in Phase 2.

mod role;
mod store;

pub use role::{AccessClass, Role, RoleParseError};
pub use store::{AuthError, AuthStore, KEY_SCHEME_PREFIX, Principal};

use std::str::FromStr;

/// Whether a serving surface requires authentication.
///
/// The conservative default is [`AuthMode::Required`]: a server refuses to
/// start without at least one credential unless the operator explicitly opts
/// into [`AuthMode::Anonymous`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AuthMode {
    /// Every request must present a valid API key (default).
    #[default]
    Required,
    /// No authentication: every request is treated as fully privileged.
    /// Explicit opt-in only; the server logs a prominent warning.
    Anonymous,
}

impl FromStr for AuthMode {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "required" => Ok(Self::Required),
            "anonymous" => Ok(Self::Anonymous),
            other => Err(format!(
                "invalid auth mode '{other}': expected 'required' or 'anonymous'"
            )),
        }
    }
}

impl std::fmt::Display for AuthMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Required => f.write_str("required"),
            Self::Anonymous => f.write_str("anonymous"),
        }
    }
}

/// Resolve an optionally-supplied auth-mode string (typically the value of
/// `ALETHEIADB_AUTH_MODE`) into an [`AuthMode`], fail-closed.
///
/// - `None` (variable unset) → [`AuthMode::Required`] (the default).
/// - A valid value (`"required"` / `"anonymous"`, case-insensitive,
///   whitespace-tolerant) → that mode.
/// - An invalid value → a warning on stderr and [`AuthMode::Required`] —
///   **never** anonymous, so a typo cannot silently disable authentication.
#[must_use]
pub fn parse_auth_mode_value(raw: Option<&str>) -> AuthMode {
    match raw {
        None => AuthMode::Required,
        Some(raw) => match raw.parse::<AuthMode>() {
            Ok(mode) => mode,
            Err(e) => {
                eprintln!("WARNING: {e}. Falling back to auth mode 'required'.");
                AuthMode::Required
            }
        },
    }
}

/// Read `ALETHEIADB_AUTH_MODE` from the environment via
/// [`parse_auth_mode_value`] (fail-closed: unset or invalid → `Required`).
///
/// Shared by the HTTP and MCP server binaries so both surfaces resolve the
/// variable with identical semantics.
#[must_use]
pub fn auth_mode_from_env() -> AuthMode {
    parse_auth_mode_value(std::env::var("ALETHEIADB_AUTH_MODE").ok().as_deref())
}

/// A secret string wrapper that never exposes its contents through `Debug`
/// and zeroizes its buffer on drop.
///
/// Used for plaintext credentials that must transit configuration (e.g. a
/// bootstrap admin key supplied via environment variable).
#[derive(Clone)]
pub struct SecretString(zeroize::Zeroizing<String>);

impl SecretString {
    /// Wrap a plaintext secret.
    pub fn new(value: impl Into<String>) -> Self {
        Self(zeroize::Zeroizing::new(value.into()))
    }

    /// Expose the plaintext. Callers must not log or persist the result.
    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for SecretString {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SecretString(<redacted>)")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auth_mode_default_is_required() {
        assert_eq!(AuthMode::default(), AuthMode::Required);
    }

    #[test]
    fn auth_mode_from_str_roundtrip() {
        assert_eq!("required".parse::<AuthMode>(), Ok(AuthMode::Required));
        assert_eq!("anonymous".parse::<AuthMode>(), Ok(AuthMode::Anonymous));
        assert_eq!("REQUIRED".parse::<AuthMode>(), Ok(AuthMode::Required));
        assert_eq!(" Anonymous ".parse::<AuthMode>(), Ok(AuthMode::Anonymous));
        assert!("open".parse::<AuthMode>().is_err());
        assert!("".parse::<AuthMode>().is_err());
    }

    #[test]
    fn auth_mode_display() {
        assert_eq!(AuthMode::Required.to_string(), "required");
        assert_eq!(AuthMode::Anonymous.to_string(), "anonymous");
    }

    #[test]
    fn parse_auth_mode_value_is_fail_closed() {
        // Unset → Required.
        assert_eq!(parse_auth_mode_value(None), AuthMode::Required);
        // Valid values pass through (case-insensitive, trimmed).
        assert_eq!(parse_auth_mode_value(Some("required")), AuthMode::Required);
        assert_eq!(
            parse_auth_mode_value(Some("anonymous")),
            AuthMode::Anonymous
        );
        assert_eq!(
            parse_auth_mode_value(Some(" ANONYMOUS ")),
            AuthMode::Anonymous
        );
        // Invalid values MUST fall back to Required, never Anonymous.
        assert_eq!(parse_auth_mode_value(Some("open")), AuthMode::Required);
        assert_eq!(
            parse_auth_mode_value(Some("anonymos")),
            AuthMode::Required,
            "a typo near 'anonymous' must still fail closed"
        );
        assert_eq!(parse_auth_mode_value(Some("")), AuthMode::Required);
    }

    #[test]
    fn secret_string_debug_is_redacted() {
        let secret = SecretString::new("aletheia_sk_super_secret_value");
        let debug = format!("{secret:?}");
        assert!(!debug.contains("super_secret_value"));
        assert!(debug.contains("redacted"));
        assert_eq!(secret.expose(), "aletheia_sk_super_secret_value");
    }
}
