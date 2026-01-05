//! HuggingFace Inference API embedding provider.
//!
//! Provides access to HuggingFace's hosted embedding models via their Inference API.
//! Supports any model available on HuggingFace that produces embeddings.
//!
//! # Popular Models
//!
//! - `sentence-transformers/all-MiniLM-L6-v2` (384 dimensions)
//! - `sentence-transformers/all-mpnet-base-v2` (768 dimensions)
//! - `BAAI/bge-small-en-v1.5` (384 dimensions)
//! - `BAAI/bge-large-en-v1.5` (1024 dimensions)
//!
//! # Example
//!
//! ```ignore
//! use gallifreydb::embeddings::{EmbeddingService, providers::huggingface::*};
//! use std::sync::Arc;
//!
//! let config = HuggingFaceConfig::from_env(
//!     "sentence-transformers/all-MiniLM-L6-v2".to_string(),
//!     384
//! )?;
//! let provider = Arc::new(HuggingFaceProvider::new(config)?);
//! let service = EmbeddingService::new(provider);
//!
//! let embedding = service.embed("Hello, world!").await?;
//! assert_eq!(embedding.len(), 384);
//! ```

use super::super::{EmbeddingError, EmbeddingProvider};
use async_trait::async_trait;
use serde::Serialize;

/// Configuration for HuggingFace Inference API.
#[derive(Clone)]
pub struct HuggingFaceConfig {
    /// API token (from HF_TOKEN environment variable or direct) - kept private for security
    api_token: String,
    /// Model ID (e.g., "sentence-transformers/all-MiniLM-L6-v2")
    pub model_id: String,
    /// Expected dimensions (must match model)
    pub dimensions: usize,
    /// API URL (default: https://api-inference.huggingface.co)
    pub base_url: Option<String>,
    /// Request timeout
    pub timeout_secs: u64,
}

impl std::fmt::Debug for HuggingFaceConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HuggingFaceConfig")
            .field("api_token", &"<redacted>")
            .field("model_id", &self.model_id)
            .field("dimensions", &self.dimensions)
            .field("base_url", &self.base_url)
            .field("timeout_secs", &self.timeout_secs)
            .finish()
    }
}

impl HuggingFaceConfig {
    /// Create config with API token from HF_TOKEN environment variable.
    ///
    /// # Arguments
    ///
    /// * `model_id` - HuggingFace model ID (e.g., "sentence-transformers/all-MiniLM-L6-v2")
    /// * `dimensions` - Expected embedding dimensions (must match the model)
    ///
    /// # Errors
    ///
    /// Returns `EmbeddingError::ConfigError` if HF_TOKEN is not set.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let config = HuggingFaceConfig::from_env(
    ///     "sentence-transformers/all-MiniLM-L6-v2".to_string(),
    ///     384
    /// )?;
    /// ```
    pub fn from_env(model_id: String, dimensions: usize) -> Result<Self, EmbeddingError> {
        let api_token = std::env::var("HF_TOKEN").map_err(|_| {
            EmbeddingError::ConfigError("HF_TOKEN environment variable not set".to_string())
        })?;

        Ok(Self {
            api_token,
            model_id,
            dimensions,
            base_url: None,
            timeout_secs: 60,
        })
    }

    /// Create config with explicit API token.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let config = HuggingFaceConfig::new(
    ///     "hf_...".to_string(),
    ///     "sentence-transformers/all-MiniLM-L6-v2".to_string(),
    ///     384
    /// );
    /// ```
    pub fn new(api_token: String, model_id: String, dimensions: usize) -> Self {
        Self {
            api_token,
            model_id,
            dimensions,
            base_url: None,
            timeout_secs: 60,
        }
    }

    /// Set a custom API base URL.
    ///
    /// Useful for self-hosted inference endpoints.
    pub fn with_base_url(mut self, base_url: String) -> Self {
        self.base_url = Some(base_url);
        self
    }

    /// Set a custom timeout in seconds.
    pub fn with_timeout(mut self, timeout_secs: u64) -> Self {
        self.timeout_secs = timeout_secs;
        self
    }

    /// Get the API token (internal use only).
    ///
    /// This is intentionally not public to prevent accidental exposure.
    pub(crate) fn api_token(&self) -> &str {
        &self.api_token
    }

