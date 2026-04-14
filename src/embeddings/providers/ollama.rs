//! Ollama embedding provider.
//!
//! Provides local embedding generation using Ollama, which runs models locally
//! via a simple HTTP API. Ollama is easy to install and manage, making it
//! ideal for local development and deployment.
//!
//! # Supported Models
//!
//! - `nomic-embed-text` (768 dimensions) - High quality, efficient
//! - `mxbai-embed-large` (1024 dimensions) - Highest quality
//! - `all-minilm` (384 dimensions) - Fast, lightweight
//!
//! # Prerequisites
//!
//! Ollama must be installed and running:
//! 1. Install: <https://ollama.ai>
//! 2. Pull a model: `ollama pull nomic-embed-text`
//! 3. Ollama server runs on `localhost:11434` by default
//!
//! # Example
//!
//! ```ignore
//! use aletheiadb::embeddings::{EmbeddingService, providers::ollama::*};
//! use std::sync::Arc;
//!
//! let config = OllamaConfig::nomic_embed_text();
//! let provider = Arc::new(OllamaProvider::new(config)?);
//! let service = EmbeddingService::new(provider);
//!
//! let embedding = service.embed("Hello, world!").await?;
//! assert_eq!(embedding.len(), 768);
//! ```

use super::super::{EmbeddingError, EmbeddingProvider};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// Configuration for Ollama provider.
#[derive(Clone)]
pub struct OllamaConfig {
    /// Model name (e.g., "nomic-embed-text", "mxbai-embed-large")
    pub model: String,
    /// Expected dimensions
    pub dimensions: usize,
    /// Ollama API base URL (default: http://localhost:11434)
    pub base_url: String,
    /// Request timeout in seconds
    pub timeout_secs: u64,
}

impl OllamaConfig {
    /// Create a new config with the specified model and dimensions.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let config = OllamaConfig::new("nomic-embed-text".to_string(), 768);
    /// ```
    pub fn new(model: String, dimensions: usize) -> Self {
        Self {
            model,
            dimensions,
            base_url: "http://localhost:11434".to_string(),
            timeout_secs: 60,
        }
    }

    /// Create config for nomic-embed-text model (768 dimensions).
    ///
    /// High quality embeddings with good performance.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let config = OllamaConfig::nomic_embed_text();
    /// ```
    pub fn nomic_embed_text() -> Self {
        Self::new("nomic-embed-text".to_string(), 768)
    }

    /// Create config for mxbai-embed-large model (1024 dimensions).
    ///
    /// Highest quality embeddings, slower inference.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let config = OllamaConfig::mxbai_embed_large();
    /// ```
    pub fn mxbai_embed_large() -> Self {
        Self::new("mxbai-embed-large".to_string(), 1024)
    }

    /// Create config for all-minilm model (384 dimensions).
    ///
    /// Fast, lightweight embeddings.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let config = OllamaConfig::all_minilm();
    /// ```
    pub fn all_minilm() -> Self {
        Self::new("all-minilm".to_string(), 384)
    }

    /// Set a custom Ollama base URL.
    ///
    /// Useful for remote Ollama instances or custom ports.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let config = OllamaConfig::nomic_embed_text()
    ///     .with_base_url("http://192.168.1.100:11434".to_string());
    /// ```
    pub fn with_base_url(mut self, base_url: String) -> Self {
        self.base_url = base_url;
        self
    }

    /// Set a custom timeout in seconds.
    pub fn with_timeout(mut self, timeout_secs: u64) -> Self {
        self.timeout_secs = timeout_secs;
        self
    }
}

/// Ollama embedding provider.
pub struct OllamaProvider {
    config: OllamaConfig,
    client: reqwest::Client,
}

impl OllamaProvider {
    /// Create a new Ollama provider.
    ///
    /// # Errors
    ///
    /// Returns `EmbeddingError::ConfigError` if the HTTP client cannot be created.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let config = OllamaConfig::nomic_embed_text();
    /// let provider = OllamaProvider::new(config)?;
    /// ```
    pub fn new(config: OllamaConfig) -> Result<Self, EmbeddingError> {
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
struct OllamaRequest<'a> {
    model: &'a str,
    prompt: &'a str,
}

#[derive(Deserialize)]
struct OllamaResponse {
    embedding: Vec<f32>,
}

#[async_trait]
impl EmbeddingProvider for OllamaProvider {
    fn dimensions(&self) -> usize {
        self.config.dimensions
    }

