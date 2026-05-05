//! Example: local Hugging Face embeddings through embed_anything.
//!
//! Run with:
//! `cargo run --example embedding_huggingface --features embedding-huggingface`

use aletheiadb::embeddings::{EmbedderBuilder, embed_data_to_dense_iter, embed_query};
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
    let data = embed_query(&documents, &embedder, None).await?;

    let db = AletheiaDB::new()?;
    for item in embed_data_to_dense_iter(data, None) {
        let item = item?;
        if let Some(content) = item.text {
            db.create_node(
                "TechDoc",
                PropertyMapBuilder::new()
                    .insert("content", content)
                    .insert_vector("embedding", &item.embedding)
                    .build(),
            )?;
        }
    }

    Ok(())
}
