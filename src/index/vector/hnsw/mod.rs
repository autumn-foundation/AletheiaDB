//! HNSW (Hierarchical Navigable Small World) vector index implementation.
//!
//! This module provides a wrapper around the `usearch` library's HNSW index,
//! implementing the `VectorIndex` trait for approximate k-nearest neighbor search.

pub mod builder;
pub mod config;
pub mod index;
mod storage;

pub use builder::HnswIndexBuilder;
pub use config::HnswConfig;
pub use index::HnswIndex;
