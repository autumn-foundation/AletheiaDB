//! HashiCorp Vault key provider (Issue #3587), gated behind `encryption-vault`.
//!
//! Reads the Master Encryption Key (MEK) from a Vault **KV v2** secret. Because
//! the [`KeyProvider`] trait is synchronous, this provider uses
//! `reqwest::blocking` -- no async runtime bridge is required.
//!
//! # Model
//!
//! `get_mek()` issues `GET {address}/v1/{mount}/data/{path}` with an
//! `X-Vault-Token` header, parses the JSON body, and reads
//! `data.data.<key_field>`. The value may be either 64 hex characters or a
//! base64-encoded 32-byte key.
//!
//! # Security
//!
//! The Vault token is a bearer secret: it is held in a zeroizing wrapper, is
//! redacted by `Debug`, and is **never** placed in any error `Display`/`Debug`
//! output.

use std::fmt;
use std::path::PathBuf;
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use zeroize::Zeroizing;

use crate::encryption::KeyProviderError;
use crate::encryption::key_provider::KeyProvider;

/// Default KV mount point.
pub const DEFAULT_MOUNT: &str = "secret";
/// Default field within the secret holding the key material.
pub const DEFAULT_KEY_FIELD: &str = "key";
/// The single logical key version this provider exposes.
const CURRENT_VERSION: u32 = 1;

/// A Vault bearer token that never appears in logs or `Debug` output.
struct SecretToken(Zeroizing<String>);

impl fmt::Debug for SecretToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("<redacted>")
    }
}

/// Configuration for the Vault key provider.
#[derive(Clone)]
pub struct VaultConfig {
    /// Base address, e.g. `https://vault.example.com:8200`.
    pub address: String,
    /// Vault token (bearer credential).
    pub token: String,
    /// KV v2 mount point (default `secret`).
    pub mount: String,
    /// Secret path under the mount.
    pub path: String,
    /// Field within the secret data holding the key (default `key`).
    pub key_field: String,
    /// Optional Vault namespace (Enterprise).
    pub namespace: Option<String>,
    /// Optional path to a PEM CA certificate for TLS verification.
    pub ca_cert: Option<PathBuf>,
}

impl fmt::Debug for VaultConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Redact the token; everything else is non-secret configuration.
        f.debug_struct("VaultConfig")
            .field("address", &self.address)
            .field("token", &"<redacted>")
            .field("mount", &self.mount)
            .field("path", &self.path)
            .field("key_field", &self.key_field)
            .field("namespace", &self.namespace)
            .field("ca_cert", &self.ca_cert)
            .finish()
    }
}

impl VaultConfig {
    /// Create a config with default mount/key_field and no namespace/CA.
    pub fn new(
        address: impl Into<String>,
        token: impl Into<String>,
        path: impl Into<String>,
    ) -> Self {
        Self {
            address: address.into(),
            token: token.into(),
            mount: DEFAULT_MOUNT.to_string(),
            path: path.into(),
            key_field: DEFAULT_KEY_FIELD.to_string(),
            namespace: None,
            ca_cert: None,
        }
    }
}

/// Reads the MEK from a Vault KV v2 secret over HTTPS/HTTP.
///
/// # Runtime context
///
/// This provider uses `reqwest::blocking` internally, so it must be
/// **constructed and called from a synchronous context** (not from within a
/// running Tokio runtime, which would panic on the nested blocking call).
/// Unlike the KMS provider it has no ambient-runtime bridge; sourcing the MEK at
/// synchronous database startup is the intended path.
pub struct VaultKeyProvider {
    client: reqwest::blocking::Client,
    address: String,
    token: SecretToken,
    mount: String,
    path: String,
    key_field: String,
    namespace: Option<String>,
}

impl fmt::Debug for VaultKeyProvider {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // NEVER expose the token.
        f.debug_struct("VaultKeyProvider")
            .field("address", &self.address)
            .field("token", &"<redacted>")
            .field("mount", &self.mount)
            .field("path", &self.path)
            .field("key_field", &self.key_field)
            .field("namespace", &self.namespace)
            .finish()
    }
}

