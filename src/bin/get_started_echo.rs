use aletheiadb::{AletheiaDB, properties};
use aletheiadb::embeddings::{EmbeddingService, providers::openai::*};
use std::sync::Arc;

#[tokio::main]
async fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    let config = OpenAIConfig::from_env(OpenAIModel::TextEmbedding3Small)?;
    let provider = Arc::new(OpenAIProvider::new(config)?);
    let service = EmbeddingService::new(provider);

    let documents = vec![
        "AletheiaDB is a bi-temporal graph database",
        "It tracks both valid time and transaction time",
    ];
    let embeddings = service.embed_batch(&documents).await?;

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
