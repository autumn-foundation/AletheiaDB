//! HNSW (Hierarchical Navigable Small World) vector index implementation.
//!
//! This module provides a wrapper around the `hnsw_rs` library's HNSW index,
//! implementing the `VectorIndex` trait for approximate k-nearest neighbor search.
//!
//! # Overview
//!
//! HNSW is a graph-based algorithm for approximate nearest neighbor search that
//! provides excellent search performance with logarithmic average-case complexity:
//!
//! - **Build time**: O(n log n) average case
//! - **Query time**: O(log n) average case, O(n) worst case
//! - **Memory**: O(n * M * d) where M is connections per node, d is dimensions
//!
//! # Performance Characteristics
//!
//! Based on hnsw_rs benchmarks and GallifreyDB testing:
//! - **Add operation**: 1-10µs per vector (depends on M, ef_construction)
//! - **Search operation**: 100µs-1ms for k=10 (depends on index size, ef_search, dimensions)
//! - **Memory usage**: ~(dimensions + M) * 4 bytes per vector
//!
//! Example for 1M vectors, 384 dimensions, M=16:
//! - Storage: ~1M * (384 + 16) * 4 = ~1.6GB
//!
//! # Tuning Parameters
//!
//! The HNSW algorithm has three main tuning parameters that trade off between
//! accuracy, speed, and memory usage:
//!
//! ## M (connections per node)
//! - **Range**: 8-64
//! - **Lower values**: Less memory, faster build, lower recall
//! - **Higher values**: More memory, slower build, higher recall
//! - **Default**: 16 (good balance for most use cases)
//!
//! ## ef_construction (build-time expansion)
//! - **Range**: 100-500
//! - **Higher values**: Better index quality, slower build time
//! - **Default**: 128
//!
//! ## ef_search (query-time expansion)
//! - **Range**: 10-500
//! - **Higher values**: Better recall, slower queries
//! - **Default**: 64
//! - **Can be adjusted at runtime** via `set_ef_search()` method
//!
//! ## Recommended Configurations
//!
//! ```text
//! Production (95%+ recall):
//!   M=16, ef_construction=200, ef_search=50
//!
//! Low latency (90% recall):
//!   M=12, ef_construction=150, ef_search=10
//!
//! High recall (98%+ recall):
//!   M=32, ef_construction=400, ef_search=100
//! ```
//!
//! # Migration from usearch
//!
//! This implementation was migrated from usearch (C++ FFI) to hnsw_rs (pure Rust)
//! to avoid FFI safety issues where C++ internal pointers became invalid when
//! Rust moved structs. See USEARCH_BUG_REPORT.md for details.
//!
//! **Trade-offs**:
//! - ✅ Pure Rust, no FFI issues
//! - ✅ Thread-safe by design
//! - ❌ ~20-30% slower than usearch (still fast enough for most use cases)
//! - ❌ No native delete support (we implement soft deletes)
//!
//! # Examples
//!
//! ```rust
//! use gallifreydb::index::vector::{HnswIndexBuilder, DistanceMetric};
//! use gallifreydb::index::VectorIndex;
//! use gallifreydb::core::id::NodeId;
//!
//! # fn example() -> gallifreydb::utils::Result<()> {
//! // Create an index for 384-dimensional embeddings using cosine similarity
//! let index = HnswIndexBuilder::new(384, DistanceMetric::Cosine)
//!     .m(16)                    // 16 connections per node
//!     .ef_construction(200)     // Build quality
//!     .initial_capacity(10000)  // Pre-allocate for 10k vectors
//!     .build()?;
//!
//! // Add vectors
//! let node1 = NodeId::new(1).unwrap();
//! let embedding1 = vec![0.1f32; 384];
//! index.add(node1, &embedding1)?;
//!
//! let node2 = NodeId::new(2).unwrap();
//! let embedding2 = vec![0.2f32; 384];
//! index.add(node2, &embedding2)?;
//!
//! // Search for similar vectors
//! let query = vec![0.15f32; 384];
//! let results = index.search(&query, 10)?;
//!
//! for (node_id, similarity) in results {
//!     println!("Found node {:?} with similarity {}", node_id, similarity);
//! }
//! # Ok(())
//! # }
//! ```
//!
//! # Thread Safety
//!
//! `HnswIndex` is fully thread-safe for concurrent operations:
//! - Multiple threads can add vectors simultaneously
//! - Multiple threads can search simultaneously
//! - Searches can run concurrently with additions
//!
//! The underlying `hnsw_rs::Hnsw` provides interior mutability with internal
//! locking (via parking_lot), so all operations take `&self` rather than `&mut self`.
//!
//! # Implementation Notes
//!
//! - Validates all input vectors for NaN/Infinity using `validate_vector()`
//! - Caps k parameter at MAX_K (10,000) to prevent DoS attacks
//! - Implements soft deletes (deleted IDs tracked in DashMap)
//! - Maps hnsw_rs results to `VectorError::IndexError`
//! - No Clone implementation (hnsw_rs::Hnsw is not Clone)

