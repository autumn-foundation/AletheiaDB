//! Embedding generation for vector properties.
//!
//! This module provides a plugin architecture for generating embeddings from text
//! using various providers (OpenAI, HuggingFace, local ONNX models, Ollama).
//!
//! # Architecture
//!
//! The embedding system is completely decoupled from the core database. Users:
//! 1. Create an `EmbeddingService` with their chosen provider
//! 2. Generate embeddings from text
//! 3. Store embeddings using the existing `PropertyMapBuilder::insert_vector()` API
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
//! ```
//!
//! # Feature Flags
//!
//! The embedding system is optional and enabled via Cargo features:
//! - `embeddings` - Core embedding traits (required for all providers)
//! - `embedding-openai` - OpenAI provider
//! - `embedding-huggingface` - HuggingFace Inference API provider
//! - `embedding-onnx` - Local ONNX models
//! - `embedding-ollama` - Ollama local models
//! - `embedding-all` - Enable all providers

#[cfg(feature = "embeddings")]
use async_trait::async_trait;
#[cfg(feature = "embeddings")]

pub mod service;

#[cfg(any(
    feature = "embedding-openai",
    feature = "embedding-huggingface",
    feature = "embedding-onnx",
    feature = "embedding-ollama"
))]
pub mod providers;

// Re-export service for convenience
#[cfg(feature = "embeddings")]
pub use service::EmbeddingService;

/// Trait for embedding providers that convert text to dense vectors.
///
/// All providers must implement this trait to be compatible with the
/// `EmbeddingService`. Implementations can be sync (ONNX) or async (API-based).
///
/// # Thread Safety
///
/// Providers must be `Send + Sync` to work with async runtimes and allow
/// concurrent embedding generation.
///
/// # Example Implementation
///
/// ```ignore
/// use gallifreydb::embeddings::{EmbeddingProvider, EmbeddingError};
/// use async_trait::async_trait;
///
/// struct MyProvider { /* fields */ }
///
/// #[async_trait]
/// impl EmbeddingProvider for MyProvider {
///     fn dimensions(&self) -> usize { 384 }
///     fn name(&self) -> &str { "my-provider" }
///
///     async fn embed(&self, text: &str) -> Result<Vec<f32>, EmbeddingError> {
///         // Generate embedding...
///         Ok(vec![0.0; 384])
///     }
/// }
/// ```
#[cfg(feature = "embeddings")]
#[async_trait]
pub trait EmbeddingProvider: Send + Sync {
    /// Get the dimensions of embeddings produced by this provider.
    ///
    /// This is a static property of the model (e.g., 384 for MiniLM, 1536 for OpenAI).
    /// Must be consistent across all calls.
    fn dimensions(&self) -> usize;

    /// Get a human-readable name for this provider.
    ///
    /// Used for error messages and debugging.
    ///
    /// # Examples
    ///
    /// - `"openai:text-embedding-3-small"`
    /// - `"onnx:all-MiniLM-L6-v2"`
    /// - `"huggingface:sentence-transformers/all-mpnet-base-v2"`
    fn name(&self) -> &str;

    /// Generate an embedding for a single text input.
    ///
    /// # Arguments
    ///
    /// * `text` - The input text to embed. Provider may have length limits.
    ///
    /// # Returns
    ///
    /// * `Ok(Vec<f32>)` - The embedding vector (length = `dimensions()`)
    /// * `Err(EmbeddingError)` - If embedding generation fails
    ///
    /// # Normalization
    ///
    /// Some providers return normalized embeddings (unit length), others don't.
    /// Use `normalized_by_default()` to check. The `EmbeddingService` can
    /// automatically normalize if needed.
    ///
    /// # Errors
    ///
    /// Returns `EmbeddingError` for:
    /// - Network failures (API-based providers)
    /// - Authentication failures (invalid API keys)
    /// - Rate limiting
    /// - Model loading failures (local providers)
    /// - Invalid input text
    async fn embed(&self, text: &str) -> Result<Vec<f32>, EmbeddingError>;

