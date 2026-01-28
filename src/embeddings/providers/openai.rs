//! OpenAI embedding provider.
//!
//! Supports OpenAI's embedding models:
//! - `text-embedding-3-small` (1536 dimensions, $0.02/1M tokens)
//! - `text-embedding-3-large` (3072 dimensions, $0.13/1M tokens)
//! - `text-embedding-ada-002` (1536 dimensions, legacy)
//!
//! # Example
//!
//! ```ignore
//! use gallifreydb::embeddings::{EmbeddingService, providers::openai::*};
//! use std::sync::Arc;
//!
//! let config = OpenAIConfig::from_env(OpenAIModel::TextEmbedding3Small)?;
//! let provider = Arc::new(OpenAIProvider::new(config)?);
//! let service = EmbeddingService::new(provider);
//!
//! let embedding = service.embed("Hello, world!").await?;
//! assert_eq!(embedding.len(), 1536);
//! ```

use super::super::{EmbeddingError, EmbeddingProvider};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// OpenAI embedding models.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenAIModel {
    /// text-embedding-3-small (1536 dimensions, $0.02/1M tokens)
    ///
    /// Best for: most use cases with good performance/cost ratio
    TextEmbedding3Small,

    /// text-embedding-3-large (3072 dimensions, $0.13/1M tokens)
    ///
    /// Best for: highest quality embeddings, when cost is not a concern
    TextEmbedding3Large,

    /// text-embedding-ada-002 (1536 dimensions, legacy)
    ///
    /// Legacy model, prefer TextEmbedding3Small for new projects
    Ada002,
}

impl OpenAIModel {
    /// Get the model name for API requests.
    pub fn model_name(&self) -> &'static str {
        match self {
            OpenAIModel::TextEmbedding3Small => "text-embedding-3-small",
            OpenAIModel::TextEmbedding3Large => "text-embedding-3-large",
            OpenAIModel::Ada002 => "text-embedding-ada-002",
        }
    }

    /// Get the number of dimensions for this model.
    pub fn dimensions(&self) -> usize {
        match self {
            OpenAIModel::TextEmbedding3Small => 1536,
            OpenAIModel::TextEmbedding3Large => 3072,
            OpenAIModel::Ada002 => 1536,
        }
    }
}

/// Configuration for OpenAI provider.
#[derive(Clone)]
pub struct OpenAIConfig {
    /// API key (from environment or direct) - kept private for security
    api_key: String,
    /// Model to use
    pub model: OpenAIModel,
    /// API base URL (default: https://api.openai.com/v1)
    pub base_url: Option<String>,
    /// Request timeout in seconds
    pub timeout_secs: u64,
}

impl std::fmt::Debug for OpenAIConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpenAIConfig")
            .field("api_key", &"<redacted>")
            .field("model", &self.model)
            .field("base_url", &self.base_url)
            .field("timeout_secs", &self.timeout_secs)
            .finish()
    }
}

impl OpenAIConfig {
    /// Create config with API key from OPENAI_API_KEY environment variable.
    ///
    /// # Errors
    ///
    /// Returns `EmbeddingError::ConfigError` if the environment variable is not set.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let config = OpenAIConfig::from_env(OpenAIModel::TextEmbedding3Small)?;
    /// ```
    pub fn from_env(model: OpenAIModel) -> Result<Self, EmbeddingError> {
        let api_key = std::env::var("OPENAI_API_KEY").map_err(|_| {
            EmbeddingError::ConfigError("OPENAI_API_KEY environment variable not set".to_string())
        })?;

        Ok(Self {
            api_key,
            model,
            base_url: None,
            timeout_secs: 30,
        })
    }

    /// Create config with explicit API key.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let config = OpenAIConfig::new(
    ///     "sk-...".to_string(),
    ///     OpenAIModel::TextEmbedding3Small
    /// );
    /// ```
    pub fn new(api_key: String, model: OpenAIModel) -> Self {
        Self {
            api_key,
            model,
            base_url: None,
            timeout_secs: 30,
        }
    }

