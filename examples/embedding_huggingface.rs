//! Example: Using HuggingFace embeddings with AletheiaDB
//!
//! This example demonstrates using the HuggingFace Inference API for embeddings.
//!
//! # Setup
//!
//! Set your HuggingFace token:
//! ```bash
//! export HF_TOKEN=hf_...
//! ```
//!
//! # Run
//!
//! ```bash
//! cargo run --example embedding_huggingface --features embedding-huggingface
//! ```

#![cfg(feature = "embedding-huggingface")]

use aletheiadb::embeddings::EmbeddingService;
use aletheiadb::embeddings::providers::huggingface::*;
use aletheiadb::{AletheiaDB, PropertyMapBuilder};
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("🤗 HuggingFace Embeddings Example\n");

    // 1. Setup HuggingFace provider with all-MiniLM model
    println!("📝 Setting up HuggingFace provider...");
    let config = HuggingFaceConfig::all_minilm_l6_v2()?;
    let provider = Arc::new(HuggingFaceProvider::new(config)?);
    let embedding_service = EmbeddingService::new(provider);

    println!("✅ Provider: {}", embedding_service.provider_name());
    println!("✅ Dimensions: {}\n", embedding_service.dimensions());

    // 2. Generate embeddings for technical documents
    println!("🔮 Generating embeddings...");
    let documents = vec![
        "Rust is a systems programming language",
        "Python is great for data science",
        "JavaScript powers the web",
    ];

    let embeddings = embedding_service.embed_batch(&documents).await?;
    println!("✅ Generated {} embeddings\n", embeddings.len());

    // 3. Store in AletheiaDB
    println!("💾 Storing in AletheiaDB...");
    let db = AletheiaDB::new()?;

    for (doc, embedding) in documents.iter().zip(embeddings.iter()) {
        db.create_node(
            "TechDoc",
            PropertyMapBuilder::new()
                .insert("content", *doc)
                .insert_vector("embedding", embedding)
                .build(),
        )?;
        println!("✅ Stored: {}", doc);
    }

    println!("\n✨ Example complete!");
    Ok(())
}

#[cfg(not(feature = "embedding-huggingface"))]
fn main() {
    eprintln!("This example requires the 'embedding-huggingface' feature.");
    eprintln!(
        "Run with: cargo run --example embedding_huggingface --features embedding-huggingface"
    );
    std::process::exit(1);
}
