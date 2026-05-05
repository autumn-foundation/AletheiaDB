//! Example: OpenAI embeddings through embed_anything.
//!
//! Run with:
//! `cargo run --example embedding_openai --features embedding-openai`

#![cfg(feature = "embedding-openai")]

use aletheiadb::embeddings::{Embedder, EmbeddingResult};
use aletheiadb::{AletheiaDB, PropertyMapBuilder};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let api_key = std::env::var("OPENAI_API_KEY")?;
    let embedder =
        Embedder::from_pretrained_cloud("OpenAI", "text-embedding-3-small", Some(api_key))?;

    let documents = [
        "AletheiaDB is a bi-temporal graph database",
        "Vector embeddings enable semantic search",
        "Time travel queries show historical data",
    ];
    let embeddings = dense_embeddings(embedder.embed(&documents, Some(32), None).await?)?;

    let db = AletheiaDB::new()?;
    for (doc, embedding) in documents.iter().zip(embeddings.iter()) {
        db.create_node(
            "Document",
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

#[cfg(not(feature = "embedding-openai"))]
fn main() {
    eprintln!("This example requires the 'embedding-openai' feature.");
    std::process::exit(1);
}
