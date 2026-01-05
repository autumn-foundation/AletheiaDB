//! Example: Using Ollama local embeddings with GallifreyDB
//!
//! This example demonstrates using Ollama for local embedding generation.
//!
//! # Setup
//!
//! 1. Install Ollama: https://ollama.ai
//! 2. Pull a model: `ollama pull nomic-embed-text`
//! 3. Ensure Ollama is running (starts automatically on macOS/Linux)
//!
//! # Run
//!
//! ```bash
//! cargo run --example embedding_ollama --features embedding-ollama
//! ```

use gallifreydb::embeddings::providers::ollama::*;
use gallifreydb::embeddings::EmbeddingService;
use gallifreydb::{GallifreyDB, PropertyMapBuilder};
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("🦙 Ollama Embeddings Example\n");

    // 1. Setup Ollama provider
    println!("📝 Setting up Ollama provider...");
    let config = OllamaConfig::nomic_embed_text();
    let provider = Arc::new(OllamaProvider::new(config)?);
    let embedding_service = EmbeddingService::new(provider);

    println!("✅ Provider: {}", embedding_service.provider_name());
    println!("✅ Dimensions: {}", embedding_service.dimensions());
    println!("✅ Local inference (no API key needed!)\n");

    // 2. Generate embeddings
    println!("🔮 Generating embeddings locally...");
    let documents = vec![
        "The quick brown fox jumps over the lazy dog",
        "Machine learning is transforming technology",
        "Graph databases excel at relationship queries",
    ];

    let start = std::time::Instant::now();
    let embeddings = embedding_service.embed_batch(&documents).await?;
    let duration = start.elapsed();

    println!("✅ Generated {} embeddings", embeddings.len());
    println!("⏱️  Time: {:?} ({:.1}ms per doc)\n", duration, duration.as_millis() as f64 / documents.len() as f64);

    // 3. Store in GallifreyDB
    println!("💾 Storing in GallifreyDB...");
    let db = GallifreyDB::new();

    for (doc, embedding) in documents.iter().zip(embeddings.iter()) {
        db.create_node(
            "Document",
            PropertyMapBuilder::new()
                .insert("text", *doc)
                .insert_vector("embedding", embedding)
                .build(),
        )?;
        println!("✅ Stored: {}", doc);
    }

    println!("\n✨ Example complete!");
    println!("💡 Tip: Ollama runs locally - no data leaves your machine!");
    Ok(())
}