    /// Generate embeddings for multiple texts in a batch.
    ///
    /// Batching can significantly improve throughput for API-based providers
    /// by reducing HTTP overhead and leveraging server-side parallelism.
    ///
    /// # Arguments
    ///
    /// * `texts` - Slice of input texts to embed
    ///
    /// # Returns
    ///
    /// * `Ok(Vec<Vec<f32>>)` - Embeddings in the same order as inputs
    /// * `Err(EmbeddingError)` - If any embedding fails (partial results discarded)
    ///
    /// # Default Implementation
    ///
    /// The default implementation calls `embed()` sequentially. Providers
    /// should override this for true batch processing when supported.
    async fn embed_batch(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, EmbeddingError> {
        let mut results = Vec::with_capacity(texts.len());
        for text in texts {
            results.push(self.embed(text).await?);
        }
        Ok(results)
    }

    /// Check if this provider normalizes embeddings to unit length by default.
    ///
    /// If `true`, the `EmbeddingService` will skip redundant normalization.
    ///
    /// # Examples
    ///
    /// - OpenAI: `true` (embeddings are pre-normalized)
    /// - sentence-transformers: `true` (typically normalized)
    /// - Raw BERT models: `false` (may need normalization)
    fn normalized_by_default(&self) -> bool {
        false
    }

    /// Maximum text length (in characters) supported by this provider.
    ///
    /// Returns `None` if no limit exists or limit is unknown.
    /// Used for validation before calling the provider.
    ///
    /// # Note
    ///
    /// This is a character count, not a token count. Some providers work
    /// with tokens, so this is an approximation. Providers should be
    /// conservative (e.g., estimate 1 token ≈ 4 characters for English).
    fn max_text_length(&self) -> Option<usize> {
        None
    }
}

/// Specialized error type for embedding operations.
///
/// Provides detailed error information for different failure modes:
/// - API errors (network, auth, rate limits)
/// - Model errors (loading, inference)
/// - Validation errors (dimensions, text length)
#[derive(Debug, Clone)]
pub enum EmbeddingError {
    /// The provider API returned an error.
    ///
    /// Includes the provider name, error message, and optional HTTP status code.
    ProviderError {
        /// Provider name (e.g., "OpenAI", "HuggingFace")
        provider: String,
        /// Error message from the provider
        message: String,
        /// HTTP status code if applicable
        status_code: Option<u16>,
    },

    /// API authentication failed (invalid API key or token).
    AuthenticationFailed {
        /// Provider name
        provider: String,
        /// Reason for auth failure
        reason: String,
    },

    /// Rate limit exceeded.
    ///
    /// Some providers include a retry-after duration in their response.
    RateLimitExceeded {
        /// Provider name
        provider: String,
        /// Optional duration to wait before retrying
        retry_after: Option<std::time::Duration>,
    },

    /// Input text exceeds provider's maximum length.
    TextTooLong {
        /// Actual text length
        length: usize,
        /// Maximum allowed length
        max_length: usize,
    },

    /// Embedding dimension mismatch.
    ///
    /// Occurs when the provider returns an embedding with unexpected dimensions.
    DimensionMismatch {
        /// Expected dimensions (from `dimensions()`)
        expected: usize,
        /// Actual dimensions received
        actual: usize,
    },

    /// Model not found or unavailable.
    ModelNotFound {
        /// Model identifier
        model: String,
        /// Provider name
        provider: String,
    },

    /// Network or connectivity error.
    NetworkError(String),

    /// Local model loading failed (ONNX, Ollama).
    ModelLoadError {
        /// Model identifier
        model: String,
        /// Error reason
        reason: String,
    },

    /// Invalid configuration.
    ConfigError(String),

    /// Generic error.
    Other(String),
}

#[cfg(feature = "embeddings")]
impl std::fmt::Display for EmbeddingError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EmbeddingError::ProviderError {
                provider,
                message,
                status_code,
            } => {
                write!(f, "{} provider error", provider)?;
                if let Some(code) = status_code {
                    write!(f, " (HTTP {})", code)?;
                }
                write!(f, ": {}", message)
            }
            EmbeddingError::AuthenticationFailed { provider, reason } => {
                write!(f, "{} authentication failed: {}", provider, reason)
            }
            EmbeddingError::RateLimitExceeded {
                provider,
                retry_after,
            } => {
                write!(f, "{} rate limit exceeded", provider)?;
                if let Some(duration) = retry_after {
                    write!(f, ", retry after {:?}", duration)?;
                }
                Ok(())
            }
            EmbeddingError::TextTooLong { length, max_length } => {
                write!(f, "Text length {} exceeds maximum {}", length, max_length)
            }
            EmbeddingError::DimensionMismatch { expected, actual } => {
                write!(f, "Expected {} dimensions, got {}", expected, actual)
            }
            EmbeddingError::ModelNotFound { model, provider } => {
                write!(f, "Model '{}' not found in {}", model, provider)
            }
            EmbeddingError::NetworkError(msg) => write!(f, "Network error: {}", msg),
            EmbeddingError::ModelLoadError { model, reason } => {
                write!(f, "Failed to load model '{}': {}", model, reason)
            }
            EmbeddingError::ConfigError(msg) => write!(f, "Configuration error: {}", msg),
            EmbeddingError::Other(msg) => write!(f, "{}", msg),
        }
    }
}

#[cfg(feature = "embeddings")]
impl std::error::Error for EmbeddingError {}

/// Convert `EmbeddingError` to GallifreyDB's main `Error` type.
///
/// This allows embedding errors to be used with the `?` operator in
/// functions that return `Result<T, gallifreydb::utils::Error>`.
#[cfg(feature = "embeddings")]
impl From<EmbeddingError> for crate::utils::Error {
    fn from(e: EmbeddingError) -> Self {
        crate::utils::Error::Other(format!("Embedding error: {}", e))
    }
}
