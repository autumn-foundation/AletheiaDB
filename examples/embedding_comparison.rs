//! Example: Comparing different embedding providers
//!
//! This example demonstrates using multiple embedding providers and
//! comparing their performance characteristics.
//!
//! # Setup
//!
//! Set API keys:
//! ```bash
//! export OPENAI_API_KEY=sk-...
//! export HF_TOKEN=hf_...
//! ```
//!
//! Install Ollama and pull a model:
//! ```bash
//! ollama pull nomic-embed-text
//! ```
//!
//! # Run
//!
//! ```bash
//! cargo run --example embedding_comparison --features embedding-all
//! ```

use gallifreydb::embeddings::EmbeddingService;
use gallifreydb::embeddings::providers::{huggingface::*, ollama::*, onnx::*, openai::*};
use std::sync::Arc;
use std::time::Instant;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("🔬 Embedding Provider Comparison\n");
    println!("=".repeat(60));

    let test_text = "GallifreyDB is a high-performance bi-temporal graph database";

    // Test OpenAI (if available)
    println!("\n📊 OpenAI (text-embedding-3-small)");
    println!("{}", "-".repeat(60));
    match test_provider_openai(test_text).await {
        Ok((latency, dims)) => {
            println!("✅ Latency: {:?}", latency);
            println!("✅ Dimensions: {}", dims);
            println!("💰 Cost: ~$0.00002 per request");
        }
        Err(e) => println!("❌ Error: {}", e),
    }

    // Test HuggingFace (if available)
    println!("\n📊 HuggingFace (all-MiniLM-L6-v2)");
    println!("{}", "-".repeat(60));
    match test_provider_huggingface(test_text).await {
        Ok((latency, dims)) => {
            println!("✅ Latency: {:?}", latency);
            println!("✅ Dimensions: {}", dims);
            println!("💰 Cost: Free tier available");
        }
        Err(e) => println!("❌ Error: {}", e),
    }

    // Test Ollama (if available)
    println!("\n📊 Ollama (nomic-embed-text)");
    println!("{}", "-".repeat(60));
    match test_provider_ollama(test_text).await {
        Ok((latency, dims)) => {
            println!("✅ Latency: {:?}", latency);
            println!("✅ Dimensions: {}", dims);
            println!("💰 Cost: Free (local)");
            println!("🔒 Privacy: Data never leaves your machine");
        }
        Err(e) => println!("❌ Error: {}", e),
    }

    // Test ONNX (placeholder)
    println!("\n📊 ONNX (all-MiniLM-L6-v2) [Placeholder]");
    println!("{}", "-".repeat(60));
    match test_provider_onnx(test_text).await {
        Ok((latency, dims)) => {
            println!("⚠️  Latency: {:?} (placeholder)", latency);
            println!("⚠️  Dimensions: {} (placeholder)", dims);
            println!("💰 Cost: Free (local)");
        }
        Err(e) => println!("❌ Error: {}", e),
    }

    println!("\n{}", "=".repeat(60));
    println!("\n💡 Recommendations:");
    println!("   • OpenAI: Best quality, paid API, ~100-200ms latency");
    println!("   • HuggingFace: Good quality, free tier, ~200-500ms latency");
    println!("   • Ollama: Good quality, free, local, ~20-50ms latency");
    println!("   • ONNX: Ultra-fast (<10ms), free, local, requires setup");

    Ok(())
}

async fn test_provider_openai(
    text: &str,
) -> Result<(std::time::Duration, usize), Box<dyn std::error::Error>> {
    let config = OpenAIConfig::from_env(OpenAIModel::TextEmbedding3Small)?;
    let provider = Arc::new(OpenAIProvider::new(config)?);
    let service = EmbeddingService::new(provider);

    let start = Instant::now();
    let embedding = service.embed(text).await?;
    let latency = start.elapsed();

    Ok((latency, embedding.len()))
}

async fn test_provider_huggingface(
    text: &str,
) -> Result<(std::time::Duration, usize), Box<dyn std::error::Error>> {
    let config = HuggingFaceConfig::all_minilm_l6_v2()?;
    let provider = Arc::new(HuggingFaceProvider::new(config)?);
    let service = EmbeddingService::new(provider);

    let start = Instant::now();
    let embedding = service.embed(text).await?;
    let latency = start.elapsed();

    Ok((latency, embedding.len()))
}

async fn test_provider_ollama(
    text: &str,
) -> Result<(std::time::Duration, usize), Box<dyn std::error::Error>> {
    let config = OllamaConfig::nomic_embed_text();
    let provider = Arc::new(OllamaProvider::new(config)?);
    let service = EmbeddingService::new(provider);

    let start = Instant::now();
    let embedding = service.embed(text).await?;
    let latency = start.elapsed();

    Ok((latency, embedding.len()))
}

async fn test_provider_onnx(
    text: &str,
) -> Result<(std::time::Duration, usize), Box<dyn std::error::Error>> {
    let config = OnnxConfig::default();
    let provider = Arc::new(OnnxProvider::new(config)?);
    let service = EmbeddingService::new(provider);

    let start = Instant::now();
    let embedding = service.embed(text).await?;
    let latency = start.elapsed();

    Ok((latency, embedding.len()))
}
