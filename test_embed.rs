// ⚠️ REQUIRES FEATURE: embedding-openai
// [dependencies]
// aletheiadb = { version = "0.1", features = ["embedding-openai"] }
use aletheiadb::{AletheiaDB, properties};
use aletheiadb::embeddings::{EmbeddingService, providers::openai::*};
use std::sync::Arc;

// Note: Requires `tokio` dependency in Cargo.toml
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Enable in Cargo.toml: features = ["embedding-openai"]

    // 1. Create embedding service
    let config = OpenAIConfig::from_env(OpenAIModel::TextEmbedding3Small)?;
    let provider = Arc::new(OpenAIProvider::new(config)?);
    let service = EmbeddingService::new(provider);

    // 2. Generate embeddings
    let documents = vec![
        "AletheiaDB is a bi-temporal graph database",
        "It tracks both valid time and transaction time",
    ];
    let embeddings = service.embed_batch(&documents).await?;

    // 3. Store with vectors
    let db = AletheiaDB::new()?;
    for (text, embedding) in documents.iter().zip(embeddings.iter()) {
        db.create_node(
            "Document",
            properties! {
                "content" => *text,
                "embedding" => &embedding[..],
            }
        )?;
    }

    Ok(())
}