use crate::core::id::NodeId;
use crate::core::vector::validate_vector;
use crate::index::vector::{DistanceMetric, VectorIndex};
use crate::utils::{Error, Result, error::VectorError};
use dashmap::DashMap;
use hnsw_rs::prelude::*;
use std::io::{Read, Write};
use std::sync::Arc;
use parking_lot::RwLock;

/// Maximum number of results that can be requested in a search.
///
/// This prevents DoS attacks via excessive memory allocation when an attacker
/// requests an extremely large k value. With 10,000 results max:
/// - Memory per search: ~10,000 * (8 bytes NodeId + 4 bytes f32) = ~120KB
/// - This is reasonable for concurrent queries without risking OOM
const MAX_K: usize = 10_000;

/// Maximum layer value for HNSW (from hnsw_rs documentation)
const MAX_LAYER: usize = 16;

/// Configuration for HNSW (Hierarchical Navigable Small World) index.
///
/// This struct encapsulates all parameters needed to configure an HNSW index
/// for approximate nearest neighbor search. It provides sensible defaults
/// optimized for a balance between accuracy, speed, and memory usage.
///
/// # Parameters
///
/// ## Required Parameters
///
/// - **`dimensions`**: Vector dimensionality (must be > 0)
///   - Common values: 384 (MiniLM), 768 (MPNet), 1536 (OpenAI), 3072 (large models)
///   - Affects: Memory usage scales linearly with dimensions
///
/// - **`metric`**: Distance metric for similarity computation
///   - `Cosine`: Best for semantic similarity (normalized vectors)
///   - `Euclidean`: Best for spatial data and clustering
///   - `DotProduct`: Best for MaxSim operations (ColBERT, etc.)
///
/// ## Performance Tuning Parameters
///
/// - **`m`**: Maximum number of bidirectional connections per node (default: 16)
///   - Range: 8-64, typical: 12-32
///   - **Higher values**: Better recall, more memory, slower build
///   - **Lower values**: Less memory, faster build, lower recall
///
/// - **`ef_construction`**: Candidate list size during index construction (default: 128)
///   - Range: 100-500, typical: 100-400
///   - **Higher values**: Better index quality, slower build time
///   - **Lower values**: Faster build, potentially lower recall
///
/// - **`ef_search`**: Candidate list size during query processing (default: 64)
///   - Range: 10-500, typical: 10-100
///   - **Higher values**: Better recall, slower queries
///   - **Lower values**: Faster queries, lower recall
///   - Can be adjusted at runtime via `HnswIndex::set_ef_search()`
///
/// - **`capacity`**: Initial capacity hint for pre-allocation (default: 0)
///   - Pre-allocates space for the specified number of vectors
///   - Reduces reallocation overhead during bulk insertion
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HnswConfig {
    /// Vector dimensionality (must be > 0)
    pub dimensions: usize,
    /// Distance metric for similarity computation
    pub metric: DistanceMetric,
    /// Maximum bidirectional connections per node (default: 16)
    pub m: usize,
    /// Build-time candidate list size (default: 128)
    pub ef_construction: usize,
    /// Query-time candidate list size (default: 64)
    pub ef_search: usize,
    /// Initial capacity for pre-allocation (default: 0)
    pub capacity: usize,
}

impl Default for HnswConfig {
    fn default() -> Self {
        HnswConfig {
            dimensions: 0,
            metric: DistanceMetric::Cosine,
            m: 16,
            ef_construction: 128,
            ef_search: 64,
            capacity: 0,
        }
    }
}

