//! Example: ONNX embeddings through embed_anything.
//!
//! Set `EMBED_ANYTHING_ONNX_PATH` to the model path inside the Hugging Face
//! repository before running.
//!
//! Run with:
//! `cargo run --example embedding_onnx --features embedding-onnx`

use aletheiadb::embeddings::{Dtype, Embedder, embed_data_to_dense_iter, embed_query};
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