impl VaultKeyProvider {
    /// Build a Vault key provider from config.
    ///
    /// # Errors
    ///
    /// Returns [`KeyProviderError::Unavailable`] if the HTTP client (including
    /// optional custom CA loading) cannot be constructed.
    pub fn new(config: &VaultConfig) -> Result<Self, KeyProviderError> {
        let mut builder = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(10))
            .connect_timeout(Duration::from_secs(5));

        if let Some(ca_path) = &config.ca_cert {
            let pem = std::fs::read(ca_path).map_err(|e| {
                KeyProviderError::Unavailable(format!("failed to read Vault CA cert: {e}"))
            })?;
            let cert = reqwest::Certificate::from_pem(&pem).map_err(|e| {
                KeyProviderError::Unavailable(format!("invalid Vault CA cert: {e}"))
            })?;
            builder = builder.add_root_certificate(cert);
        }

        let client = builder.build().map_err(|e| {
            KeyProviderError::Unavailable(format!("failed to build Vault HTTP client: {e}"))
        })?;

        let mount = if config.mount.is_empty() {
            DEFAULT_MOUNT.to_string()
        } else {
            config.mount.clone()
        };
        let key_field = if config.key_field.is_empty() {
            DEFAULT_KEY_FIELD.to_string()
        } else {
            config.key_field.clone()
        };

        Ok(Self {
            client,
            address: config.address.trim_end_matches('/').to_string(),
            token: SecretToken(Zeroizing::new(config.token.clone())),
            mount,
            path: config.path.clone(),
            key_field,
            namespace: config.namespace.clone(),
        })
    }

    fn secret_url(&self) -> String {
        format!("{}/v1/{}/data/{}", self.address, self.mount, self.path)
    }

    fn fetch(&self) -> Result<Zeroizing<[u8; 32]>, KeyProviderError> {
        let mut req = self
            .client
            .get(self.secret_url())
            .header("X-Vault-Token", self.token.0.as_str());
        if let Some(ns) = &self.namespace {
            req = req.header("X-Vault-Namespace", ns);
        }

        let resp = req.send().map_err(|e| {
            // Transport-level failure (connection refused, TLS, timeout). The
            // reqwest error may embed the URL but never the token header.
            if e.is_timeout() {
                KeyProviderError::Unavailable("Vault request timed out".to_string())
            } else {
                KeyProviderError::Unavailable("Vault transport failure".to_string())
            }
        })?;

        let status = resp.status();
        if status == reqwest::StatusCode::FORBIDDEN {
            return Err(KeyProviderError::AccessDenied(
                "Vault denied access (403)".to_string(),
            ));
        }
        if status == reqwest::StatusCode::NOT_FOUND {
            return Err(KeyProviderError::KeyNotFound);
        }
        if !status.is_success() {
            return Err(KeyProviderError::Unavailable(format!(
                "Vault returned HTTP {}",
                status.as_u16()
            )));
        }

        let body: serde_json::Value = resp.json().map_err(|_| {
            KeyProviderError::InvalidKeyFormat("Vault response was not valid JSON".to_string())
        })?;

        // KV v2 shape: { "data": { "data": { "<key_field>": "..." } } }
        let value = body
            .get("data")
            .and_then(|d| d.get("data"))
            .and_then(|d| d.get(&self.key_field))
            .and_then(|v| v.as_str())
            .ok_or_else(|| KeyProviderError::KeyNotFound)?;

        decode_key_material(value)
    }
}

