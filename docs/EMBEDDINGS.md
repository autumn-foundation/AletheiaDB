# Embedding Generation for GallifreyDB

GallifreyDB provides optional embedding generation capabilities through a plugin-based provider system. This allows you to generate vector embeddings from text using various services and local models, while maintaining the flexibility to bring your own embeddings.

## Table of Contents

- [Overview](#overview)
- [Architecture](#architecture)
- [Getting Started](#getting-started)
- [Providers](#providers)
  - [OpenAI](#openai)
  - [HuggingFace](#huggingface)
  - [Ollama](#ollama)
  - [ONNX](#onnx)
- [Usage Examples](#usage-examples)
- [Provider Comparison](#provider-comparison)
- [Best Practices](#best-practices)
- [Troubleshooting](#troubleshooting)

## Overview

The embedding system is **completely optional** and decoupled from the core database. You can:

1. **Use embedding providers** - Generate embeddings automatically from text
2. **Bring your own** - Provide pre-computed embeddings from any source
3. **Mix and match** - Use different providers for different use cases

### Key Features

- ✅ **4 Provider Implementations**: OpenAI, HuggingFace, Ollama, ONNX
- ✅ **Zero-Cost Abstraction**: No overhead when not used (via feature flags)
- ✅ **Plugin Architecture**: Easy to add custom providers
- ✅ **Async-First**: Efficient batch processing
- ✅ **Type-Safe**: Strong typing with comprehensive error handling

## Architecture

```
User Application
    │
    ├─→ EmbeddingService (optional) ─→ Providers (OpenAI, HF, ONNX, Ollama)
    │                                          ↓
    └─→ GallifreyDB ←──────────────── Vec<f32> embeddings
```

**Key Principle**: The database layer is pure - it only stores and indexes vectors. The embedding service is a separate, optional layer that generates embeddings before storage.

## Getting Started

### 1. Enable Feature Flags

Add the embedding features you need to your `Cargo.toml`:

```toml
[dependencies]
gallifreydb = { version = "0.1", features = ["embedding-openai"] }
```

Available features:
- `embeddings` - Core embedding traits (required for all providers)
- `embedding-openai` - OpenAI provider
- `embedding-huggingface` - HuggingFace Inference API
- `embedding-onnx` - Local ONNX models
- `embedding-ollama` - Ollama local models
- `embedding-all` - Enable all providers

### 2. Basic Usage

```rust
use gallifreydb::embeddings::{EmbeddingService, providers::openai::*};
use gallifreydb::{GallifreyDB, PropertyMapBuilder};
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 1. Create embedding service
    let config = OpenAIConfig::from_env(OpenAIModel::TextEmbedding3Small)?;
    let provider = Arc::new(OpenAIProvider::new(config)?);
    let service = EmbeddingService::new(provider);

    // 2. Generate embedding
    let text = "GallifreyDB is a bi-temporal graph database";
    let embedding = service.embed(text).await?;

    // 3. Store in database
    let db = GallifreyDB::new();
    let node_id = db.create_node(
        "Document",
        PropertyMapBuilder::new()
            .insert("content", text)
            .insert_vector("embedding", &embedding)
            .build(),
    )?;

    Ok(())
}
```

## Providers

### OpenAI

High-quality embeddings via OpenAI's API. Best for production applications where quality matters more than cost.

#### Setup

```bash
export OPENAI_API_KEY=sk-...
```

#### Configuration

```rust
use gallifreydb::embeddings::providers::openai::*;

// From environment variable
let config = OpenAIConfig::from_env(OpenAIModel::TextEmbedding3Small)?;

// Or explicit API key
let config = OpenAIConfig::new(
    "sk-...".to_string(),
    OpenAIModel::TextEmbedding3Small
);

// Customize
let config = config
    .with_base_url("https://custom.api".to_string())
    .with_timeout(60);
```

#### Available Models

| Model | Dimensions | Cost | Best For |
|-------|-----------|------|----------|
| `TextEmbedding3Small` | 1536 | $0.02/1M tokens | Most use cases |
| `TextEmbedding3Large` | 3072 | $0.13/1M tokens | Highest quality |
| `Ada002` | 1536 | $0.10/1M tokens | Legacy (deprecated) |

#### Characteristics

- ✅ **Normalized**: Yes (returned embeddings are unit vectors)
- ⏱️ **Latency**: ~100-200ms
- 💰 **Cost**: Paid per request
- 🔒 **Privacy**: Data sent to OpenAI
- 📊 **Quality**: Excellent
- 🚀 **Batch Support**: Yes (efficient)

---

### HuggingFace

Access to thousands of open-source embedding models via the HuggingFace Inference API.

#### Setup

```bash
export HF_TOKEN=hf_...
```

#### Configuration

```rust
use gallifreydb::embeddings::providers::huggingface::*;

// From environment variable
let config = HuggingFaceConfig::from_env(
    "sentence-transformers/all-MiniLM-L6-v2".to_string(),
    384
)?;

// Or use presets
let config = HuggingFaceConfig::all_minilm_l6_v2()?;  // 384 dims
let config = HuggingFaceConfig::all_mpnet_base_v2()?;  // 768 dims
```

#### Popular Models

| Model | Dimensions | Best For |
|-------|-----------|----------|
| `sentence-transformers/all-MiniLM-L6-v2` | 384 | Fast, good quality |
| `sentence-transformers/all-mpnet-base-v2` | 768 | Higher quality |
| `BAAI/bge-small-en-v1.5` | 384 | Retrieval tasks |
| `BAAI/bge-large-en-v1.5` | 1024 | Best quality |

#### Characteristics

- ✅ **Normalized**: Depends on model (sentence-transformers: yes)
- ⏱️ **Latency**: ~200-500ms
- 💰 **Cost**: Free tier available
- 🔒 **Privacy**: Data sent to HuggingFace
- 📊 **Quality**: Good to excellent (model-dependent)
- 🚀 **Batch Support**: Yes

---

### Ollama

Local embedding generation using Ollama. Best for privacy-sensitive applications and low-latency requirements.

#### Setup

1. Install Ollama: https://ollama.ai
2. Pull a model:
```bash
ollama pull nomic-embed-text
```
3. Ensure Ollama is running (starts automatically on macOS/Linux)

#### Configuration

```rust
use gallifreydb::embeddings::providers::ollama::*;

// Use presets
let config = OllamaConfig::nomic_embed_text();     // 768 dims
let config = OllamaConfig::mxbai_embed_large();    // 1024 dims
let config = OllamaConfig::all_minilm();           // 384 dims

// Or custom
let config = OllamaConfig::new("my-model".to_string(), 512)
    .with_base_url("http://192.168.1.100:11434".to_string())
    .with_timeout(120);
```

#### Available Models

| Model | Dimensions | Best For |
|-------|-----------|----------|
| `nomic-embed-text` | 768 | High quality, efficient |
| `mxbai-embed-large` | 1024 | Highest quality |
| `all-minilm` | 384 | Fast, lightweight |

#### Characteristics

- ✅ **Normalized**: Yes
- ⏱️ **Latency**: ~20-50ms
- 💰 **Cost**: Free (local)
- 🔒 **Privacy**: Data never leaves your machine
- 📊 **Quality**: Good to excellent
- 🚀 **Batch Support**: Sequential (no native batching)

---

### ONNX

Ultra-fast local inference using ONNX Runtime. Best for maximum performance and complete control.

#### Setup

**Note**: Current implementation is a placeholder. Full implementation requires:

1. Download ONNX models (export from HuggingFace with `optimum`)
2. Place in `models/` directory
3. Implement tokenization

#### Configuration

```rust
use gallifreydb::embeddings::providers::onnx::*;

// Use preset
let config = OnnxConfig::default();  // all-MiniLM-L6-v2

// Or custom
let config = OnnxConfig::new(OnnxModel::AllMpnetBaseV2)
    .with_custom_model("path/to/model.onnx".to_string(), 512)
    .with_num_threads(8);
```

#### Characteristics

- ✅ **Normalized**: Yes (sentence-transformers)
- ⏱️ **Latency**: ~1-10ms (when fully implemented)
- 💰 **Cost**: Free (local)
- 🔒 **Privacy**: Complete (local)
- 📊 **Quality**: Excellent (model-dependent)
- 🚀 **Batch Support**: Yes (efficient)
- ⚠️ **Status**: Placeholder implementation

---

## Usage Examples

### Batch Processing

Process multiple documents efficiently:

```rust
let documents = vec![
    "First document",
    "Second document",
    "Third document",
];

// Generate all embeddings in a batch
let embeddings = service.embed_batch(&documents).await?;

// Store in transaction
let mut tx = db.write_transaction()?;
for (doc, embedding) in documents.iter().zip(embeddings.iter()) {
    tx.create_node(
        "Document",
        PropertyMapBuilder::new()
            .insert("content", *doc)
            .insert_vector("embedding", embedding)
            .build(),
    )?;
}
tx.commit()?;
```

### Normalization Control

```rust
// Force normalization (even if provider normalizes)
let service = EmbeddingService::new(provider)
    .with_normalization(true);

// Skip normalization (trust provider)
let service = EmbeddingService::new(provider)
    .with_normalization(false);
```

### Error Handling

```rust
use gallifreydb::embeddings::EmbeddingError;

match service.embed(text).await {
    Ok(embedding) => {
        // Use embedding
    }
    Err(EmbeddingError::AuthenticationFailed { provider, reason }) => {
        eprintln!("{} auth failed: {}", provider, reason);
    }
    Err(EmbeddingError::RateLimitExceeded { provider, retry_after }) => {
        eprintln!("{} rate limited", provider);
        if let Some(duration) = retry_after {
            eprintln!("Retry after: {:?}", duration);
        }
    }
    Err(EmbeddingError::NetworkError(msg)) => {
        eprintln!("Network error: {}", msg);
    }
    Err(e) => {
        eprintln!("Other error: {}", e);
    }
}
```

### Concurrent Embedding

```rust
use tokio::task::JoinSet;

let service = Arc::new(service);
let mut join_set = JoinSet::new();

for text in texts {
    let service = Arc::clone(&service);
    join_set.spawn(async move {
        service.embed(&text).await
    });
}

while let Some(result) = join_set.join_next().await {
    let embedding = result??;
    // Process embedding
}
```

## Provider Comparison

| Provider | Latency | Cost | Privacy | Quality | Setup Difficulty |
|----------|---------|------|---------|---------|------------------|
| OpenAI | 100-200ms | $$$ | ❌ | ⭐⭐⭐⭐⭐ | Easy |
| HuggingFace | 200-500ms | $ | ❌ | ⭐⭐⭐⭐ | Easy |
| Ollama | 20-50ms | Free | ✅ | ⭐⭐⭐⭐ | Medium |
| ONNX | 1-10ms | Free | ✅ | ⭐⭐⭐⭐ | Hard |

### When to Use Each

**OpenAI**:
- ✅ Production applications
- ✅ Need highest quality
- ✅ Don't mind API costs
- ❌ Privacy concerns
- ❌ Need low latency

**HuggingFace**:
- ✅ Experimenting with models
- ✅ Free tier sufficient
- ✅ Open-source preference
- ❌ Privacy concerns
- ❌ Need low latency

**Ollama**:
- ✅ Privacy-sensitive data
- ✅ Low latency requirements
- ✅ Local development
- ✅ Cost optimization
- ❌ Need absolute best quality

**ONNX**:
- ✅ Maximum performance
- ✅ Complete control
- ✅ Embedded systems
- ✅ High-volume processing
- ❌ Quick setup needed

## Best Practices

### 1. Choose the Right Provider

```rust
// Development: Use Ollama (fast, free, local)
#[cfg(debug_assertions)]
let config = OllamaConfig::nomic_embed_text();

// Production: Use OpenAI (best quality)
#[cfg(not(debug_assertions))]
let config = OpenAIConfig::from_env(OpenAIModel::TextEmbedding3Small)?;
```

### 2. Always Use Batching

```rust
// ❌ Bad: One request per document
for doc in documents {
    let embedding = service.embed(doc).await?;
}

// ✅ Good: Single batch request
let embeddings = service.embed_batch(&documents).await?;
```

### 3. Handle Errors Gracefully

```rust
use std::time::Duration;

// Retry with exponential backoff for rate limits
let mut retries = 0;
let embedding = loop {
    match service.embed(text).await {
        Ok(emb) => break emb,
        Err(EmbeddingError::RateLimitExceeded { retry_after, .. }) => {
            if retries >= 3 {
                return Err("Max retries exceeded".into());
            }
            tokio::time::sleep(retry_after.unwrap_or(Duration::from_secs(1))).await;
            retries += 1;
        }
        Err(e) => return Err(e.into()),
    }
};
```

### 4. Cache Embeddings

```rust
use std::collections::HashMap;
use std::sync::Arc;

struct EmbeddingCache {
    service: EmbeddingService,
    cache: HashMap<String, Arc<Vec<f32>>>,
}

impl EmbeddingCache {
    async fn embed(&mut self, text: &str) -> Result<Arc<Vec<f32>>, EmbeddingError> {
        if let Some(embedding) = self.cache.get(text) {
            return Ok(Arc::clone(embedding));
        }

        let embedding = self.service.embed(text).await?;
        let arc_embedding = Arc::new(embedding);
        self.cache.insert(text.to_string(), Arc::clone(&arc_embedding));
        Ok(arc_embedding)
    }
}
```

### 5. Monitor Costs

```rust
use std::sync::atomic::{AtomicUsize, Ordering};

struct CostTracker {
    service: EmbeddingService,
    requests: AtomicUsize,
}

impl CostTracker {
    async fn embed(&self, text: &str) -> Result<Vec<f32>> {
        let embedding = self.service.embed(text).await?;
        self.requests.fetch_add(1, Ordering::Relaxed);
        Ok(embedding)
    }

    fn estimated_cost(&self) -> f64 {
        let requests = self.requests.load(Ordering::Relaxed);
        // OpenAI: $0.02 per 1M tokens (~0.00002 per request)
        requests as f64 * 0.00002
    }
}
```

## Troubleshooting

### OpenAI: Authentication Failed

```
Error: OpenAI authentication failed: Invalid API key
```

**Solution**: Check your API key is set correctly:
```bash
echo $OPENAI_API_KEY
export OPENAI_API_KEY=sk-...
```

### HuggingFace: Model Not Found

```
Error: Model 'wrong-model' not found in HuggingFace
```

**Solution**: Verify the model exists on HuggingFace Hub and the ID is correct.

### Ollama: Cannot Connect

```
Error: Cannot connect to Ollama at http://localhost:11434. Is Ollama running?
```

**Solution**:
1. Check Ollama is installed: `ollama --version`
2. Check Ollama is running: `ollama list`
3. Pull the model: `ollama pull nomic-embed-text`

### ONNX: Model Load Error

```
Error: Failed to load model 'models/model.onnx'
```

**Solution**: ONNX provider is currently a placeholder. Full implementation pending.

### Dimension Mismatch

```
Error: Expected 384 dimensions, got 768
```

**Solution**: Ensure the configured dimensions match your model:
```rust
// ❌ Wrong
let config = HuggingFaceConfig::new(
    token,
    "sentence-transformers/all-mpnet-base-v2".to_string(),
    384  // Wrong! This model is 768
);

// ✅ Correct
let config = HuggingFaceConfig::all_mpnet_base_v2()?;  // Correct dimensions
```

---

## See Also

- [Architecture Decision Record](./adr/0016-embedding-providers.md) - Design decisions
- [Examples](../examples/) - Runnable code examples
- [Vector Search Design](./VECTOR_SEARCH_DESIGN.md) - Overall vector search architecture
- [CLAUDE.md](../CLAUDE.md) - Development guidelines
