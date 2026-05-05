//! Example: local embeddings through embed_anything.
//!
//! The `embedding-ollama` feature is retained as a compatibility alias for
//! downstream Cargo manifests. Provider ownership now lives in embed_anything;
//! this example uses its local Hugging Face/Candle path.
//!
//! Run with:
//! `cargo run --example embedding_ollama --features embedding-ollama`

use aletheiadb::embeddings::{EmbedderBuilder, embed_data_to_dense_iter, embed_query};
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
    let data = embed_query(&documents, &embedder, None).await?;

    let db = AletheiaDB::new()?;
    for item in embed_data_to_dense_iter(data, None) {
        let item = item?;
        if let Some(text) = item.text {
            db.create_node(
                "Document",
                PropertyMapBuilder::new()
                    .insert("text", text)
                    .insert_vector("embedding", &item.embedding)
                    .build(),
            )?;
        }
    }

    Ok(())
}