/// Decode a Vault-stored key value: 64 hex chars, or base64 of 32 bytes.
fn decode_key_material(value: &str) -> Result<Zeroizing<[u8; 32]>, KeyProviderError> {
    let trimmed = value.trim();

    // Hex first (64 chars). Hold the decoded key material in a zeroizing buffer
    // so it is wiped on drop even before it is copied into the fixed array.
    if trimmed.len() == 64
        && let Some(decoded) = crate::core::hex::decode(trimmed).map(Zeroizing::new)
        && decoded.len() == 32
    {
        let mut key = Zeroizing::new([0u8; 32]);
        key.copy_from_slice(&decoded);
        return Ok(key);
    }

    // Base64 of exactly 32 bytes.
    if let Ok(decoded) = BASE64.decode(trimmed.as_bytes()).map(Zeroizing::new)
        && decoded.len() == 32
    {
        let mut key = Zeroizing::new([0u8; 32]);
        key.copy_from_slice(&decoded);
        return Ok(key);
    }

    Err(KeyProviderError::InvalidKeyFormat(
        "Vault key field is neither 64 hex chars nor base64 of 32 bytes".to_string(),
    ))
}

impl KeyProvider for VaultKeyProvider {
    fn get_mek(&self) -> Result<Zeroizing<[u8; 32]>, KeyProviderError> {
        self.fetch()
    }

    fn get_key_version(&self, version: u32) -> Result<Zeroizing<[u8; 32]>, KeyProviderError> {
        // v1 reads the current secret version only; explicit KV-v2 version
        // selection is a follow-up. Unknown versions are a typed KeyNotFound.
        if version == CURRENT_VERSION || version == 0 {
            self.get_mek()
        } else {
            Err(KeyProviderError::KeyNotFound)
        }
    }

    fn provider_name(&self) -> &str {
        "vault"
    }