    fn name(&self) -> &str {
        &self.config.model
    }

    async fn embed(&self, text: &str) -> Result<Vec<f32>, EmbeddingError> {
        let url = format!("{}/api/embeddings", self.config.base_url);

        let request = OllamaRequest {
            model: &self.config.model,
            prompt: text,
        };

        let response = self
            .client
            .post(&url)
            .json(&request)
            .send()
            .await
            .map_err(|e| {
                // Check if it's a connection error (Ollama not running)
                if e.is_connect() {
                    EmbeddingError::NetworkError(format!(
                        "Cannot connect to Ollama at {}. Is Ollama running?",
                        self.config.base_url
                    ))
                } else {
                    EmbeddingError::NetworkError(e.to_string())
                }
            })?;

        let status = response.status();

        // Handle model not found (model not pulled)
        if status == reqwest::StatusCode::NOT_FOUND {
            return Err(EmbeddingError::ModelNotFound {
                model: self.config.model.clone(),
                provider: "Ollama".to_string(),
            });
        }

        // Handle other errors
        if !status.is_success() {
            let error_text = response
                .text()
                .await
                .unwrap_or_else(|_| "Unknown error".to_string());
            return Err(EmbeddingError::ProviderError {
                provider: "Ollama".to_string(),
                message: error_text,
                status_code: Some(status.as_u16()),
            });
        }

        // Parse response
        let ollama_response: OllamaResponse =
            response
                .json()
                .await
                .map_err(|e| EmbeddingError::ProviderError {
                    provider: "Ollama".to_string(),
                    message: format!("Failed to parse response: {}", e),
                    status_code: None,
                })?;

        // Validate dimensions
        if ollama_response.embedding.len() != self.config.dimensions {
            return Err(EmbeddingError::DimensionMismatch {
                expected: self.config.dimensions,
                actual: ollama_response.embedding.len(),
            });
        }

        Ok(ollama_response.embedding)
    }

    async fn embed_batch(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, EmbeddingError> {
        // Ollama API doesn't support batch requests natively
        // Process sequentially (could be parallelized with tokio::join!)
        let mut results = Vec::with_capacity(texts.len());
        for text in texts {
            results.push(self.embed(text).await?);
        }
        Ok(results)
    }

    fn normalized_by_default(&self) -> bool {
        true // Ollama models typically normalize embeddings
    }

    fn max_text_length(&self) -> Option<usize> {
        // Most Ollama models support long contexts (2048+ tokens)
        // Conservative estimate
        Some(2048 * 4)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_config_presets() {
        let nomic = OllamaConfig::nomic_embed_text();
        assert_eq!(nomic.model, "nomic-embed-text");
        assert_eq!(nomic.dimensions, 768);
        assert_eq!(nomic.base_url, "http://localhost:11434");

        let mxbai = OllamaConfig::mxbai_embed_large();
        assert_eq!(mxbai.model, "mxbai-embed-large");
        assert_eq!(mxbai.dimensions, 1024);

        let minilm = OllamaConfig::all_minilm();
        assert_eq!(minilm.model, "all-minilm");
        assert_eq!(minilm.dimensions, 384);
    }

    #[test]
    fn test_config_builder() {
        let config = OllamaConfig::new("custom-model".to_string(), 512)
            .with_base_url("http://192.168.1.100:11434".to_string())
            .with_timeout(120);

        assert_eq!(config.model, "custom-model");
        assert_eq!(config.dimensions, 512);
        assert_eq!(config.base_url, "http://192.168.1.100:11434");
        assert_eq!(config.timeout_secs, 120);
    }

    #[test]
    fn test_provider_creation() {
        let config = OllamaConfig::nomic_embed_text();
        let provider = OllamaProvider::new(config);
        assert!(provider.is_ok());

        let provider = provider.unwrap();
        assert_eq!(provider.dimensions(), 768);
        assert_eq!(provider.name(), "nomic-embed-text");
        assert!(provider.normalized_by_default());
        assert!(provider.max_text_length().is_some());
    }

    #[test]
    fn test_provider_with_custom_config() {
        let config = OllamaConfig::new("my-model".to_string(), 256);
        let provider = OllamaProvider::new(config).unwrap();

        assert_eq!(provider.dimensions(), 256);
        assert_eq!(provider.name(), "my-model");
    }
}
