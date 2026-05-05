//! Example: ONNX embeddings through embed_anything.
//!
//! Set `EMBED_ANYTHING_ONNX_PATH` to the model path inside the Hugging Face
//! repository before running.
//!
//! Run with:
//! `cargo run --example embedding_onnx --features embedding-onnx`

#![cfg(feature = "embedding-onnx")]

use aletheiadb::embeddings::{Dtype, Embedder, EmbeddingResult};
use aletheiadb::{AletheiaDB, PropertyMapBuilder};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let model_id = std::env::var("EMBED_ANYTHING_ONNX_MODEL")
        .unwrap_or_else(|_| "sentence-transformers/all-MiniLM-L6-v2".to_string());
    let path_in_repo = std::env::var("EMBED_ANYTHING_ONNX_PATH")?;

    let embedder = Embedder::from_pretrained_onnx(
        "bert",
        None,
        None,
        Some(&model_id),
        Some(Dtype::F32),
        Some(&path_in_repo),
    )?;

    let documents = [
        "ONNX enables cross-platform inference",
        "Local models provide privacy and low latency",
        "Embedding models can run on CPU or GPU",
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

#[cfg(not(feature = "embedding-onnx"))]
fn main() {
    eprintln!("This example requires the 'embedding-onnx' feature.");
    std::process::exit(1);
}
