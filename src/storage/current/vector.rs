//! Manages in-memory vector embeddings for the current state.

use crate::index::vector::DistanceMetric;
use crate::index::vector::hnsw::{HnswConfig, HnswIndex};
use crate::index::vector::temporal::{TemporalVectorConfig, TemporalVectorIndex};
use std::sync::Arc;

/// Entry for a single vector index on a specific property.
///
/// Each property can have its own HNSW index with independent configuration
/// (dimensions, distance metric, etc.). This enables multi-property vector
/// indexing for use cases like separate title/body embeddings.
pub(crate) struct VectorIndexEntry {
    /// The HNSW index for this property
    pub(crate) index: Arc<HnswIndex>,
    /// Configuration used to create this index
    pub(crate) config: HnswConfig,
}

/// Information about a configured vector index.
///
/// Returned by [`crate::storage::CurrentStorage::list_vector_indexes`] to provide
/// metadata about all enabled vector indexes.
#[derive(Debug, Clone)]
pub struct VectorIndexInfo {
    /// The property name this index is configured for
    pub property_name: String,
    /// Number of dimensions in the vectors
    pub dimensions: usize,
    /// Distance metric used for similarity calculations
    pub distance_metric: DistanceMetric,
}

/// Entry for a temporal vector index (multi-property support).
pub(crate) struct TemporalVectorIndexEntry {
    /// The temporal vector index for this property
    pub(crate) index: Arc<TemporalVectorIndex>,
    /// Configuration used to create this index
    #[allow(dead_code)]
    pub(crate) config: TemporalVectorConfig,
}
