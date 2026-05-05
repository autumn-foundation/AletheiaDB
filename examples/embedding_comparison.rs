//! Example: compare embed_anything construction paths.
//!
//! Run with:
//! `cargo run --example embedding_comparison --features embedding-all`

#![cfg(feature = "embedding-all")]

use aletheiadb::embeddings::{Embedder, EmbedderBuilder, EmbeddingResult};
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
    let results = embedder.embed(&[text], Some(1), None).await?;
    let dims = dense_embeddings(results)?
        .first()
        .map(Vec::len)
        .unwrap_or_default();

    Ok((start.elapsed(), dims))
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

fn report(name: &str, (duration, dimensions): (Duration, usize)) {
    println!("{name}: {duration:?}, {dimensions} dimensions");
}

#[cfg(not(feature = "embedding-all"))]
fn main() {
    eprintln!("This example requires the 'embedding-all' feature.");
}
