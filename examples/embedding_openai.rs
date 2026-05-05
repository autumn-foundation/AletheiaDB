//! Example: OpenAI embeddings through embed_anything.
//!
//! Run with:
//! `cargo run --example embedding_openai --features embeddings`

use aletheiadb::embeddings::{Embedder, embed_data_to_dense_iter, embed_query};
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
    let data = embed_query(&documents, &embedder, None).await?;

    let db = AletheiaDB::new()?;
    for item in embed_data_to_dense_iter(data, None) {
        let item = item?;
        if let Some(content) = item.text {
            db.create_node(
                "Document",
                PropertyMapBuilder::new()
                    .insert("content", content)
                    .insert_vector("embedding", &item.embedding)
                    .build(),
            )?;
        }
    }

    Ok(())
}
