//! Optional embedding generation via [`embed_anything`].
//!
//! AletheiaDB stores and indexes vector properties; it does not maintain
//! provider-specific embedding clients. Enable the `embeddings` feature to use
//! and re-export `embed_anything`, then store the resulting dense vectors with
//! `PropertyMapBuilder::insert_vector()`.

pub use embed_anything;

pub use embed_anything::{
    Dtype, FileLoadingError, embed_directory_stream, embed_file, embed_files_batch,
    embed_image_directory, embed_query, embed_webpage, process_chunks,
};

pub use embed_anything::config::{ImageEmbedConfig, SplittingStrategy, TextEmbedConfig};

pub use embed_anything::embeddings::embed::{
    EmbedData, Embedder, EmbedderBuilder, EmbeddingResult,
};