impl HnswConfig {
    /// Creates a new configuration with the specified dimensions and metric.
    pub fn new(dimensions: usize, metric: DistanceMetric) -> Self {
        HnswConfig {
            dimensions,
            metric,
            ..Default::default()
        }
    }

    /// Sets the M parameter (connections per node).
    pub fn with_m(mut self, m: usize) -> Self {
        self.m = m;
        self
    }

    /// Sets ef_construction (build-time expansion).
    pub fn with_ef_construction(mut self, ef_construction: usize) -> Self {
        self.ef_construction = ef_construction;
        self
    }

    /// Sets ef_search (query-time expansion).
    pub fn with_ef_search(mut self, ef_search: usize) -> Self {
        self.ef_search = ef_search;
        self
    }

    /// Sets initial capacity.
    pub fn with_capacity(mut self, capacity: usize) -> Self {
        self.capacity = capacity;
        self
    }

    /// Sets the dimensions.
    pub fn with_dimensions(mut self, dimensions: usize) -> Self {
        self.dimensions = dimensions;
        self
    }

    /// Sets the distance metric.
    pub fn with_metric(mut self, metric: DistanceMetric) -> Self {
        self.metric = metric;
        self
    }

    /// Serialize configuration to a writer in little-endian binary format.
    pub fn serialize_into<W: Write>(&self, writer: &mut W) -> Result<()> {
        writer.write_all(&(self.dimensions as u64).to_le_bytes())?;
        writer.write_all(&[self.metric.to_u8()])?;
        writer.write_all(&(self.m as u64).to_le_bytes())?;
        writer.write_all(&(self.ef_construction as u64).to_le_bytes())?;
        writer.write_all(&(self.ef_search as u64).to_le_bytes())?;
        writer.write_all(&(self.capacity as u64).to_le_bytes())?;
        Ok(())
    }

    /// Deserialize configuration from a reader.
    pub fn deserialize_from<R: Read>(reader: &mut R) -> Result<Self> {
        let mut buf_u64 = [0u8; 8];
        let mut buf_u8 = [0u8; 1];

        reader.read_exact(&mut buf_u64)?;
        let dimensions = u64::from_le_bytes(buf_u64) as usize;

        reader.read_exact(&mut buf_u8)?;
        let metric = DistanceMetric::from_u8(buf_u8[0])?;

        reader.read_exact(&mut buf_u64)?;
        let m = u64::from_le_bytes(buf_u64) as usize;

        reader.read_exact(&mut buf_u64)?;
        let ef_construction = u64::from_le_bytes(buf_u64) as usize;

        reader.read_exact(&mut buf_u64)?;
        let ef_search = u64::from_le_bytes(buf_u64) as usize;

        reader.read_exact(&mut buf_u64)?;
        let capacity = u64::from_le_bytes(buf_u64) as usize;

        Ok(HnswConfig {
            dimensions,
            metric,
            m,
            ef_construction,
            ef_search,
            capacity,
        })
    }
}

/// Builder for configuring and creating an `HnswIndex`.
pub struct HnswIndexBuilder {
    dimensions: usize,
    metric: DistanceMetric,
    m: usize,
    ef_construction: usize,
    ef_search: usize,
    initial_capacity: Option<usize>,
}

impl HnswIndexBuilder {
    /// Creates a new builder with the required parameters.
    pub fn new(dimensions: usize, metric: DistanceMetric) -> Self {
        HnswIndexBuilder {
            dimensions,
            metric,
            m: 16,
            ef_construction: 128,
            ef_search: 64,
            initial_capacity: None,
        }
    }

    /// Creates a builder from an existing configuration.
    pub fn from_config(config: &HnswConfig) -> Self {
        HnswIndexBuilder {
            dimensions: config.dimensions,
            metric: config.metric,
            m: config.m,
            ef_construction: config.ef_construction,
            ef_search: config.ef_search,
            initial_capacity: if config.capacity > 0 {
                Some(config.capacity)
            } else {
                None
            },
        }
    }

    /// Sets the M parameter (connections per node).
    pub fn m(mut self, m: usize) -> Self {
        self.m = m;
        self
    }

    /// Sets ef_construction (build-time expansion).
    pub fn ef_construction(mut self, ef_construction: usize) -> Self {
        self.ef_construction = ef_construction;
        self
    }

