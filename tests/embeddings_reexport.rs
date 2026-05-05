#![cfg(feature = "embeddings")]

use aletheiadb::embeddings::{EmbedData, EmbeddingResult, embed_anything};

#[test]
fn reexports_embed_anything_types() {
    let embedding = EmbeddingResult::from(vec![1.0_f32, 0.0, 0.0]);
    let data = EmbedData::new(embedding.clone(), Some("hello".to_string()), None);

    assert_eq!(embedding.to_dense().unwrap(), vec![1.0_f32, 0.0, 0.0]);
    assert_eq!(data.text.as_deref(), Some("hello"));
}

#[test]
fn reexports_embed_anything_crate_namespace() {
    let embedding = embed_anything::embeddings::embed::EmbeddingResult::from(vec![0.5_f32, 0.5]);

    assert_eq!(embedding.to_dense().unwrap(), vec![0.5_f32, 0.5]);
}
