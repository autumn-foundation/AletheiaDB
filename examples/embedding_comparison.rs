//! Example: compare embed_anything construction paths.
//!
//! Run with:
//! `cargo run --example embedding_comparison --features embeddings`

use aletheiadb::embeddings::{Embedder, EmbedderBuilder, embed_data_to_dense_iter, embed_query};
use std::time::{Duration, Instant};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let text = "AletheiaDB is a high-performance bi-temporal graph database";

    if let Ok(api_key) = std::env::var("OPENAI_API_KEY") {
        let embedder =
            Embedder::from_pretrained_cloud("OpenAI", "text-embedding-3-small", Some(api_key))?;
        report("OpenAI", time_embed(&embedder, text).await?);
    }

    let hf = EmbedderBuilder::new()
        .model_architecture("bert")
        .model_id(Some("sentence-transformers/all-MiniLM-L6-v2"))
        .from_pretrained_hf()?;
    report("Hugging Face local", time_embed(&hf, text).await?);

    Ok(())
}

async fn time_embed(
    embedder: &Embedder,
    text: &str,
) -> Result<(Duration, usize), Box<dyn std::error::Error>> {
    let start = Instant::now();
    let data = embed_query(&[text], embedder, None).await?;
    let dimensions = embed_data_to_dense_iter(data, Some(1))
        .next()
        .transpose()?
        .map(|item| item.embedding.len())
        .unwrap_or_default();

    Ok((start.elapsed(), dimensions))
}

fn report(name: &str, (duration, dimensions): (Duration, usize)) {
    println!("{name}: {duration:?}, {dimensions} dimensions");
}