    /// Create a config for the popular all-MiniLM-L6-v2 model (384 dimensions).
    ///
    /// # Example
    ///
    /// ```ignore
    /// let config = HuggingFaceConfig::all_minilm_l6_v2()?;
    /// ```
    pub fn all_minilm_l6_v2() -> Result<Self, EmbeddingError> {
        Self::from_env("sentence-transformers/all-MiniLM-L6-v2".to_string(), 384)
    }

    /// Create a config for the all-mpnet-base-v2 model (768 dimensions).
    ///
    /// # Example
    ///
    /// ```ignore
    /// let config = HuggingFaceConfig::all_mpnet_base_v2()?;
    /// ```
    pub fn all_mpnet_base_v2() -> Result<Self, EmbeddingError> {
        Self::from_env("sentence-transformers/all-mpnet-base-v2".to_string(), 768)
    }
}

/// HuggingFace Inference API embedding provider.
pub struct HuggingFaceProvider {
    config: HuggingFaceConfig,
    client: reqwest::Client,
}

impl HuggingFaceProvider {
    /// Create a new HuggingFace provider.
    ///
    /// # Errors
    ///
    /// Returns `EmbeddingError::ConfigError` if the HTTP client cannot be created.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let config = HuggingFaceConfig::from_env(
    ///     "sentence-transformers/all-MiniLM-L6-v2".to_string(),
    ///     384
    /// )?;
    /// let provider = HuggingFaceProvider::new(config)?;
    /// ```
    pub fn new(config: HuggingFaceConfig) -> Result<Self, EmbeddingError> {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(config.timeout_secs))
            .build()
            .map_err(|e| {
                EmbeddingError::ConfigError(format!("Failed to create HTTP client: {}", e))
            })?;

        Ok(Self { config, client })
    }
}

#[derive(Serialize)]
struct HFRequest<'a> {
    inputs: Vec<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    options: Option<HFOptions>,
}

#[derive(Serialize)]
struct HFOptions {
    wait_for_model: bool,
}

#[async_trait]
impl EmbeddingProvider for HuggingFaceProvider {
    fn dimensions(&self) -> usize {
        self.config.dimensions
    }

    fn name(&self) -> &str {
        &self.config.model_id
    }

    async fn embed(&self, text: &str) -> Result<Vec<f32>, EmbeddingError> {
        let results = self.embed_batch(&[text]).await?;
        results
            .into_iter()
            .next()
            .ok_or_else(|| EmbeddingError::ProviderError {
                provider: "HuggingFace".to_string(),
                message: "Empty response from API".to_string(),
                status_code: None,
            })
    }

    async fn embed_batch(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, EmbeddingError> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }

        let base_url = self
            .config
            .base_url
            .as_deref()
            .unwrap_or("https://api-inference.huggingface.co");
        let url = format!("{}/models/{}", base_url, self.config.model_id);

        let request = HFRequest {
            inputs: texts.to_vec(),
            options: Some(HFOptions {
                wait_for_model: true,
            }),
        };

        let response = self
            .client
            .post(&url)
            .header(
                "Authorization",
                format!("Bearer {}", self.config.api_token()),
            )
            .header("Content-Type", "application/json")
            .json(&request)
            .send()
            .await
            .map_err(|e| EmbeddingError::NetworkError(e.to_string()))?;

        let status = response.status();

        // Handle authentication errors
        if status == reqwest::StatusCode::UNAUTHORIZED {
            return Err(EmbeddingError::AuthenticationFailed {
                provider: "HuggingFace".to_string(),
                reason: "Invalid API token".to_string(),
            });
        }

