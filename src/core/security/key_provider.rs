use async_trait::async_trait;
use thiserror::Error;
use zeroize::Zeroizing;

/// Errors that can occur during key management operations.
#[derive(Error, Debug)]
pub enum KeyError {
    /// The requested key was not found.
    #[error("Key not found")]
    NotFound,
    /// Failed to decrypt the key (e.g. invalid passphrase).
    #[error("Decryption failed")]
    DecryptionFailed,
    /// Configuration error (e.g. invalid path, missing env var).
    #[error("Invalid configuration: {0}")]
    ConfigError(String),
    /// Error from the underlying provider (e.g. AWS SDK, Vault).
    #[error("Provider error: {0}")]
    ProviderError(String),
}

/// Wrapped master key with secure memory handling.
/// The key data is zeroized when dropped.
pub struct MasterKey {
    key: Zeroizing<[u8; 32]>,
    version: u32,
}

impl MasterKey {
    /// Create a new MasterKey with the given key data and version.
    pub fn new(key: [u8; 32], version: u32) -> Self {
        Self {
            key: Zeroizing::new(key),
            version,
        }
    }

    /// Get the key version number.
    pub fn version(&self) -> u32 {
        self.version
    }

    /// Access the key bytes securely.
    /// Note: The returned reference is still protected by Zeroizing wrapper.
    pub fn as_bytes(&self) -> &[u8] {
        &self.key[..]
    }
}

/// Provider for encryption master keys
#[async_trait]
pub trait KeyProvider: Send + Sync {
    /// Get the current master encryption key
    async fn get_master_key(&self) -> Result<MasterKey, KeyError>;

    /// Get a specific key version (for decryption during rotation)
    async fn get_key_version(&self, version: u32) -> Result<MasterKey, KeyError>;

    /// Get current key version number
    fn current_version(&self) -> u32;

    /// Provider name for logging/metrics
    fn name(&self) -> &str;
}
