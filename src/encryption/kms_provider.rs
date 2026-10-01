//! AWS KMS key provider (Issue #3587), gated behind `encryption-aws-kms`.
//!
//! # Envelope model
//!
//! The Master Encryption Key (MEK) is never stored in plaintext. Instead the
//! configuration carries:
//! - a KMS key id / ARN, and
//! - a **base64-encoded KMS-encrypted data-key ciphertext blob** (the wrapped
//!   MEK).
//!
//! At startup [`get_mek`](KmsKeyProvider::get_mek) calls KMS `Decrypt` on the
//! blob and returns the recovered 32-byte plaintext as a
//! [`Zeroizing<[u8; 32]>`]. Nothing sensitive is logged or exposed via `Debug`.
//!
//! # Async bridge
//!
//! `aws-sdk-kms` is async but the [`KeyProvider`] trait is synchronous, so this
//! provider owns an internal current-thread Tokio runtime and blocks on the
//! `Decrypt` future. If a call happens to occur *inside* an ambient Tokio
//! runtime (which would make a nested `block_on` panic), the future is run on a
//! dedicated OS thread instead, detected via
//! [`Handle::try_current`](tokio::runtime::Handle::try_current).
//!
//! # Credentials
//!
//! Credentials are read from the standard `AWS_ACCESS_KEY_ID` /
//! `AWS_SECRET_ACCESS_KEY` / `AWS_SESSION_TOKEN` environment variables. A full
//! default credential provider chain (instance metadata, SSO, config files) via
//! `aws-config` is a deliberate follow-up; the envelope-decrypt flow is
//! identical either way.

use std::fmt;

use aws_sdk_kms::Client;
use aws_sdk_kms::config::{BehaviorVersion, Credentials, Region};
use aws_sdk_kms::error::{ProvideErrorMetadata, SdkError};
use aws_sdk_kms::operation::decrypt::DecryptError;
use aws_sdk_kms::primitives::Blob;
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use tokio::runtime::{Handle, Runtime};
use zeroize::Zeroizing;

use crate::encryption::KeyProviderError;
use crate::encryption::key_provider::KeyProvider;

/// The single logical key version this envelope model exposes.
const CURRENT_VERSION: u32 = 1;

/// Configuration for the AWS KMS key provider.
#[derive(Clone)]
pub struct KmsConfig {
    /// KMS key id or ARN used to decrypt the data key.
    pub key_id: String,
    /// Base64-encoded KMS-encrypted data-key ciphertext blob (the wrapped MEK).
    pub encrypted_data_key: String,
    /// AWS region (defaults to `us-east-1` if unset).
    pub region: Option<String>,
    /// Optional custom endpoint URL (e.g. LocalStack or a VPC endpoint).
    pub endpoint_url: Option<String>,
}

impl fmt::Debug for KmsConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Redact the wrapped-key blob; key_id/region/endpoint are not secret but
        // the ciphertext blob is treated as sensitive material.
        f.debug_struct("KmsConfig")
            .field("key_id", &self.key_id)
            .field("encrypted_data_key", &"<redacted>")
            .field("region", &self.region)
            .field("endpoint_url", &self.endpoint_url)
            .finish()
    }
}

/// Decrypts a KMS-wrapped data key into the MEK on demand.
pub struct KmsKeyProvider {
    client: Client,
    key_id: String,
    /// Raw (base64-decoded) ciphertext blob.
    blob: Vec<u8>,
    runtime: Runtime,
}

impl fmt::Debug for KmsKeyProvider {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // NEVER expose the wrapped-key blob.
        f.debug_struct("KmsKeyProvider")
            .field("key_id", &self.key_id)
            .field("blob", &"<redacted>")
            .finish()
    }
}