    /// Set a custom API base URL.
    ///
    /// Useful for Azure OpenAI or other OpenAI-compatible endpoints.
    pub fn with_base_url(mut self, base_url: String) -> Self {
        self.base_url = Some(base_url);
        self
    }

    /// Set a custom timeout in seconds.
    pub fn with_timeout(mut self, timeout_secs: u64) -> Self {
        self.timeout_secs = timeout_secs;
        self
    }

    /// Get the API key (internal use only).
    ///
    /// This is intentionally not public to prevent accidental exposure.
    pub(crate) fn api_key(&self) -> &str {
        &self.api_key
    }
}

/// OpenAI embedding provider.
pub struct OpenAIProvider {
    config: OpenAIConfig,
    client: reqwest::Client,
}

impl OpenAIProvider {
    /// Create a new OpenAI provider.
    ///
    /// # Errors
    ///
    /// Returns `EmbeddingError::ConfigError` if the HTTP client cannot be created.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let config = OpenAIConfig::from_env(OpenAIModel::TextEmbedding3Small)?;
    /// let provider = OpenAIProvider::new(config)?;
    /// ```
    pub fn new(config: OpenAIConfig) -> Result<Self, EmbeddingError> {
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
struct EmbeddingRequest<'a> {
    input: Vec<&'a str>,
    model: &'a str,
}

#[derive(Deserialize)]
struct EmbeddingResponse {
    data: Vec<EmbeddingData>,
}

#[derive(Deserialize)]
struct EmbeddingData {
    embedding: Vec<f32>,
    index: usize,
}

#[async_trait]
impl EmbeddingProvider for OpenAIProvider {
    fn dimensions(&self) -> usize {
        self.config.model.dimensions()
    }

    fn name(&self) -> &str {
        self.config.model.model_name()
    }

