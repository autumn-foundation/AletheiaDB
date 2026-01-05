//! Integration tests for the embedding system.
//!
//! These tests use mock providers to test the embedding pipeline without
//! requiring API keys or external dependencies.

use gallifreydb::embeddings::{EmbeddingError, EmbeddingProvider, EmbeddingService};
use async_trait::async_trait;
use std::sync::Arc;

/// Mock provider for testing
struct MockProvider {
    dimensions: usize,
    normalized: bool,
    fail_on_text: Option<String>,
}

impl MockProvider {
    fn new(dimensions: usize) -> Self {
        Self {
            dimensions,
            normalized: false,
            fail_on_text: None,
        }
    }

    fn with_normalization(mut self, normalized: bool) -> Self {
        self.normalized = normalized;
        self
    }

    fn with_failure(mut self, text: String) -> Self {
        self.fail_on_text = Some(text);
        self
    }
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
        // Fail if configured to fail on this text
        if let Some(fail_text) = &self.fail_on_text {
            if text == fail_text {
                return Err(EmbeddingError::ProviderError {
                    provider: "mock".to_string(),
                    message: "Simulated failure".to_string(),
                    status_code: None,
                });
            }
        }

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
    let provider = Arc::new(MockProvider::new(384));
    let service = EmbeddingService::new(provider);

    let embedding = service.embed("test input").await.unwrap();
    assert_eq!(embedding.len(), 384);
    assert_eq!(service.dimensions(), 384);
    assert_eq!(service.provider_name(), "mock");
}

#[tokio::test]
async fn test_batch_embedding() {
    let provider = Arc::new(MockProvider::new(128));
    let service = EmbeddingService::new(provider);

    let texts = vec!["first", "second", "third"];
    let embeddings = service.embed_batch(&texts).await.unwrap();

    assert_eq!(embeddings.len(), 3);
    assert_eq!(embeddings[0].len(), 128);
    assert_eq!(embeddings[1].len(), 128);
    assert_eq!(embeddings[2].len(), 128);
}

#[tokio::test]
async fn test_normalization_override() {
    let provider = Arc::new(MockProvider::new(10));

    // Force normalization on
    let service = EmbeddingService::new(provider).with_normalization(true);

    let embedding = service.embed("test").await.unwrap();

    // Check that embedding is normalized (magnitude ≈ 1.0)
    let magnitude: f32 = embedding.iter().map(|x| x * x).sum::<f32>().sqrt();
    assert!((magnitude - 1.0).abs() < 1e-6);
}

#[tokio::test]
async fn test_normalization_skip() {
    let provider = Arc::new(MockProvider::new(10).with_normalization(true));

    // Provider says normalized, so service should skip
    let service = EmbeddingService::new(provider);

    let embedding = service.embed("test").await.unwrap();

    // Should be the raw values (not normalized)
    let expected_value = 4.0 / 100.0; // "test".len() / 100.0
    assert!((embedding[0] - expected_value).abs() < 1e-6);
}

#[tokio::test]
async fn test_error_propagation() {
    let provider = Arc::new(MockProvider::new(128).with_failure("fail".to_string()));
    let service = EmbeddingService::new(provider);

    // Should succeed for normal text
    let result = service.embed("success").await;
    assert!(result.is_ok());

    // Should fail for configured text
    let result = service.embed("fail").await;
    assert!(result.is_err());
    match result {
        Err(EmbeddingError::ProviderError { provider, .. }) => {
            assert_eq!(provider, "mock");
        }
        _ => panic!("Expected ProviderError"),
    }
}

#[tokio::test]
async fn test_empty_batch() {
    let provider = Arc::new(MockProvider::new(128));
    let service = EmbeddingService::new(provider);

    let embeddings = service.embed_batch(&[]).await.unwrap();
    assert_eq!(embeddings.len(), 0);
}

#[tokio::test]
async fn test_large_batch() {
    let provider = Arc::new(MockProvider::new(64));
    let service = EmbeddingService::new(provider);

    let texts: Vec<&str> = (0..100).map(|_| "test").collect();
    let embeddings = service.embed_batch(&texts).await.unwrap();

    assert_eq!(embeddings.len(), 100);
    for emb in embeddings {
        assert_eq!(emb.len(), 64);
    }
}

#[tokio::test]
async fn test_different_dimensions() {
    // Test common embedding dimensions
    for dims in [384, 768, 1536, 3072] {
        let provider = Arc::new(MockProvider::new(dims));
        let service = EmbeddingService::new(provider);

        let embedding = service.embed("test").await.unwrap();
        assert_eq!(embedding.len(), dims);
    }
}

#[tokio::test]
async fn test_concurrent_embedding() {
    let provider = Arc::new(MockProvider::new(256));
    let service = Arc::new(EmbeddingService::new(provider));

    // Spawn multiple concurrent embedding tasks
    let mut handles = vec![];
    for i in 0..10 {
        let service = Arc::clone(&service);
        let text = format!("text {}", i);
        handles.push(tokio::spawn(async move {
            service.embed(&text).await
        }));
    }

    // Wait for all to complete
    for handle in handles {
        let result = handle.await.unwrap();
        assert!(result.is_ok());
        assert_eq!(result.unwrap().len(), 256);
    }
}