impl KmsKeyProvider {
    /// Build a KMS key provider from config.
    ///
    /// # Errors
    ///
    /// Returns [`KeyProviderError::InvalidKeyFormat`] if the ciphertext blob is
    /// not valid base64, or [`KeyProviderError::Unavailable`] if the internal
    /// runtime cannot be constructed.
    pub fn new(config: &KmsConfig) -> Result<Self, KeyProviderError> {
        let blob = BASE64
            .decode(config.encrypted_data_key.as_bytes())
            .map_err(|_| {
                KeyProviderError::InvalidKeyFormat(
                    "encrypted_data_key is not valid base64".to_string(),
                )
            })?;

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| {
                KeyProviderError::Unavailable(format!("failed to build KMS runtime: {e}"))
            })?;

        // Credentials from the standard AWS_* environment variables. Missing
        // credentials simply yield an auth failure at Decrypt time (typed
        // AccessDenied), never a panic.
        let access = std::env::var("AWS_ACCESS_KEY_ID").unwrap_or_default();
        let secret = std::env::var("AWS_SECRET_ACCESS_KEY").unwrap_or_default();
        let session = std::env::var("AWS_SESSION_TOKEN").ok();
        let creds = Credentials::new(access, secret, session, None, "aletheia-env");

        let region = config
            .region
            .clone()
            .or_else(|| std::env::var("AWS_REGION").ok())
            .unwrap_or_else(|| "us-east-1".to_string());

        let mut builder = aws_sdk_kms::config::Builder::default()
            .behavior_version(BehaviorVersion::latest())
            .region(Region::new(region))
            .credentials_provider(creds)
            // Fail fast rather than retrying against an unreachable endpoint.
            .retry_config(aws_sdk_kms::config::retry::RetryConfig::disabled())
            .timeout_config(
                aws_sdk_kms::config::timeout::TimeoutConfig::builder()
                    .operation_timeout(std::time::Duration::from_secs(10))
                    .operation_attempt_timeout(std::time::Duration::from_secs(5))
                    .build(),
            );
        if let Some(ep) = &config.endpoint_url {
            builder = builder.endpoint_url(ep.clone());
        }
        let client = Client::from_conf(builder.build());

        Ok(Self {
            client,
            key_id: config.key_id.clone(),
            blob,
            runtime,
        })
    }

    /// Test-only constructor injecting an already-built KMS `Client` (e.g. one
    /// backed by a mocked transport) plus the raw ciphertext blob, bypassing the
    /// real AWS credential/endpoint config. Not part of the public API; lets the
    /// Decrypt success path be exercised without a live KMS.
    #[cfg(test)]
    fn with_client_for_test(client: Client, blob: Vec<u8>) -> Self {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("test runtime");
        Self {
            client,
            key_id: "alias/test".to_string(),
            blob,
            runtime,
        }
    }

    /// Run the (owned, `'static`) decrypt future to completion, bridging the
    /// async client into the synchronous trait without panicking inside an
    /// ambient runtime.
    fn block_on_decrypt(&self) -> Result<Zeroizing<[u8; 32]>, KeyProviderError> {
        let fut = decrypt(self.client.clone(), self.key_id.clone(), self.blob.clone());

        if Handle::try_current().is_ok() {
            // We are inside another Tokio runtime; a nested block_on would
            // panic. Run on a dedicated OS thread with our runtime's handle.
            let handle = self.runtime.handle().clone();
            let jh = std::thread::spawn(move || handle.block_on(fut));
            jh.join().unwrap_or_else(|_| {
                Err(KeyProviderError::Unavailable(
                    "KMS decrypt worker thread panicked".to_string(),
                ))
            })
        } else {
            self.runtime.block_on(fut)
        }
    }
}

/// The owned, `'static` decrypt future body.
async fn decrypt(
    client: Client,
    key_id: String,
    blob: Vec<u8>,
) -> Result<Zeroizing<[u8; 32]>, KeyProviderError> {
    let out = client
        .decrypt()
        .key_id(key_id)
        .ciphertext_blob(Blob::new(blob))
        .send()
        .await
        .map_err(map_decrypt_err)?;

    let plaintext = out.plaintext().ok_or_else(|| {
        KeyProviderError::InvalidKeyFormat("KMS returned no plaintext".to_string())
    })?;
    plaintext_to_key(plaintext.as_ref())
}