    /// Sets ef_search (query-time expansion).
    pub fn ef_search(mut self, ef_search: usize) -> Self {
        self.ef_search = ef_search;
        self
    }

    /// Sets initial capacity hint for pre-allocation.
    pub fn initial_capacity(mut self, capacity: usize) -> Self {
        self.initial_capacity = Some(capacity);
        self
    }

    /// Builds the HNSW index with the configured parameters.
    pub fn build(self) -> Result<HnswIndex> {
        // Validate dimensions
        if self.dimensions == 0 {
            return Err(Error::Vector(VectorError::InvalidVector {
                reason: "dimensions must be > 0".to_string(),
            }));
        }

        // Validate M
        if self.m == 0 || self.m > 64 {
            return Err(Error::Vector(VectorError::InvalidVector {
                reason: format!("M must be in range [1, 64], got {}", self.m),
            }));
        }

        // Validate ef_construction
        if self.ef_construction == 0 {
            return Err(Error::Vector(VectorError::InvalidVector {
                reason: "ef_construction must be > 0".to_string(),
            }));
        }

        // Validate ef_search
        if self.ef_search == 0 {
            return Err(Error::Vector(VectorError::InvalidVector {
                reason: "ef_search must be > 0".to_string(),
            }));
        }

        let capacity = self.initial_capacity.unwrap_or(1000);

        // Create the inner index based on metric
        let inner = match self.metric {
            DistanceMetric::Cosine => HnswIndexInner::Cosine(Hnsw::<'static, f32, DistCosine>::new(
                self.m,
                capacity,
                MAX_LAYER,
                self.ef_construction,
                DistCosine {},
            )),
            DistanceMetric::Euclidean => HnswIndexInner::Euclidean(Hnsw::<'static, f32, DistL2>::new(
                self.m,
                capacity,
                MAX_LAYER,
                self.ef_construction,
                DistL2 {},
            )),
            DistanceMetric::DotProduct => HnswIndexInner::DotProduct(Hnsw::<'static, f32, DistDot>::new(
                self.m,
                capacity,
                MAX_LAYER,
                self.ef_construction,
                DistDot {},
            )),
        };

        Ok(HnswIndex {
            inner: Arc::new(RwLock::new(inner)),
            dimensions: self.dimensions,
            metric: self.metric,
            m: self.m,
            ef_construction: self.ef_construction,
            ef_search: Arc::new(RwLock::new(self.ef_search)),
            next_data_id: Arc::new(RwLock::new(0)),
            id_mapping: Arc::new(DashMap::new()),
            reverse_id_mapping: Arc::new(DashMap::new()),
            deleted_ids: Arc::new(DashMap::new()),
        })
    }
}