        // Handle rate limiting
        if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
            return Err(EmbeddingError::RateLimitExceeded {
                provider: "HuggingFace".to_string(),
                retry_after: None,
            });
        }

        // Handle model not found
        if status == reqwest::StatusCode::NOT_FOUND {
            return Err(EmbeddingError::ModelNotFound {
                model: self.config.model_id.clone(),
                provider: "HuggingFace".to_string(),
            });
        }

        // Handle other errors
        if !status.is_success() {
            let error_text = response
                .text()
                .await
                .unwrap_or_else(|_| "Unknown error".to_string());
            return Err(EmbeddingError::ProviderError {
                provider: "HuggingFace".to_string(),
                message: error_text,
                status_code: Some(status.as_u16()),
            });
        }

        // Parse response - HuggingFace returns a flat array of embeddings
        let embeddings: Vec<Vec<f32>> =
            response
                .json()
                .await
                .map_err(|e| EmbeddingError::ProviderError {
                    provider: "HuggingFace".to_string(),
                    message: format!("Failed to parse response: {}", e),
                    status_code: None,
                })?;

        // Validate dimensions
        let expected_dims = self.dimensions();
        for emb in &embeddings {
            if emb.len() != expected_dims {
                return Err(EmbeddingError::DimensionMismatch {
                    expected: expected_dims,
                    actual: emb.len(),
                });
            }
        }

        Ok(embeddings)
    }

    fn normalized_by_default(&self) -> bool {
        // sentence-transformers models are typically normalized
        self.config.model_id.contains("sentence-transformers")
    }

    fn max_text_length(&self) -> Option<usize> {
        // Most models have a token limit of 512 tokens
        // Approximate as 4 characters per token
        Some(512 * 4)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    lazy_static::lazy_static! {
        static ref ENV_MUTEX: Mutex<()> = Mutex::new(());
    }

    #[test]
    fn test_config_from_missing_env() {
        let _guard = ENV_MUTEX.lock().unwrap(); // Lock before manipulating env
        let original_var = std::env::var("HF_TOKEN").ok();

        unsafe {
            std::env::remove_var("HF_TOKEN");
        }

        let result = HuggingFaceConfig::from_env("test-model".to_string(), 384);
        assert!(result.is_err());
        match result {
            Err(EmbeddingError::ConfigError(msg)) => {
                assert!(msg.contains("HF_TOKEN"));
            }
            _ => panic!("Expected ConfigError"),
        }

        // Restore original value
        if let Some(val) = original_var {
            unsafe {
                std::env::set_var("HF_TOKEN", val);
            }
        }
    } // Mutex is unlocked when _guard goes out of scope

    #[test]
    fn test_config_builder() {
        let config =
            HuggingFaceConfig::new("test-token".to_string(), "test-model".to_string(), 384)
                .with_base_url("https://custom.api".to_string())
                .with_timeout(120);

        // API token is private for security - test other fields
        assert_eq!(config.model_id, "test-model");
        assert_eq!(config.dimensions, 384);
        assert_eq!(config.base_url, Some("https://custom.api".to_string()));
        assert_eq!(config.timeout_secs, 120);
    }

    #[test]
    fn test_config_debug_redacts_api_token() {
        let config = HuggingFaceConfig::new(
            "hf_secret_token_12345".to_string(),
            "test-model".to_string(),
            384,
        );

        let debug_output = format!("{:?}", config);

        // API token should be redacted in debug output
        assert!(debug_output.contains("<redacted>"));
        assert!(!debug_output.contains("hf_secret_token_12345"));
        // Other fields should still be visible
        assert!(debug_output.contains("test-model"));
    }

    #[test]
    fn test_api_token_getter() {
        let config =
            HuggingFaceConfig::new("test-token".to_string(), "test-model".to_string(), 384);

        // Internal getter should return the token
        assert_eq!(config.api_token(), "test-token");
    }

    #[test]
    fn test_provider_creation() {
        let config = HuggingFaceConfig::new(
            "test-token".to_string(),
            "sentence-transformers/all-MiniLM-L6-v2".to_string(),
            384,
        );
        let provider = HuggingFaceProvider::new(config);
        assert!(provider.is_ok());

        let provider = provider.unwrap();
        assert_eq!(provider.dimensions(), 384);
        assert_eq!(provider.name(), "sentence-transformers/all-MiniLM-L6-v2");
        assert!(provider.normalized_by_default()); // sentence-transformers should be normalized
        assert!(provider.max_text_length().is_some());
    }

    #[test]
    fn test_provider_non_sentence_transformers() {
        let config =
            HuggingFaceConfig::new("test-token".to_string(), "custom-model".to_string(), 512);
        let provider = HuggingFaceProvider::new(config).unwrap();

        assert!(!provider.normalized_by_default()); // Non sentence-transformers
    }
}
