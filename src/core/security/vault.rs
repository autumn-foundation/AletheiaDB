#![cfg(feature = "vault")]

use super::{KeyError, KeyProvider, MasterKey};
use async_trait::async_trait;
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use serde::Deserialize;
use vaultrs::client::{VaultClient, VaultClientSettingsBuilder};
use vaultrs::kv2;

/// Abstract interface for Vault operations.
#[async_trait]
pub trait VaultOperations: Send + Sync {
    /// Read the master key from the specified path.
    /// Returns the base64 encoded key string.
    async fn read_key(&self, mount: &str, path: &str) -> Result<String, String>;
}

#[derive(Deserialize)]
struct KeyResponse {
    key: String,
}

/// Real Vault implementation.
pub struct RealVaultClient {
    client: VaultClient,
}

impl RealVaultClient {
    /// Create a new RealVaultClient.
    pub fn new(addr: &str, token: &str) -> Result<Self, String> {
        let settings = VaultClientSettingsBuilder::default()
            .address(addr)
            .token(token)
            .build()
            .map_err(|e| e.to_string())?;

        let client = VaultClient::new(settings).map_err(|e| e.to_string())?;
        Ok(Self { client })
    }
}

#[async_trait]
impl VaultOperations for RealVaultClient {
    async fn read_key(&self, mount: &str, path: &str) -> Result<String, String> {
        // Assume key is stored in field "key"
        let secret: KeyResponse = kv2::read(&self.client, mount, path)
            .await
            .map_err(|e| e.to_string())?;

        Ok(secret.key)
    }
}

/// Key provider that retrieves the master key from HashiCorp Vault.
///
/// Expects the key to be stored in a KV2 secret with a field named "key",
/// containing the Base64 encoded 32-byte key.
///
/// The `secret_path` should be in the format `mount/path/to/secret`.
pub struct VaultProvider {
    ops: Box<dyn VaultOperations>,
    mount: String,
    path: String,
}

impl VaultProvider {
    /// Create a new VaultProvider.
    pub fn new(addr: &str, token: &str, secret_path: &str) -> Result<Self, KeyError> {
        let ops = RealVaultClient::new(addr, token).map_err(|e| KeyError::ConfigError(e))?;

        // Parse mount and path
        let parts: Vec<&str> = secret_path.splitn(2, '/').collect();
        if parts.len() != 2 {
            return Err(KeyError::ConfigError(
                "Invalid secret path. Expected format: mount/path".to_string(),
            ));
        }

        Ok(Self {
            ops: Box::new(ops),
            mount: parts[0].to_string(),
            path: parts[1].to_string(),
        })
    }

    /// Create with custom operations for testing.
    pub fn new_with_ops(
        ops: Box<dyn VaultOperations>,
        secret_path: &str,
    ) -> Result<Self, KeyError> {
        let parts: Vec<&str> = secret_path.splitn(2, '/').collect();
        if parts.len() != 2 {
            return Err(KeyError::ConfigError(
                "Invalid secret path. Expected format: mount/path".to_string(),
            ));
        }

        Ok(Self {
            ops,
            mount: parts[0].to_string(),
            path: parts[1].to_string(),
        })
    }
}

#[async_trait]
impl KeyProvider for VaultProvider {
    async fn get_master_key(&self) -> Result<MasterKey, KeyError> {
        let val = self
            .ops
            .read_key(&self.mount, &self.path)
            .await
            .map_err(|e| KeyError::ProviderError(e))?;

        let bytes = BASE64.decode(val.trim()).map_err(|e| {
            KeyError::ConfigError(format!("Invalid base64 encoding from Vault: {}", e))
        })?;

        if bytes.len() != 32 {
            return Err(KeyError::ConfigError(format!(
                "Invalid key length from Vault: expected 32 bytes, got {}",
                bytes.len()
            )));
        }

        let mut key_arr = [0u8; 32];
        key_arr.copy_from_slice(&bytes);

        Ok(MasterKey::new(key_arr, 1))
    }

    async fn get_key_version(&self, version: u32) -> Result<MasterKey, KeyError> {
        let key = self.get_master_key().await?;
        if key.version() == version {
            Ok(key)
        } else {
            Err(KeyError::NotFound)
        }
    }

    fn current_version(&self) -> u32 {
        1
    }

    fn name(&self) -> &str {
        "VaultProvider"
    }
}
