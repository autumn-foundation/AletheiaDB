//! Example: local Hugging Face embeddings through embed_anything.
//!
//! Run with:
//! `cargo run --example embedding_huggingface --features embedding-huggingface`

#![cfg(feature = "embedding-huggingface")]

use aletheiadb::embeddings::{EmbedderBuilder, EmbeddingResult};
use aletheiadb::{AletheiaDB, PropertyMapBuilder};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let embedder = EmbedderBuilder::new()
        .model_architecture("bert")
        .model_id(Some("sentence-transformers/all-MiniLM-L6-v2"))
        .from_pretrained_hf()?;

    let documents = [
        "Rust is a systems programming language",
        "Python is common in data science",
        "JavaScript powers browser applications",
    ];
    let embeddings = dense_embeddings(embedder.embed(&documents, Some(32), None).await?)?;

    let db = AletheiaDB::new()?;
    for (doc, embedding) in documents.iter().zip(embeddings.iter()) {
        db.create_node(
            "TechDoc",
            PropertyMapBuilder::new()
                .insert("content", *doc)
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

#[cfg(not(feature = "embedding-huggingface"))]
fn main() {
    eprintln!("This example requires the 'embedding-huggingface' feature.");
    std::process::exit(1);
}
