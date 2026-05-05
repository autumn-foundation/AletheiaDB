//! Example: local embeddings through embed_anything.
//!
//! The `embedding-ollama` feature is retained as a compatibility alias for
//! downstream Cargo manifests. Provider ownership now lives in embed_anything;
//! this example uses its local Hugging Face/Candle path.
//!
//! Run with:
//! `cargo run --example embedding_ollama --features embedding-ollama`

#![cfg(feature = "embedding-ollama")]

use aletheiadb::embeddings::{EmbedderBuilder, EmbeddingResult};
use aletheiadb::{AletheiaDB, PropertyMapBuilder};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let embedder = EmbedderBuilder::new()
        .model_architecture("jina")
        .model_id(Some("jinaai/jina-embeddings-v2-small-en"))
        .from_pretrained_hf()?;

    let documents = [
        "The quick brown fox jumps over the lazy dog",
        "Machine learning is transforming technology",
        "Graph databases excel at relationship queries",
    ];
    let embeddings = dense_embeddings(embedder.embed(&documents, Some(32), None).await?)?;

    let db = AletheiaDB::new()?;
    for (doc, embedding) in documents.iter().zip(embeddings.iter()) {
        db.create_node(
            "Document",
            PropertyMapBuilder::new()
                .insert("text", *doc)
                .insert_vector("embedding", embedding)
                .build(),
        )?;
    }

    Ok(())
}

fn dense_embeddings(
    results: Vec<EmbeddingResult>,
) -> Result<Vec<Vec<f32>>, Box<dyn std::error::Error>> {
    results
        .iter()
        .map(EmbeddingResult::to_dense)
        .collect::<Result<Vec<_>, _>>()
        .map_err(Into::into)
}

#[cfg(not(feature = "embedding-ollama"))]
fn main() {
    eprintln!("This example requires the 'embedding-ollama' feature.");
    std::process::exit(1);
}