    async fn embed(&self, text: &str) -> Result<Vec<f32>, EmbeddingError> {
        let results = self.embed_batch(&[text]).await?;
        results
            .into_iter()
            .next()
            .ok_or_else(|| EmbeddingError::ProviderError {
                provider: "OpenAI".to_string(),
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
            .unwrap_or("https://api.openai.com/v1");
        let url = format!("{}/embeddings", base_url);

        let request = EmbeddingRequest {
            input: texts.to_vec(),
            model: self.config.model.model_name(),
        };

        let response = self
            .client
            .post(&url)
            .header("Authorization", format!("Bearer {}", self.config.api_key()))
            .header("Content-Type", "application/json")
            .json(&request)
            .send()
            .await
            .map_err(|e| EmbeddingError::NetworkError(e.to_string()))?;

        let status = response.status();

        // Handle authentication errors
        if status == reqwest::StatusCode::UNAUTHORIZED {
            return Err(EmbeddingError::AuthenticationFailed {
                provider: "OpenAI".to_string(),
                reason: "Invalid API key".to_string(),
            });
        }

        // Handle rate limiting
        if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
            let retry_after = response
                .headers()
                .get("retry-after")
                .and_then(|v| v.to_str().ok())
                .and_then(|s| s.parse::<u64>().ok())
                .map(std::time::Duration::from_secs);

            return Err(EmbeddingError::RateLimitExceeded {
                provider: "OpenAI".to_string(),
                retry_after,
            });
        }

        // Handle other errors
        if !status.is_success() {
            let error_text = response
                .text()
                .await
                .unwrap_or_else(|_| "Unknown error".to_string());
            return Err(EmbeddingError::ProviderError {
                provider: "OpenAI".to_string(),
                message: error_text,
                status_code: Some(status.as_u16()),
            });
        }

        // Parse response
        let embedding_response: EmbeddingResponse =
            response
                .json()
                .await
                .map_err(|e| EmbeddingError::ProviderError {
                    provider: "OpenAI".to_string(),
                    message: format!("Failed to parse response: {}", e),
                    status_code: None,
                })?;

        // Sort by index to maintain input order
        let mut embeddings = embedding_response.data;
        embeddings.sort_by_key(|d| d.index);

        // Validate dimensions
        let expected_dims = self.dimensions();
        for emb in &embeddings {
            if emb.embedding.len() != expected_dims {
                return Err(EmbeddingError::DimensionMismatch {
                    expected: expected_dims,
                    actual: emb.embedding.len(),
                });
            }
        }

        Ok(embeddings.into_iter().map(|d| d.embedding).collect())
    }

    fn normalized_by_default(&self) -> bool {
        true // OpenAI embeddings are normalized
    }

    fn max_text_length(&self) -> Option<usize> {
        // OpenAI's token limit is 8191 tokens
        // APPROXIMATION: We estimate 4 characters per token for safety margin.
        // This is a rough estimate and may be inaccurate for:
        // - Non-English text (different tokenization)
        // - Code or technical content (more tokens per character)
        // - Languages with different character sets
        // For accurate token counting, consider using tiktoken-rs or similar
        Some(8191 * 4)
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
    fn test_model_dimensions() {
        assert_eq!(OpenAIModel::TextEmbedding3Small.dimensions(), 1536);
        assert_eq!(OpenAIModel::TextEmbedding3Large.dimensions(), 3072);
        assert_eq!(OpenAIModel::Ada002.dimensions(), 1536);
    }

    #[test]
    fn test_model_names() {
        assert_eq!(
            OpenAIModel::TextEmbedding3Small.model_name(),
            "text-embedding-3-small"
        );
        assert_eq!(
            OpenAIModel::TextEmbedding3Large.model_name(),
            "text-embedding-3-large"
        );
        assert_eq!(OpenAIModel::Ada002.model_name(), "text-embedding-ada-002");
    }

    #[test]
    fn test_config_from_missing_env() {
        let _guard = ENV_MUTEX.lock().unwrap(); // Lock before manipulating env
        let original_var = std::env::var("OPENAI_API_KEY").ok();

        unsafe {
            std::env::remove_var("OPENAI_API_KEY");
        }

        let result = OpenAIConfig::from_env(OpenAIModel::Ada002);
        assert!(result.is_err());
        match result {
            Err(EmbeddingError::ConfigError(msg)) => {
                assert!(msg.contains("OPENAI_API_KEY"));
            }
            _ => panic!("Expected ConfigError"),
        }

        // Restore original value
        if let Some(val) = original_var {
            unsafe {
                std::env::set_var("OPENAI_API_KEY", val);
            }
        }
    } // Mutex is unlocked when _guard goes out of scope

    #[test]
    fn test_config_builder() {
        let config = OpenAIConfig::new("test-key".to_string(), OpenAIModel::TextEmbedding3Small)
            .with_base_url("https://custom.api".to_string())
            .with_timeout(60);

        // API key is private for security - test other fields
        assert_eq!(config.model, OpenAIModel::TextEmbedding3Small);
        assert_eq!(config.base_url, Some("https://custom.api".to_string()));
        assert_eq!(config.timeout_secs, 60);
    }

    #[test]
    fn test_config_debug_redacts_api_key() {
        let config = OpenAIConfig::new(
            "sk-secret-key-12345".to_string(),
            OpenAIModel::TextEmbedding3Small,
        );

        let debug_output = format!("{:?}", config);

        // API key should be redacted in debug output
        assert!(debug_output.contains("<redacted>"));
        assert!(!debug_output.contains("sk-secret-key-12345"));
        // Other fields should still be visible
        assert!(debug_output.contains("TextEmbedding3Small"));
    }

    #[test]
    fn test_api_key_getter() {
        let config = OpenAIConfig::new("test-key".to_string(), OpenAIModel::Ada002);

        // Internal getter should return the key
        assert_eq!(config.api_key(), "test-key");
    }

    #[test]
    fn test_provider_creation() {
        let config = OpenAIConfig::new("test-key".to_string(), OpenAIModel::TextEmbedding3Small);
        let provider = OpenAIProvider::new(config);
        assert!(provider.is_ok());

        let provider = provider.unwrap();
        assert_eq!(provider.dimensions(), 1536);
        assert_eq!(provider.name(), "text-embedding-3-small");
        assert!(provider.normalized_by_default());
        assert!(provider.max_text_length().is_some());
    }
}
