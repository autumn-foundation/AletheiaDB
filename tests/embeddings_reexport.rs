#![cfg(feature = "embeddings")]

use std::collections::HashMap;

use aletheiadb::embeddings::{
    DenseEmbeddingError, EmbedData, EmbeddingResult, embed_anything, embed_data_to_dense_iter,
    to_dense_iter,
};

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

#[test]
fn to_dense_iter_respects_limit() {
    let results = vec![
        EmbeddingResult::from(vec![1.0_f32, 0.0]),
        EmbeddingResult::from(vec![0.0_f32, 1.0]),
    ];

    let dense = to_dense_iter(results, Some(1))
        .collect::<Result<Vec<_>, _>>()
        .unwrap();

    assert_eq!(dense, vec![vec![1.0_f32, 0.0]]);
}

#[test]
fn to_dense_iter_rejects_multi_vectors() {
    let results = vec![EmbeddingResult::from(vec![vec![1.0_f32, 0.0]])];

    let error = to_dense_iter(results, None)
        .collect::<Result<Vec<_>, _>>()
        .unwrap_err();

    assert_eq!(error, DenseEmbeddingError);
}

#[test]
fn embed_data_to_dense_iter_preserves_text_and_metadata() {
    let mut metadata = HashMap::new();
    metadata.insert("source".to_string(), "doc-1".to_string());
    let data = vec![EmbedData::new(
        EmbeddingResult::from(vec![0.25_f32, 0.75]),
        Some("chunk text".to_string()),
        Some(metadata),
    )];

    let dense = embed_data_to_dense_iter(data, None)
        .collect::<Result<Vec<_>, _>>()
        .unwrap();

    assert_eq!(dense[0].text.as_deref(), Some("chunk text"));
    assert_eq!(
        dense[0]
            .metadata
            .as_ref()
            .and_then(|metadata| metadata.get("source"))
            .map(String::as_str),
        Some("doc-1")
    );
    assert_eq!(dense[0].embedding, vec![0.25_f32, 0.75]);
}