/// Convert a decrypted plaintext byte slice into a 32-byte MEK.
fn plaintext_to_key(bytes: &[u8]) -> Result<Zeroizing<[u8; 32]>, KeyProviderError> {
    if bytes.len() != 32 {
        return Err(KeyProviderError::InvalidKeyFormat(format!(
            "KMS plaintext data key is {} bytes, expected 32",
            bytes.len()
        )));
    }
    let mut key = Zeroizing::new([0u8; 32]);
    key.copy_from_slice(bytes);
    Ok(key)
}

/// Map a KMS `Decrypt` SDK error into a typed [`KeyProviderError`] without
/// leaking any request/response body.
fn map_decrypt_err<R>(e: SdkError<DecryptError, R>) -> KeyProviderError {
    match &e {
        SdkError::TimeoutError(_) => {
            KeyProviderError::Unavailable("KMS request timed out".to_string())
        }
        SdkError::DispatchFailure(_) => {
            KeyProviderError::Unavailable("KMS transport/dispatch failure".to_string())
        }
        SdkError::ResponseError(_) => {
            KeyProviderError::Unavailable("KMS returned an unparseable response".to_string())
        }
        SdkError::ServiceError(se) => {
            let code = se.err().code().unwrap_or("Unknown");
            if code.contains("AccessDenied")
                || code.contains("NotAuthorized")
                || code.contains("Unauthorized")
                || code.contains("Disabled")
                || code.contains("InvalidState")
            {
                KeyProviderError::AccessDenied(format!("KMS denied decrypt ({code})"))
            } else if code.contains("Throttling")
                || code.contains("Internal")
                || code.contains("Unavailable")
                || code.contains("DependencyTimeout")
            {
                KeyProviderError::Unavailable(format!("KMS transient error ({code})"))
            } else {
                KeyProviderError::Provider(Box::new(std::io::Error::other(format!(
                    "KMS decrypt error ({code})"
                ))))
            }
        }
        _ => KeyProviderError::Provider(Box::new(std::io::Error::other(
            "KMS decrypt failed".to_string(),
        ))),
    }
}

impl KeyProvider for KmsKeyProvider {
    fn get_mek(&self) -> Result<Zeroizing<[u8; 32]>, KeyProviderError> {
        self.block_on_decrypt()
    }

    fn get_key_version(&self, version: u32) -> Result<Zeroizing<[u8; 32]>, KeyProviderError> {
        // The envelope model wraps a single data key. Only the current version
        // is retrievable; any other version is a typed KeyNotFound (never a
        // panic). Native KMS key-rotation history is a follow-up.
        if version == CURRENT_VERSION || version == 0 {
            self.get_mek()
        } else {
            Err(KeyProviderError::KeyNotFound)
        }
    }

    fn provider_name(&self) -> &str {
        "kms"
    }

