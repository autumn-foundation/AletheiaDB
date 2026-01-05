//! High-level embedding service API.
//!
//! The `EmbeddingService` wraps an `EmbeddingProvider` and provides additional
//! functionality like normalization control and input validation.

use super::{EmbeddingError, EmbeddingProvider};
use crate::core::vector::{normalize_in_place, validate_vector};
use std::sync::Arc;

/// High-level embedding service that manages providers and normalization.
///
/// # Example
///
/// ```ignore
/// use gallifreydb::embeddings::{EmbeddingService, providers::openai::*};
/// use std::sync::Arc;
///
/// let config = OpenAIConfig::from_env(OpenAIModel::TextEmbedding3Small)?;
/// let provider = Arc::new(OpenAIProvider::new(config)?);
/// let service = EmbeddingService::new(provider);
///
/// let embedding = service.embed("Hello, world!").await?;
/// assert_eq!(embedding.len(), 1536);
/// ```
pub struct EmbeddingService {
    provider: Arc<dyn EmbeddingProvider>,
    /// Whether to normalize embeddings (overrides provider default)
    normalize: Option<bool>,
}

impl EmbeddingService {
    /// Create a new embedding service with the given provider.
    ///
    /// # Arguments
    ///
    /// * `provider` - An `Arc`-wrapped provider implementing `EmbeddingProvider`
    ///
    /// # Example
    ///
    /// ```ignore
    /// use std::sync::Arc;
    /// let service = EmbeddingService::new(Arc::new(provider));
    /// ```
    pub fn new(provider: Arc<dyn EmbeddingProvider>) -> Self {
        Self {
            provider,
            normalize: None,
        }
    }

    /// Force normalization on or off (overrides provider default).
    ///
    /// By default, the service respects the provider's `normalized_by_default()`
    /// setting. Use this method to override that behavior.
    ///
    /// # Arguments
    ///
    /// * `normalize` - `true` to always normalize, `false` to skip normalization
    ///
    /// # Example
    ///
    /// ```ignore
    /// let service = EmbeddingService::new(provider)
    ///     .with_normalization(true);  // Always normalize
    /// ```
    pub fn with_normalization(mut self, normalize: bool) -> Self {
        self.normalize = Some(normalize);
        self
    }

    /// Generate an embedding for a single text.
    ///
    /// # Arguments
    ///
    /// * `text` - The input text to embed
    ///
    /// # Returns
    ///
    /// * `Ok(Vec<f32>)` - The embedding vector, normalized if configured
    /// * `Err(EmbeddingError)` - If embedding generation fails
    ///
    /// # Validation
    ///
    /// - Checks text length against provider's `max_text_length()`
    /// - Validates returned embedding has no NaN/Inf values
    /// - Normalizes if needed based on configuration
    ///
    /// # Example
    ///
    /// ```ignore
    /// let embedding = service.embed("GallifreyDB is a bi-temporal graph database").await?;
    /// println!("Generated {}-dimensional embedding", embedding.len());
    /// ```
    pub async fn embed(&self, text: &str) -> Result<Vec<f32>, EmbeddingError> {
        // Validate text length
        if let Some(max_len) = self.provider.max_text_length() {
            if text.len() > max_len {
                return Err(EmbeddingError::TextTooLong {
                    length: text.len(),
                    max_length: max_len,
                });
            }
        }

        // Generate embedding from provider
        let mut embedding = self.provider.embed(text).await?;

        // Validate dimensions
        let expected_dims = self.provider.dimensions();
        if embedding.len() != expected_dims {
            return Err(EmbeddingError::DimensionMismatch {
                expected: expected_dims,
                actual: embedding.len(),
            });
        }

        // Validate no NaN/Inf values
        validate_vector(&embedding).map_err(|e| EmbeddingError::ProviderError {
            provider: self.provider.name().to_string(),
            message: format!("Invalid embedding returned: {}", e),
            status_code: None,
        })?;

        // Normalize if configured
        let should_normalize = self
            .normalize
            .unwrap_or_else(|| !self.provider.normalized_by_default());

        if should_normalize {
            normalize_in_place(&mut embedding);
        }

        Ok(embedding)
    }