    fn health_check(&self) -> Result<(), KeyProviderError> {
        self.get_mek().map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::atomic::{AtomicU64, Ordering};

    const TEST_TOKEN: &str = "s.SUPERSECRETVAULTTOKEN123";

    static PORT_HINT: AtomicU64 = AtomicU64::new(0);

    /// Spawn a one-shot HTTP server that returns `response` (raw HTTP/1.1) for a
    /// single request, then closes. Returns the bound `127.0.0.1:PORT` address.
    fn spawn_once(response: &'static str) -> String {
        let _ = PORT_HINT.fetch_add(1, Ordering::Relaxed);
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut buf = [0u8; 2048];
                let _ = stream.read(&mut buf);
                let _ = stream.write_all(response.as_bytes());
                let _ = stream.flush();
            }
        });
        format!("http://127.0.0.1:{}", addr.port())
    }

    fn hex_body(hex: &str) -> String {
        let json = format!("{{\"data\":{{\"data\":{{\"key\":\"{hex}\"}},\"metadata\":{{}}}}}}");
        format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            json.len(),
            json
        )
    }

    #[test]
    fn get_mek_recovers_hex_key() {
        // 64 hex chars = 32 bytes of 0xAB.
        let hex = "ab".repeat(32);
        let response: &'static str = Box::leak(hex_body(&hex).into_boxed_str());
        let addr = spawn_once(response);

        let cfg = VaultConfig::new(addr, TEST_TOKEN, "myapp/mek");
        let provider = VaultKeyProvider::new(&cfg).unwrap();
        let mek = provider.get_mek().unwrap();
        assert_eq!(mek.as_ref(), &[0xABu8; 32]);
        assert_eq!(provider.provider_name(), "vault");
    }

    #[test]
    fn get_mek_recovers_base64_key() {
        let raw = [0x11u8; 32];
        let b64 = BASE64.encode(raw);
        let json = format!("{{\"data\":{{\"data\":{{\"key\":\"{b64}\"}}}}}}");
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            json.len(),
            json
        );
        let response: &'static str = Box::leak(response.into_boxed_str());
        let addr = spawn_once(response);

        let cfg = VaultConfig::new(addr, TEST_TOKEN, "myapp/mek");
        let provider = VaultKeyProvider::new(&cfg).unwrap();
        let mek = provider.get_mek().unwrap();
        assert_eq!(mek.as_ref(), &raw);
    }

    #[test]
    fn forbidden_is_access_denied() {
        let response = "HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
        let addr = spawn_once(response);
        let cfg = VaultConfig::new(addr, TEST_TOKEN, "myapp/mek");
        let provider = VaultKeyProvider::new(&cfg).unwrap();
        let err = provider.get_mek().unwrap_err();
        assert!(
            matches!(err, KeyProviderError::AccessDenied(_)),
            "expected AccessDenied, got: {err}"
        );
        // Token must not leak in the error.
        assert!(!err.to_string().contains(TEST_TOKEN));
    }

    #[test]
    fn missing_field_is_typed_error() {
        let json = "{\"data\":{\"data\":{\"other\":\"value\"}}}";
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            json.len(),
            json
        );
        let response: &'static str = Box::leak(response.into_boxed_str());
        let addr = spawn_once(response);
        let cfg = VaultConfig::new(addr, TEST_TOKEN, "myapp/mek");
        let provider = VaultKeyProvider::new(&cfg).unwrap();
        let err = provider.get_mek().unwrap_err();
        assert!(
            matches!(err, KeyProviderError::KeyNotFound),
            "expected KeyNotFound for missing field, got: {err}"
        );
    }

    #[test]
    fn connection_refused_is_unavailable() {
        // Nothing listening on this port -> connection refused, fast.
        let cfg = VaultConfig::new("http://127.0.0.1:1", TEST_TOKEN, "myapp/mek");
        let provider = VaultKeyProvider::new(&cfg).unwrap();
        let err = provider.get_mek().unwrap_err();
        assert!(
            matches!(err, KeyProviderError::Unavailable(_)),
            "expected Unavailable, got: {err}"
        );
        assert!(!err.to_string().contains(TEST_TOKEN));
    }

    #[test]
    fn not_found_status_is_key_not_found() {
        let response = "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
        let addr = spawn_once(response);
        let cfg = VaultConfig::new(addr, TEST_TOKEN, "myapp/mek");
        let provider = VaultKeyProvider::new(&cfg).unwrap();
        let err = provider.get_mek().unwrap_err();
        assert!(matches!(err, KeyProviderError::KeyNotFound), "got: {err}");
    }

    #[test]
    fn debug_does_not_leak_token() {
        let cfg = VaultConfig::new("https://vault.example.com", TEST_TOKEN, "myapp/mek");
        let provider = VaultKeyProvider::new(&cfg).unwrap();
        let dbg = format!("{provider:?}");
        assert!(
            !dbg.contains(TEST_TOKEN),
            "provider Debug leaked token: {dbg}"
        );
        assert!(dbg.contains("redacted"));

        let cfg_dbg = format!("{cfg:?}");
        assert!(
            !cfg_dbg.contains(TEST_TOKEN),
            "config Debug leaked token: {cfg_dbg}"
        );
        assert!(cfg_dbg.contains("redacted"));
    }

    #[test]
    fn get_key_version_unknown_is_key_not_found() {
        let cfg = VaultConfig::new("http://127.0.0.1:1", TEST_TOKEN, "myapp/mek");
        let provider = VaultKeyProvider::new(&cfg).unwrap();
        let err = provider.get_key_version(7).unwrap_err();
        assert!(matches!(err, KeyProviderError::KeyNotFound), "got: {err}");
    }

    #[test]
    fn decode_key_material_variants() {
        // hex
        let hex = "cd".repeat(32);
        assert_eq!(decode_key_material(&hex).unwrap().as_ref(), &[0xCDu8; 32]);
        // base64
        let b64 = BASE64.encode([0x22u8; 32]);
        assert_eq!(decode_key_material(&b64).unwrap().as_ref(), &[0x22u8; 32]);
        // garbage
        assert!(matches!(
            decode_key_material("nope").unwrap_err(),
            KeyProviderError::InvalidKeyFormat(_)
        ));
    }
}
