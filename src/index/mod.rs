//! Indexing subsystem for fast queries.
//!
//! This module contains various index structures for efficient graph queries:
//! - Current indexes: For fast current-state queries (hot path)
//! - Adjacency indexes: CSR format for cache-friendly traversals
//! - Temporal indexes: B-Tree indexes for historical queries
//! - Vector indexes: Approximate nearest neighbor search for embeddings

pub mod adjacency;
pub mod current;
pub mod temporal;
pub mod vector;

// Re-export commonly used types
pub use adjacency::{AdjacencyEntry, AdjacencyIndex};
pub use current::CurrentIndexes;
pub use temporal::TemporalIndexes;
pub use vector::{DistanceMetric, VectorIndex};