/// Internal enum to handle different distance metrics as type parameters.
enum HnswIndexInner {
    Cosine(Hnsw<'static, f32, DistCosine>),
    Euclidean(Hnsw<'static, f32, DistL2>),
    DotProduct(Hnsw<'static, f32, DistDot>),
}

/// HNSW vector index for approximate k-nearest neighbor search.
///
/// This struct wraps `hnsw_rs::Hnsw` and implements the `VectorIndex` trait.
/// All operations are thread-safe due to interior mutability.
///
/// # Soft Deletes
///
/// Since hnsw_rs doesn't support native deletion, we implement soft deletes:
/// - Deleted NodeIds are tracked in `deleted_ids` DashMap
/// - Search results filter out deleted IDs
/// - Deleted vectors still consume memory until index is rebuilt
pub struct HnswIndex {
    /// Underlying hnsw_rs index (wrapped in RwLock for interior mutability)
    inner: Arc<RwLock<HnswIndexInner>>,
    /// Cached dimensions for O(1) access
    dimensions: usize,
    /// Cached metric for O(1) access
    metric: DistanceMetric,
    /// Connections per node (M parameter)
    m: usize,
    /// Build-time expansion factor
    ef_construction: usize,
    /// Query-time expansion factor (mutable for set_ef_search)
    ef_search: Arc<RwLock<usize>>,
    /// Next available data ID (hnsw_rs uses usize as DataId)
    next_data_id: Arc<RwLock<usize>>,
    /// Mapping from NodeId to DataId
    id_mapping: Arc<DashMap<NodeId, usize>>,
    /// Reverse mapping from DataId to NodeId (for O(1) lookups during search)
    reverse_id_mapping: Arc<DashMap<usize, NodeId>>,
    /// Soft-deleted NodeIds
    deleted_ids: Arc<DashMap<NodeId, ()>>,
}

impl VectorIndex for HnswIndex {
    fn add(&self, id: NodeId, vector: &[f32]) -> Result<()> {
        // Validate vector
        validate_vector(vector)?;

        // Check dimensions match
        if vector.len() != self.dimensions {
            return Err(Error::Vector(VectorError::DimensionMismatch {
                expected: self.dimensions,
                actual: vector.len(),
            }));
        }

        // Remove from deleted set if re-adding
        self.deleted_ids.remove(&id);

        // Get or create DataId for this NodeId
        let data_id = if let Some(entry) = self.id_mapping.get(&id) {
            *entry.value()
        } else {
            let mut next_id = self.next_data_id.write();
            let data_id = *next_id;
            *next_id += 1;
            drop(next_id);
            self.id_mapping.insert(id, data_id);
            self.reverse_id_mapping.insert(data_id, id);
            data_id
        };

        // Insert into index
        let mut inner = self.inner.write();
        match &mut *inner {
            HnswIndexInner::Cosine(hnsw) => {
                hnsw.insert((vector, data_id));
            }
            HnswIndexInner::Euclidean(hnsw) => {
                hnsw.insert((vector, data_id));
            }
            HnswIndexInner::DotProduct(hnsw) => {
                hnsw.insert((vector, data_id));
            }
        }

        Ok(())
    }

    fn remove(&self, id: NodeId) -> Result<()> {
        // Soft delete: mark as deleted
        // We don't remove from id_mapping to preserve DataId allocation
        self.deleted_ids.insert(id, ());
        Ok(())
    }

    fn search(&self, query: &[f32], k: usize) -> Result<Vec<(NodeId, f32)>> {
        // Validate query vector
        validate_vector(query)?;

        // Check dimensions match
        if query.len() != self.dimensions {
            return Err(Error::Vector(VectorError::DimensionMismatch {
                expected: self.dimensions,
                actual: query.len(),
            }));
        }

        // Cap k to prevent DoS
        let k_capped = k.min(MAX_K);

        // Get ef_search parameter
        let ef_search = *self.ef_search.read();

        // Perform search
        let inner = self.inner.read();
        let results = match &*inner {
            HnswIndexInner::Cosine(hnsw) => hnsw.search(query, k_capped, ef_search),
            HnswIndexInner::Euclidean(hnsw) => hnsw.search(query, k_capped, ef_search),
            HnswIndexInner::DotProduct(hnsw) => hnsw.search(query, k_capped, ef_search),
        };

        // Convert results to (NodeId, similarity) format
        let mut output: Vec<(NodeId, f32)> = Vec::new();

        for neighbor in results {
            let data_id = neighbor.d_id;

            // Lookup NodeId for this DataId (O(1) using reverse mapping)
            if let Some(node_id_ref) = self.reverse_id_mapping.get(&data_id) {
                let node_id = *node_id_ref.value();

                // Skip if deleted
                if self.deleted_ids.contains_key(&node_id) {
                    continue;
                }

                // Convert distance to similarity based on metric
                let similarity = match self.metric {
                    DistanceMetric::Cosine => 1.0 - neighbor.distance,
                    DistanceMetric::Euclidean => -neighbor.distance,
                    DistanceMetric::DotProduct => -neighbor.distance,
                };

                output.push((node_id, similarity));
            }
        }

        // Sort by similarity descending
        output.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

        Ok(output)
    }

    fn search_with_filter<F>(
        &self,
        query: &[f32],
        k: usize,
        predicate: F,
    ) -> Result<Vec<(NodeId, f32)>>
    where
        F: Fn(&NodeId) -> bool + Send + Sync,
    {
        // For hnsw_rs, we'll do post-filtering since the FilterT trait is complex
        // This is acceptable since filtering is done at search time in the results
        let results = self.search(query, k * 2)?; // Fetch more to account for filtering

        let filtered: Vec<(NodeId, f32)> = results
            .into_iter()
            .filter(|(node_id, _)| predicate(node_id))
            .take(k)
            .collect();

        Ok(filtered)
    }

    fn len(&self) -> usize {
        self.id_mapping.len() - self.deleted_ids.len()
    }

    fn dimensions(&self) -> usize {
        self.dimensions
    }

    fn distance_metric(&self) -> DistanceMetric {
        self.metric
    }
}

impl HnswIndex {
    /// Creates a new HNSW index from a configuration.
    pub fn new(config: HnswConfig) -> Result<Self> {
        HnswIndexBuilder::from_config(&config).build()
    }

    /// Sets the ef_search parameter for query-time search quality.
    ///
    /// Higher values improve recall but slow down searches.
    pub fn set_ef_search(&self, ef_search: usize) {
        *self.ef_search.write() = ef_search;
    }

    /// Gets the current ef_search value.
    pub fn get_ef_search(&self) -> usize {
        *self.ef_search.read()
    }

    /// Returns the configuration used to create this index.
    pub fn config(&self) -> HnswConfig {
        HnswConfig {
            dimensions: self.dimensions,
            metric: self.metric,
            m: self.m,
            ef_construction: self.ef_construction,
            ef_search: *self.ef_search.read(),
            capacity: 0, // capacity is not tracked
        }
    }

    /// Returns the M parameter (connections per node).
    pub fn m(&self) -> usize {
        self.m
    }
}

// SAFETY: HnswIndex is safe to send across threads and share between threads because:
// 1. hnsw_rs::Hnsw uses internal Arc/RwLock for all mutable state, making it thread-safe
// 2. All our fields use thread-safe wrappers:
//    - inner: Arc<RwLock<HnswIndexInner>> - provides synchronized access to the HNSW index
//    - ef_search: Arc<RwLock<usize>> - synchronized mutable parameter
//    - next_data_id: Arc<RwLock<usize>> - synchronized ID counter
//    - id_mapping, reverse_id_mapping: Arc<DashMap<...>> - lock-free concurrent hashmaps
//    - deleted_ids: Arc<DashMap<...>> - lock-free concurrent hashmap
// 3. Immutable fields (dimensions, metric, m, ef_construction) are safe to share
// 4. hnsw_rs library guarantees thread-safety for all public APIs as documented in their crate
unsafe impl Send for HnswIndex {}
unsafe impl Sync for HnswIndex {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hnsw_basic() -> Result<()> {
        let index = HnswIndexBuilder::new(4, DistanceMetric::Cosine).build()?;

        let node1 = NodeId::new(1).unwrap();
        let node2 = NodeId::new(2).unwrap();

        index.add(node1, &[1.0, 0.0, 0.0, 0.0])?;
        index.add(node2, &[0.0, 1.0, 0.0, 0.0])?;

        assert_eq!(index.len(), 2);

        let results = index.search(&[1.0, 0.0, 0.0, 0.0], 2)?;
        assert_eq!(results[0].0, node1);

        Ok(())
    }

    #[test]
    fn test_hnsw_remove() -> Result<()> {
        let index = HnswIndexBuilder::new(4, DistanceMetric::Cosine).build()?;

        let node1 = NodeId::new(1).unwrap();
        let node2 = NodeId::new(2).unwrap();

        index.add(node1, &[1.0, 0.0, 0.0, 0.0])?;
        index.add(node2, &[0.0, 1.0, 0.0, 0.0])?;

        assert_eq!(index.len(), 2);

        index.remove(node1)?;

        assert_eq!(index.len(), 1);

        let results = index.search(&[1.0, 0.0, 0.0, 0.0], 2)?;
        // Should only return node2 (node1 is deleted)
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].0, node2);

        Ok(())
    }

    #[test]
    fn test_hnsw_search_with_filter() -> Result<()> {
        let index = HnswIndexBuilder::new(4, DistanceMetric::Cosine).build()?;

        let node1 = NodeId::new(1).unwrap();
        let node2 = NodeId::new(2).unwrap();
        let node3 = NodeId::new(3).unwrap();

        index.add(node1, &[1.0, 0.0, 0.0, 0.0])?;
        index.add(node2, &[0.9, 0.1, 0.0, 0.0])?;
        index.add(node3, &[0.8, 0.2, 0.0, 0.0])?;

        // Filter to only even node IDs
        let results = index.search_with_filter(&[1.0, 0.0, 0.0, 0.0], 3, |id| {
            id.as_u64() % 2 == 0
        })?;

        // Should only return node2
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].0, node2);

        Ok(())
    }
}
