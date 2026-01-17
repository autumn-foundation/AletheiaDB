//! Example: Using OpenAI embeddings with GallifreyDB
//!
//! This example demonstrates how to:
//! 1. Configure the OpenAI provider
//! 2. Generate embeddings from text
//! 3. Store embeddings in GallifreyDB
//! 4. Perform similarity search
//!
//! # Setup
//!
//! Set your OpenAI API key:
//! ```bash
//! export OPENAI_API_KEY=sk-...
//! ```
//!
//! # Run
//!
//! ```bash
//! cargo run --example embedding_openai --features embedding-openai
//! ```

#![cfg(feature = "embedding-openai")]

use gallifreydb::embeddings::EmbeddingService;
use gallifreydb::embeddings::providers::openai::*;
use gallifreydb::{GallifreyDB, PropertyMapBuilder};
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("🚀 OpenAI Embeddings Example\n");

    // 1. Setup OpenAI provider
    println!("📝 Setting up OpenAI provider...");
    let config = OpenAIConfig::from_env(OpenAIModel::TextEmbedding3Small)?;
    let provider = Arc::new(OpenAIProvider::new(config)?);
    let embedding_service = EmbeddingService::new(provider);

    println!("✅ Provider: {}", embedding_service.provider_name());
    println!("✅ Dimensions: {}\n", embedding_service.dimensions());

    // 2. Generate embeddings
    println!("🔮 Generating embeddings...");
    let documents = vec![
        "GallifreyDB is a bi-temporal graph database",
        "Vector embeddings enable semantic search",
        "Time travel queries show historical data",
    ];

    let embeddings = embedding_service.embed_batch(&documents).await?;
    println!("✅ Generated {} embeddings\n", embeddings.len());

    // 3. Store in GallifreyDB
    println!("💾 Storing in GallifreyDB...");
    let db = GallifreyDB::new();

    let mut node_ids = Vec::new();
    for (doc, embedding) in documents.iter().zip(embeddings.iter()) {
        let node_id = db.create_node(
            "Document",
            PropertyMapBuilder::new()
                .insert("content", *doc)
                .insert_vector("embedding", embedding)
                .build(),
        )?;
        node_ids.push(node_id);
        println!("✅ Stored: {}", doc);
    }

    // 4. Query similar documents
    println!("\n🔍 Finding similar documents...");
    let query = "What is GallifreyDB?";
    let query_embedding = embedding_service.embed(query).await?;

    println!("Query: {}", query);

    // Manual similarity calculation (since HNSW has compilation errors)
    use gallifreydb::core::vector::cosine_similarity;

    let mut similarities: Vec<(usize, f32)> = embeddings
        .iter()
        .enumerate()
        .map(|(i, emb)| {
            let sim = cosine_similarity(&query_embedding, emb).unwrap_or(0.0);
            (i, sim)
        })
        .collect();

    similarities.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());

    println!("\nTop 3 matches:");
    for (i, (doc_idx, similarity)) in similarities.iter().take(3).enumerate() {
        println!(
            "{}. {} (similarity: {:.3})",
            i + 1,
            documents[*doc_idx],
            similarity
        );
    }

    println!("\n✨ Example complete!");
    Ok(())
}

#[cfg(not(feature = "embedding-openai"))]
fn main() {
    eprintln!("This example requires the 'embedding-openai' feature.");
    eprintln!("Run with: cargo run --example embedding_openai --features embedding-openai");
    std::process::exit(1);
}