    /// Generate embeddings for multiple texts in a batch.
    ///
    /// This is more efficient than calling `embed()` multiple times for
    /// API-based providers, as it can batch requests and reduce overhead.
    ///
    /// # Arguments
    ///
    /// * `texts` - Slice of input texts to embed
    ///
    /// # Returns
    ///
    /// * `Ok(Vec<Vec<f32>>)` - Embeddings in the same order as inputs
    /// * `Err(EmbeddingError)` - If any embedding fails
    ///
    /// # Validation
    ///
    /// - Validates all text lengths before processing
    /// - Validates all returned embeddings
    /// - Normalizes all embeddings if configured
    ///
    /// # Example
    ///
    /// ```ignore
    /// let texts = vec!["first document", "second document", "third document"];
    /// let embeddings = service.embed_batch(&texts).await?;
    /// assert_eq!(embeddings.len(), 3);
    /// ```
    pub async fn embed_batch(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, EmbeddingError> {
        // Validate all text lengths
        if let Some(max_len) = self.provider.max_text_length() {
            for text in texts.iter() {
                if text.len() > max_len {
                    return Err(EmbeddingError::TextTooLong {
                        length: text.len(),
                        max_length: max_len,
                    });
                }
            }
        }

        // Generate embeddings from provider
        let mut embeddings = self.provider.embed_batch(texts).await?;

        // Validate dimensions and values for all embeddings
        let expected_dims = self.provider.dimensions();
        for (i, embedding) in embeddings.iter().enumerate() {
            if embedding.len() != expected_dims {
                return Err(EmbeddingError::DimensionMismatch {
                    expected: expected_dims,
                    actual: embedding.len(),
                });
            }

            validate_vector(embedding).map_err(|e| EmbeddingError::ProviderError {
                provider: self.provider.name().to_string(),
                message: format!("Invalid embedding at index {}: {}", i, e),
                status_code: None,
            })?;
        }

        // Apply normalization based on configuration
        // Three-state logic:
        // 1. normalize = Some(true):  Always normalize (even if provider already does)
        // 2. normalize = Some(false): Never normalize (trust provider output)
        // 3. normalize = None:        Auto-detect (normalize only if provider doesn't)
        let should_normalize = self
            .normalize
            .unwrap_or_else(|| !self.provider.normalized_by_default());

        if should_normalize {
            for embedding in &mut embeddings {
                normalize_in_place(embedding);
            }
        }

        Ok(embeddings)
    }

    /// Get the dimensions of embeddings produced by this service.
    ///
    /// This is determined by the underlying provider.
    ///
    /// # Example
    ///
    /// ```ignore
    /// println!("Embeddings will have {} dimensions", service.dimensions());
    /// ```
    pub fn dimensions(&self) -> usize {
        self.provider.dimensions()
    }

    /// Get the name of the underlying provider.
    ///
    /// Useful for debugging and logging.
    ///
    /// # Example
    ///
    /// ```ignore
    /// println!("Using provider: {}", service.provider_name());
    /// ```
    pub fn provider_name(&self) -> &str {
        self.provider.name()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;

    /// Mock provider for testing
    struct MockProvider {
        dimensions: usize,
        normalized: bool,
    }

    #[async_trait]
    impl EmbeddingProvider for MockProvider {
        fn dimensions(&self) -> usize {
            self.dimensions
        }

        fn name(&self) -> &str {
            "mock"
        }

        async fn embed(&self, text: &str) -> Result<Vec<f32>, EmbeddingError> {
            // Return deterministic embedding based on text length
            let value = text.len() as f32 / 100.0;
            Ok(vec![value; self.dimensions])
        }

        fn normalized_by_default(&self) -> bool {
            self.normalized
        }
    }

    #[tokio::test]
    async fn test_service_with_mock_provider() {
        let provider = Arc::new(MockProvider {
            dimensions: 384,
            normalized: false,
        });
        let service = EmbeddingService::new(provider);

        let embedding = service.embed("test input").await.unwrap();
        assert_eq!(embedding.len(), 384);
    }

    #[tokio::test]
    async fn test_batch_embedding() {
        let provider = Arc::new(MockProvider {
            dimensions: 128,
            normalized: false,
        });
        let service = EmbeddingService::new(provider);

        let texts = vec!["first", "second", "third"];
        let embeddings = service.embed_batch(&texts).await.unwrap();

        assert_eq!(embeddings.len(), 3);
        assert_eq!(embeddings[0].len(), 128);
    }

    #[tokio::test]
    async fn test_normalization_override() {
        let provider = Arc::new(MockProvider {
            dimensions: 10,
            normalized: false,
        });

        // Force normalization on
        let service = EmbeddingService::new(provider).with_normalization(true);

        let embedding = service.embed("test").await.unwrap();

        // Check that embedding is normalized (magnitude ≈ 1.0)
        let magnitude: f32 = embedding.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((magnitude - 1.0).abs() < 1e-6);
    }

    #[tokio::test]
    async fn test_dimensions_getter() {
        let provider = Arc::new(MockProvider {
            dimensions: 768,
            normalized: true,
        });
        let service = EmbeddingService::new(provider);

        assert_eq!(service.dimensions(), 768);
    }

    #[tokio::test]
    async fn test_provider_name_getter() {
        let provider = Arc::new(MockProvider {
            dimensions: 384,
            normalized: true,
        });
        let service = EmbeddingService::new(provider);

        assert_eq!(service.provider_name(), "mock");
    }
}
