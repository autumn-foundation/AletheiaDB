use super::{KeyError, KeyProvider, MasterKey};
use async_trait::async_trait;
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use std::env;

/// Provider that loads the master key from a base64-encoded environment variable.
pub struct EnvKeyProvider {
    var_name: String,
    test_override: Option<String>,
}

impl EnvKeyProvider {
    /// Create a new EnvKeyProvider.
    pub fn new(var_name: impl Into<String>) -> Self {
        Self {
            var_name: var_name.into(),
            test_override: None,
        }
    }

    /// Create a new EnvKeyProvider with a direct value for testing (bypassing env vars).
    #[cfg(test)]
    pub fn new_test(val: impl Into<String>) -> Self {
        Self {
            var_name: "TEST".to_string(),
            test_override: Some(val.into()),
        }
    }
}

#[async_trait]
impl KeyProvider for EnvKeyProvider {
    async fn get_master_key(&self) -> Result<MasterKey, KeyError> {
        let val = if let Some(ref v) = self.test_override {
            v.clone()
        } else {
            env::var(&self.var_name).map_err(|_| KeyError::NotFound)?
        };

        let bytes = BASE64
            .decode(val.trim())
            .map_err(|e| KeyError::ConfigError(format!("Invalid base64 encoding: {}", e)))?;

        if bytes.len() != 32 {
            return Err(KeyError::ConfigError(format!(
                "Invalid key length: expected 32 bytes, got {}",
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
        "EnvKeyProvider"
    }
}