    fn health_check(&self) -> Result<(), KeyProviderError> {
        self.get_mek().map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> KmsConfig {
        KmsConfig {
            key_id: "alias/test".to_string(),
            // base64 of arbitrary bytes -- decodes fine; decrypt would need a
            // live KMS, which these tests deliberately avoid.
            encrypted_data_key: BASE64.encode(b"wrapped-data-key-ciphertext"),
            region: Some("us-east-1".to_string()),
            endpoint_url: None,
        }
    }

    #[test]
    fn plaintext_to_key_accepts_32_bytes() {
        let bytes = [7u8; 32];
        let key = plaintext_to_key(&bytes).unwrap();
        assert_eq!(key.as_ref(), &bytes);
    }

    #[test]
    fn plaintext_to_key_rejects_non_32() {
        for len in [0usize, 16, 31, 33, 64] {
            let bytes = vec![0u8; len];
            let err = plaintext_to_key(&bytes).unwrap_err();
            assert!(
                matches!(err, KeyProviderError::InvalidKeyFormat(_)),
                "len {len} should be InvalidKeyFormat, got: {err}"
            );
        }
    }

    #[test]
    fn new_rejects_invalid_base64_blob() {
        let cfg = KmsConfig {
            encrypted_data_key: "not!valid!base64!!!".to_string(),
            ..test_config()
        };
        let err = KmsKeyProvider::new(&cfg).unwrap_err();
        assert!(
            matches!(err, KeyProviderError::InvalidKeyFormat(_)),
            "expected InvalidKeyFormat, got: {err}"
        );
    }

    #[test]
    fn new_succeeds_with_valid_config() {
        let provider = KmsKeyProvider::new(&test_config()).unwrap();
        assert_eq!(provider.provider_name(), "kms");
    }

    #[test]
    fn unreachable_endpoint_returns_typed_error_not_panic() {
        // Point at a closed local port so the request fails fast with a
        // transport error. This exercises the async bridge + error mapping
        // without a live KMS (see the KMS coverage-boundary note in the PR).
        let cfg = KmsConfig {
            endpoint_url: Some("http://127.0.0.1:1".to_string()),
            ..test_config()
        };
        let provider = KmsKeyProvider::new(&cfg).unwrap();
        let err = provider.get_mek().unwrap_err();
        // Transport failure or a denied/typed error -- crucially, NOT a panic
        // and NOT a hang.
        assert!(
            matches!(
                err,
                KeyProviderError::Unavailable(_)
                    | KeyProviderError::AccessDenied(_)
                    | KeyProviderError::Provider(_)
            ),
            "expected a typed transport/denied error, got: {err}"
        );
    }

    #[test]
    fn get_key_version_unknown_is_key_not_found() {
        let provider = KmsKeyProvider::new(&test_config()).unwrap();
        let err = provider.get_key_version(999).unwrap_err();
        assert!(
            matches!(err, KeyProviderError::KeyNotFound),
            "unknown version must be KeyNotFound, got: {err}"
        );
    }

    #[test]
    fn mocked_decrypt_returns_exact_plaintext() {
        // Replay a canned KMS Decrypt success whose plaintext is a fixed, known
        // 32 bytes and assert get_mek() returns exactly those bytes — the
        // success path the unreachable-endpoint test cannot cover.
        use aws_sdk_kms::operation::decrypt::DecryptOutput;
        use aws_smithy_mocks::{mock, mock_client};

        let expected = [0x5Au8; 32];
        let rule = mock!(aws_sdk_kms::Client::decrypt).then_output(move || {
            DecryptOutput::builder()
                .plaintext(Blob::new(expected.to_vec()))
                .build()
        });
        let client = mock_client!(aws_sdk_kms, &[&rule]);

        let provider = KmsKeyProvider::with_client_for_test(client, b"wrapped-blob".to_vec());
        let mek = provider.get_mek().expect("mocked decrypt should succeed");
        assert_eq!(mek.as_ref(), &expected);
    }

    #[test]
    fn mocked_decrypt_non_32_byte_plaintext_is_invalid_format() {
        // A mocked Decrypt returning a non-32-byte plaintext must map to a typed
        // InvalidKeyFormat, never a panic.
        use aws_sdk_kms::operation::decrypt::DecryptOutput;
        use aws_smithy_mocks::{mock, mock_client};

        let rule = mock!(aws_sdk_kms::Client::decrypt).then_output(|| {
            DecryptOutput::builder()
                .plaintext(Blob::new(vec![1u8; 16]))
                .build()
        });
        let client = mock_client!(aws_sdk_kms, &[&rule]);

        let provider = KmsKeyProvider::with_client_for_test(client, b"wrapped-blob".to_vec());
        let err = provider.get_mek().unwrap_err();
        assert!(
            matches!(err, KeyProviderError::InvalidKeyFormat(_)),
            "non-32-byte plaintext must be InvalidKeyFormat, got: {err}"
        );
    }

    #[test]
    fn debug_does_not_leak_blob() {
        let provider = KmsKeyProvider::new(&test_config()).unwrap();
        let dbg = format!("{provider:?}");
        assert!(dbg.contains("redacted"));
        assert!(!dbg.contains("wrapped-data-key"));

        let cfg_dbg = format!("{:?}", test_config());
        assert!(cfg_dbg.contains("redacted"));
        // The base64 blob string must not appear.
        assert!(!cfg_dbg.contains(&BASE64.encode(b"wrapped-data-key-ciphertext")));
    }
}
