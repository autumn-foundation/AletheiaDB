//! Optional embedding generation via [`embed_anything`].
//!
//! AletheiaDB stores and indexes vector properties; it does not maintain
//! provider-specific embedding clients. Enable the `embeddings` feature to use
//! and re-export `embed_anything`, then store the resulting dense vectors with
//! `PropertyMapBuilder::insert_vector()`.

use std::{collections::HashMap, error::Error, fmt};

pub use embed_anything;

pub use embed_anything::{
    Dtype, FileLoadingError, embed_directory_stream, embed_file, embed_files_batch,
    embed_image_directory, embed_query, embed_webpage, process_chunks,
};

pub use embed_anything::config::{ImageEmbedConfig, SplittingStrategy, TextEmbedConfig};

pub use embed_anything::embeddings::embed::{
    EmbedData, Embedder, EmbedderBuilder, EmbeddingResult,
};

/// Dense embedding data with chunk text and metadata preserved.
#[derive(Debug, Clone, PartialEq)]
pub struct DenseEmbedData {
    /// Text associated with the embedding chunk, when provided upstream.
    pub text: Option<String>,
    /// Metadata associated with the embedding chunk, when provided upstream.
    pub metadata: Option<HashMap<String, String>>,
    /// Dense vector representation suitable for AletheiaDB vector storage.
    pub embedding: Vec<f32>,
}

/// Error returned when an upstream embedding result is not dense.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum DenseEmbeddingError {
    /// A multi-vector embedding cannot be represented as a single dense vector.
    NotDense,
}

impl fmt::Display for DenseEmbeddingError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotDense => formatter.write_str("embedding result is not a dense vector"),
        }
    }
}

impl Error for DenseEmbeddingError {}

/// Converts upstream embedding results into dense vectors lazily.
///
/// When `limit` is provided, at most that many results are converted.
pub fn to_dense_iter<I>(
    results: I,
    limit: Option<usize>,
) -> impl Iterator<Item = Result<Vec<f32>, DenseEmbeddingError>>
where
    I: IntoIterator<Item = EmbeddingResult>,
{
    results
        .into_iter()
        .take(limit.unwrap_or(usize::MAX))
        .map(embedding_result_to_dense)
}

/// Converts upstream `EmbedData` values into dense vectors while preserving
/// chunk text and metadata.
///
/// When `limit` is provided, at most that many chunks are converted.
pub fn embed_data_to_dense_iter<I>(
    data: I,
    limit: Option<usize>,
) -> impl Iterator<Item = Result<DenseEmbedData, DenseEmbeddingError>>
where
    I: IntoIterator<Item = EmbedData>,
{
    data.into_iter()
        .take(limit.unwrap_or(usize::MAX))
        .map(embed_data_to_dense)
}

fn embed_data_to_dense(data: EmbedData) -> Result<DenseEmbedData, DenseEmbeddingError> {
    let embedding = embedding_result_to_dense(data.embedding)?;

    Ok(DenseEmbedData {
        text: data.text,
        metadata: data.metadata,
        embedding,
    })
}

fn embedding_result_to_dense(result: EmbeddingResult) -> Result<Vec<f32>, DenseEmbeddingError> {
    match result {
        EmbeddingResult::DenseVector(vector) => Ok(vector),
        EmbeddingResult::MultiVector(_) => Err(DenseEmbeddingError::NotDense),
    }
}
