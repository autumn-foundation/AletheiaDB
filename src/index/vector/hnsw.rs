//! HNSW (Hierarchical Navigable Small World) vector index implementation.
//!
//! This module provides a wrapper around the `usearch` library's HNSW index,
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
//! Based on usearch benchmarks and GallifreyDB testing:
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
//! - **Default**: 200
//!
//! ## ef_search (query-time expansion)
//! - **Range**: 10-500
//! - **Higher values**: Better recall, slower queries
//! - **Default**: 10 (fast queries, ~90% recall)
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
//! let node1 = NodeId::new(1);
//! let embedding1 = vec![0.1f32; 384];
//! index.add(node1, &embedding1)?;
//!
//! let node2 = NodeId::new(2);
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
//! The underlying `usearch::Index` provides interior mutability with internal
//! locking, so all operations take `&self` rather than `&mut self`.
//!
//! # Implementation Notes
//!
//! - Uses L2sq (squared Euclidean) instead of L2 for performance (avoids sqrt)
//! - Validates all input vectors for NaN/Infinity using `validate_vector()`
//! - Caps k parameter at MAX_K (10,000) to prevent DoS attacks
//! - Maps usearch exceptions to `VectorError::IndexError`
//! - No Clone implementation (usearch::Index is not Clone)

use crate::core::id::NodeId;
use crate::core::vector::validate_vector;
use crate::index::vector::{DistanceMetric, VectorIndex};
use crate::utils::{Error, Result, error::VectorError};

/// Maximum number of results that can be requested in a search.
///
/// This prevents DoS attacks via excessive memory allocation when an attacker
/// requests an extremely large k value. With 10,000 results max:
/// - Memory per search: ~10,000 * (8 bytes NodeId + 4 bytes f32) = ~120KB
/// - This is reasonable for concurrent queries without risking OOM
///
/// **Design Choice**: This is a global constant rather than per-index configuration
/// to maintain a consistent security boundary across all indexes. If use cases
/// require larger k values, this can be made configurable in future versions,
/// but 10,000 results should be sufficient for most similarity search use cases
/// (typically k=10 to k=100).
const MAX_K: usize = 10_000;

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
///   - **Higher values**: Better recall, more memory (O(n * M * d) bytes), slower build
///   - **Lower values**: Less memory, faster build, lower recall
///   - Memory impact: Each connection adds ~4 bytes per vector
///   - Recommended: 16 for general use, 32 for high recall requirements
///
/// - **`ef_construction`**: Candidate list size during index construction (default: 128)
///   - Range: 100-500, typical: 100-400
///   - **Higher values**: Better index quality, slower build time (O(log n) per insert)
///   - **Lower values**: Faster build, potentially lower recall
///   - Build time impact: Roughly linear with ef_construction
///   - Recommended: 128 for balanced performance, 200+ for high-quality indexes
///
/// - **`ef_search`**: Candidate list size during query processing (default: 64)
///   - Range: 10-500, typical: 10-100
///   - **Higher values**: Better recall, slower queries
///   - **Lower values**: Faster queries, lower recall (~90% at ef=10)
///   - Query time impact: Roughly linear with ef_search
///   - Can be adjusted at runtime via `HnswIndex::set_ef_search()`
///   - Recommended: 64 for balanced recall/speed, 10 for low-latency
///
/// - **`capacity`**: Initial capacity hint for pre-allocation (default: 0)
///   - Pre-allocates space for the specified number of vectors
///   - Reduces reallocation overhead during bulk insertion
///   - Recommended: Set to expected index size if known in advance
///
/// # Examples
///
/// ```rust
/// use gallifreydb::index::vector::{HnswConfig, DistanceMetric};
///
/// // Use default configuration
/// let config = HnswConfig::default()
///     .with_dimensions(384)
///     .with_metric(DistanceMetric::Cosine);
///
/// // High-recall configuration
/// let config = HnswConfig::default()
///     .with_dimensions(768)
///     .with_metric(DistanceMetric::Cosine)
///     .with_m(32)
///     .with_ef_construction(200)
///     .with_ef_search(100);
///
/// // Low-latency configuration
/// let config = HnswConfig::default()
///     .with_dimensions(384)
///     .with_metric(DistanceMetric::Cosine)
///     .with_m(12)
///     .with_ef_construction(100)
///     .with_ef_search(10);
///
/// // Production configuration with known capacity
/// let config = HnswConfig::default()
///     .with_dimensions(1536)
///     .with_metric(DistanceMetric::Cosine)
///     .with_capacity(1_000_000); // Pre-allocate for 1M vectors
/// ```
///
/// # Performance Trade-offs
///
/// ```text
/// Configuration    | Recall | Build Speed | Query Speed | Memory
/// -----------------|--------|-------------|-------------|--------
/// Low Latency      |  ~90%  | Fast        | Fast        | Low
/// (M=12, ef_c=100, ef_s=10)
///
/// Balanced (Default) | ~95% | Medium      | Medium      | Medium
/// (M=16, ef_c=128, ef_s=64)
///
/// High Recall      | ~98%  | Slow        | Slow        | High
/// (M=32, ef_c=200, ef_s=100)
/// ```
///
/// # See Also
///
/// - [`HnswIndexBuilder`]: Builder for creating indexes from config
/// - [`VectorIndex`]: Trait implemented by HNSW index
/// - Module documentation for tuning guidelines
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
    /// Returns a default HNSW configuration optimized for balanced performance.
    ///
    /// Default values:
    /// - `dimensions`: 0 (must be set before use)
    /// - `metric`: Cosine
    /// - `m`: 16
    /// - `ef_construction`: 128
    /// - `ef_search`: 64
    /// - `capacity`: 0
    ///
    /// # Note
    ///
    /// The `dimensions` field defaults to 0 and must be set to a valid value (> 0)
    /// before building an index.
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
    ///
    /// Other parameters are set to their default values.
    ///
    /// # Arguments
    ///
    /// * `dimensions` - Vector dimensionality (must be > 0)
    /// * `metric` - Distance metric to use
    ///
    /// # Examples
    ///
    /// ```rust
    /// use gallifreydb::index::vector::{HnswConfig, DistanceMetric};
    ///
    /// let config = HnswConfig::new(384, DistanceMetric::Cosine);
    /// ```
    pub fn new(dimensions: usize, metric: DistanceMetric) -> Self {
        HnswConfig {
            dimensions,
            metric,
            ..Default::default()
        }
    }

    /// Sets the vector dimensionality.
    pub fn with_dimensions(mut self, dimensions: usize) -> Self {
        self.dimensions = dimensions;
        self
    }

    /// Sets the distance metric.
    pub fn with_metric(mut self, metric: DistanceMetric) -> Self {
        self.metric = metric;
        self
    }

    /// Sets the maximum bidirectional connections per node (M parameter).
    ///
    /// Higher values improve recall but increase memory usage and build time.
    /// Typical range: 8-64, recommended: 16 (default) to 32 (high recall).
    pub fn with_m(mut self, m: usize) -> Self {
        self.m = m;
        self
    }

    /// Sets the build-time candidate list size (ef_construction parameter).
    ///
    /// Higher values create better quality indexes but take longer to build.
    /// Typical range: 100-500, recommended: 128 (default) to 200+ (high quality).
    pub fn with_ef_construction(mut self, ef_construction: usize) -> Self {
        self.ef_construction = ef_construction;
        self
    }

    /// Sets the query-time candidate list size (ef_search parameter).
    ///
    /// Higher values improve recall but slow down queries.
    /// Typical range: 10-500, recommended: 64 (default) for balanced recall/speed.
    ///
    /// Note: This can be adjusted at runtime via `HnswIndex::set_ef_search()`.
    pub fn with_ef_search(mut self, ef_search: usize) -> Self {
        self.ef_search = ef_search;
        self
    }

    /// Sets the initial capacity for pre-allocation.
    ///
    /// Pre-allocates space for the specified number of vectors to reduce
    /// reallocation overhead during bulk insertion.
    pub fn with_capacity(mut self, capacity: usize) -> Self {
        self.capacity = capacity;
        self
    }
}

/// Builder for configuring and creating an `HnswIndex`.
///
/// Required parameters:
/// - `dimensions`: Vector dimensionality (must be > 0)
/// - `metric`: Distance metric (Cosine, Euclidean, or DotProduct)
///
/// Optional parameters with defaults:
/// - `m`: Connections per node (default: 16)
/// - `ef_construction`: Build-time expansion (default: 200)
/// - `ef_search`: Query-time expansion (default: 10)
/// - `initial_capacity`: Pre-allocated capacity (default: 0 - grows dynamically)
///
/// # Validation Strategy
///
/// **Design Choice**: Parameter validation happens at `build()` time, not in setters.
/// This allows the builder pattern to be more flexible (e.g., setting invalid values
/// temporarily during construction) and provides a single point of failure with clear
/// error messages. The trade-off is that errors are deferred rather than fail-fast,
/// but this is acceptable for a builder pattern where the user explicitly calls
/// `build()` to finalize configuration.
///
/// # Examples
///
/// ```rust
/// use gallifreydb::index::vector::{HnswIndexBuilder, DistanceMetric};
///
/// # fn example() -> gallifreydb::utils::Result<()> {
/// // Minimal configuration
/// let index = HnswIndexBuilder::new(384, DistanceMetric::Cosine).build()?;
///
/// // Full configuration
/// let index = HnswIndexBuilder::new(768, DistanceMetric::Euclidean)
///     .m(32)
///     .ef_construction(400)
///     .ef_search(50)
///     .initial_capacity(100_000)
///     .build()?;
/// # Ok(())
/// # }
/// ```
pub struct HnswIndexBuilder {
    dimensions: usize,
    metric: DistanceMetric,
    #[allow(dead_code)]
    m: usize,
    ef_construction: usize,
    #[allow(dead_code)]
    ef_search: usize,
    initial_capacity: Option<usize>,
}

impl HnswIndexBuilder {
    /// Creates a new builder with the required parameters.
    ///
    /// # Arguments
    ///
    /// * `dimensions` - Vector dimensionality (must be > 0)
    /// * `metric` - Distance metric to use
    ///
    /// # Panics
    ///
    /// Panics if dimensions is 0 (caught during build()).
    pub fn new(dimensions: usize, metric: DistanceMetric) -> Self {
        HnswIndexBuilder {
            dimensions,
            metric,
            m: 16,                // Default from usearch
            ef_construction: 200, // Good default for build quality
            ef_search: 10,        // Fast searches with ~90% recall
            initial_capacity: None,
        }
    }

    /// Creates a new builder from an `HnswConfig`.
    ///
    /// This allows you to configure the index using a reusable configuration
    /// object rather than chaining builder methods.
    ///
    /// # Arguments
    ///
    /// * `config` - HNSW configuration with all parameters
    ///
    /// # Examples
    ///
    /// ```rust
    /// use gallifreydb::index::vector::{HnswConfig, HnswIndexBuilder, DistanceMetric};
    ///
    /// # fn example() -> gallifreydb::utils::Result<()> {
    /// // Create a reusable configuration
    /// let config = HnswConfig::new(384, DistanceMetric::Cosine)
    ///     .with_m(32)
    ///     .with_ef_construction(200);
    ///
    /// // Build index from config
    /// let index = HnswIndexBuilder::from_config(config).build()?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn from_config(config: HnswConfig) -> Self {
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

    /// Sets the number of bidirectional connections per node (M parameter).
    ///
    /// Higher values improve recall but increase memory usage and build time.
    ///
    /// # Arguments
    ///
    /// * `m` - Connections per node (typical range: 8-64, must be > 0)
    pub fn m(mut self, m: usize) -> Self {
        self.m = m;
        self
    }

    /// Sets the ef_construction parameter for index building.
    ///
    /// Higher values create better quality indexes but take longer to build.
    ///
    /// # Arguments
    ///
    /// * `ef` - Build-time expansion factor (typical range: 100-500, must be > 0)
    pub fn ef_construction(mut self, ef: usize) -> Self {
        self.ef_construction = ef;
        self
    }

    /// Sets the ef_search parameter for query processing.
    ///
    /// Higher values improve recall but slow down queries.
    ///
    /// # Arguments
    ///
    /// * `ef` - Query-time expansion factor (typical range: 10-500, must be > 0)
    pub fn ef_search(mut self, ef: usize) -> Self {
        self.ef_search = ef;
        self
    }

    /// Pre-allocates capacity for a certain number of vectors.
    ///
    /// This can improve performance if you know approximately how many
    /// vectors will be indexed.
    ///
    /// # Arguments
    ///
    /// * `capacity` - Number of vectors to pre-allocate space for
    pub fn initial_capacity(mut self, capacity: usize) -> Self {
        self.initial_capacity = Some(capacity);
        self
    }

    /// Builds the HNSW index with the configured parameters.
    ///
    /// # Returns
    ///
    /// - `Ok(HnswIndex)` if the index was created successfully
    /// - `Err(Error::Vector(VectorError::InvalidVector))` if parameters are invalid
    /// - `Err(Error::Vector(VectorError::IndexError))` if usearch index creation fails
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - `dimensions` is 0
    /// - `m` is 0
    /// - `ef_construction` is 0
    /// - `ef_search` is 0
    /// - usearch index creation fails
    pub fn build(self) -> Result<HnswIndex> {
        // Validate parameters
        if self.dimensions == 0 {
            return Err(Error::Vector(VectorError::InvalidVector {
                reason: "dimensions must be greater than 0".to_string(),
            }));
        }
        if self.m == 0 {
            return Err(Error::Vector(VectorError::InvalidVector {
                reason: "M (connections per node) must be greater than 0".to_string(),
            }));
        }
        if self.ef_construction == 0 {
            return Err(Error::Vector(VectorError::InvalidVector {
                reason: "ef_construction must be greater than 0".to_string(),
            }));
        }
        if self.ef_search == 0 {
            return Err(Error::Vector(VectorError::InvalidVector {
                reason: "ef_search must be greater than 0".to_string(),
            }));
        }

        // Map our DistanceMetric to usearch::MetricKind
        let usearch_metric = match self.metric {
            DistanceMetric::Cosine => usearch::MetricKind::Cos,
            DistanceMetric::Euclidean => usearch::MetricKind::L2sq, // Squared Euclidean (faster)
            DistanceMetric::DotProduct => usearch::MetricKind::IP,  // Inner Product
        };

        // Create usearch index options
        let options = usearch::IndexOptions {
            dimensions: self.dimensions,
            metric: usearch_metric,
            quantization: usearch::ScalarKind::F32,
            connectivity: self.m,
            expansion_add: self.ef_construction,
            expansion_search: self.ef_search,
            multi: false,
        };

        // Create the index
        let index = usearch::Index::new(&options).map_err(|e| {
            Error::Vector(VectorError::IndexError(format!(
                "Failed to create HNSW index: {}",
                e
            )))
        })?;

        // Reserve capacity if specified
        if let Some(capacity) = self.initial_capacity {
            index.reserve(capacity).map_err(|e| {
                Error::Vector(VectorError::IndexError(format!(
                    "Failed to reserve capacity: {}",
                    e
                )))
            })?;
        }

        Ok(HnswIndex {
            index,
            dimensions: self.dimensions,
            metric: self.metric,
            m: self.m,
            ef_construction: self.ef_construction,
            ef_search: self.ef_search,
        })
    }
}

/// HNSW vector index for approximate k-nearest neighbor search.
///
/// This struct wraps `usearch::Index` and implements the `VectorIndex` trait.
/// All operations are thread-safe due to interior mutability in usearch.
///
/// # Thread Safety
///
/// - Implements `Send + Sync` for concurrent access
/// - All methods take `&self` (not `&mut self`)
/// - usearch provides internal locking for thread safety
///
/// # Memory Layout
///
/// - `index`: The underlying usearch HNSW graph (~4 bytes per dimension + M * 4 bytes per vector)
/// - `dimensions`: Cached dimension count (8 bytes)
/// - `metric`: Cached distance metric (1 byte + alignment)
/// - `m`, `ef_construction`, `ef_search`: Tuning parameters (8 bytes each)
///
/// Total struct size: ~48 bytes + usearch index size
#[allow(dead_code)]
pub struct HnswIndex {
    /// Underlying usearch index
    index: usearch::Index,
    /// Cached dimensions for O(1) access
    dimensions: usize,
    /// Cached metric for O(1) access
    metric: DistanceMetric,
    /// Connections per node (M parameter)
    #[allow(dead_code)]
    m: usize,
    /// Build-time expansion factor
    #[allow(dead_code)]
    ef_construction: usize,
    /// Query-time expansion factor
    #[allow(dead_code)]
    ef_search: usize,
}

// usearch::Index implements Send + Sync, so we can too

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

        // Convert NodeId to u64 key
        let key = id.as_u64();

        // Handle duplicate IDs: remove then re-add
        // usearch doesn't provide a native "upsert" operation, and with multi=false
        // it doesn't allow duplicate keys. The VectorIndex trait contract says:
        // "If a vector with the same id already exists, it will be replaced."
        // So we remove first (if exists) then add. This involves two operations but
        // ensures correct replacement semantics.
        // We ignore errors from remove since the key may not exist (first insert).
        let _ = self.index.remove(key);

        // Add to index
        self.index.add(key, vector).map_err(|e| {
            Error::Vector(VectorError::IndexError(format!(
                "Failed to add vector: {}",
                e
            )))
        })?;

        Ok(())
    }

    fn remove(&self, id: NodeId) -> Result<()> {
        let key = id.as_u64();

        // Remove from index (no-op if doesn't exist, per trait contract)
        self.index.remove(key).map_err(|e| {
            Error::Vector(VectorError::IndexError(format!(
                "Failed to remove vector: {}",
                e
            )))
        })?;

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

        // Perform search
        let matches = self
            .index
            .search(query, k_capped)
            .map_err(|e| Error::Vector(VectorError::IndexError(format!("Search failed: {}", e))))?;

        // Convert distances to similarities based on metric
        // usearch returns DISTANCES (not similarities) for all metrics:
        // - Cosine: 1 - cos(a,b), range [0, 2] (0 = identical, 2 = opposite)
        // - L2sq: ||a-b||^2, range [0, ∞) (0 = identical)
        //   Note: usearch uses L2sq internally (squared Euclidean) for performance,
        //   avoiding the sqrt computation. This is faster and preserves ordering.
        // - DotProduct: -dot(a,b), range (-∞, ∞) (more negative = more similar)
        //
        // We convert to similarities where higher = more similar:
        // - Cosine: similarity = 1 - distance (recovers original cosine similarity)
        // - Euclidean: similarity = -distance (negative squared distance, higher = more similar)
        //   Users get negative L2 squared distance, not L2 distance. This maintains
        //   ordering (closer vectors = less negative = higher similarity).
        // - DotProduct: similarity = -distance (recovers original dot product)
        let mut results: Vec<(NodeId, f32)> = matches
            .keys
            .iter()
            .zip(matches.distances.iter())
            .map(|(&key, &distance)| {
                let similarity = match self.metric {
                    DistanceMetric::Cosine => 1.0 - distance,
                    DistanceMetric::Euclidean => -distance,
                    DistanceMetric::DotProduct => -distance,
                };
                (NodeId::new(key), similarity)
            })
            .collect();

        // Sort by similarity descending (highest similarity first) to match trait contract
        results.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

        Ok(results)
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

        // Create filter function that converts key to NodeId
        let filter = |key: u64| -> bool {
            let node_id = NodeId::new(key);
            predicate(&node_id)
        };

        // Perform filtered search
        let matches = self
            .index
            .filtered_search(query, k_capped, filter)
            .map_err(|e| {
                Error::Vector(VectorError::IndexError(format!(
                    "Filtered search failed: {}",
                    e
                )))
            })?;

        // Convert distances to similarities (same logic as search())
        let mut results: Vec<(NodeId, f32)> = matches
            .keys
            .iter()
            .zip(matches.distances.iter())
            .map(|(&key, &distance)| {
                let similarity = match self.metric {
                    DistanceMetric::Cosine => 1.0 - distance,
                    DistanceMetric::Euclidean => -distance,
                    DistanceMetric::DotProduct => -distance,
                };
                (NodeId::new(key), similarity)
            })
            .collect();

        // Sort by similarity descending (highest similarity first) to match trait contract
        results.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

        Ok(results)
    }

    fn len(&self) -> usize {
        self.index.size()
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
    ///
    /// This is a convenience factory method that wraps `HnswIndexBuilder::from_config().build()`.
    /// It provides a simpler API for creating indexes from reusable configurations.
    ///
    /// # Arguments
    ///
    /// * `config` - HNSW configuration with all parameters
    ///
    /// # Returns
    ///
    /// - `Ok(HnswIndex)` if the index was created successfully
    /// - `Err(Error)` if parameters are invalid or index creation fails
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - `config.dimensions` is 0
    /// - `config.m` is 0
    /// - `config.ef_construction` is 0
    /// - `config.ef_search` is 0
    /// - Underlying usearch index creation fails
    ///
    /// # Examples
    ///
    /// ```rust
    /// use gallifreydb::index::vector::{HnswIndex, HnswConfig, DistanceMetric};
    ///
    /// # fn example() -> gallifreydb::utils::Result<()> {
    /// // Create a reusable configuration
    /// let config = HnswConfig::new(384, DistanceMetric::Cosine)
    ///     .with_m(16)
    ///     .with_ef_construction(200)
    ///     .with_ef_search(64);
    ///
    /// // Create index from config
    /// let index = HnswIndex::new(config)?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn new(config: HnswConfig) -> Result<Self> {
        HnswIndexBuilder::from_config(config).build()
    }

    /// Creates a new HNSW index from a configuration with explicit capacity.
    ///
    /// This factory method allows overriding the capacity from the config,
    /// which is useful when you want to use a standard config but need different
    /// pre-allocation for this specific index.
    ///
    /// # Arguments
    ///
    /// * `config` - HNSW configuration (capacity field will be overridden)
    /// * `capacity` - Number of vectors to pre-allocate space for
    ///
    /// # Returns
    ///
    /// - `Ok(HnswIndex)` if the index was created successfully
    /// - `Err(Error)` if parameters are invalid or index creation fails
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - `config.dimensions` is 0
    /// - `config.m` is 0
    /// - `config.ef_construction` is 0
    /// - `config.ef_search` is 0
    /// - Underlying usearch index creation fails
    ///
    /// # Examples
    ///
    /// ```rust
    /// use gallifreydb::index::vector::{HnswIndex, HnswConfig, DistanceMetric};
    ///
    /// # fn example() -> gallifreydb::utils::Result<()> {
    /// // Standard configuration
    /// let config = HnswConfig::new(384, DistanceMetric::Cosine);
    ///
    /// // Create index with explicit capacity (ignores config.capacity)
    /// let index = HnswIndex::with_capacity(config, 1_000_000)?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn with_capacity(config: HnswConfig, capacity: usize) -> Result<Self> {
        HnswIndexBuilder::from_config(config)
            .initial_capacity(capacity)
            .build()
    }

    /// Returns the M parameter (connections per node).
    ///
    /// Higher values improve recall at the cost of memory and build time.
    /// Typical range: 8-64, default: 16.
    #[must_use]
    pub fn m(&self) -> usize {
        self.m
    }

    /// Returns the ef_construction parameter (build-time expansion).
    ///
    /// Higher values improve index quality at the cost of build time.
    /// Typical range: 100-500, default: 200.
    #[must_use]
    pub fn ef_construction(&self) -> usize {
        self.ef_construction
    }

    /// Returns the ef_search parameter (query-time expansion).
    ///
    /// Higher values improve recall at the cost of query latency.
    /// Typical range: 10-500, default: 10.
    ///
    /// Note: This returns the configured value. To change ef_search at runtime,
    /// use `set_ef_search()`.
    #[must_use]
    pub fn ef_search(&self) -> usize {
        self.ef_search
    }

    /// Sets the ef_search parameter for subsequent queries.
    ///
    /// This allows runtime tuning of the recall/latency trade-off without
    /// rebuilding the index.
    ///
    /// # Arguments
    ///
    /// * `ef_search` - New expansion factor (must be > 0)
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # use gallifreydb::index::{HnswIndexBuilder, DistanceMetric};
    /// # let index = HnswIndexBuilder::new(4, DistanceMetric::Cosine)
    /// #     .build().unwrap();
    /// // Start with fast queries (lower recall)
    /// index.set_ef_search(10);
    ///
    /// // Switch to high-recall mode for important queries
    /// index.set_ef_search(100);
    /// ```
    pub fn set_ef_search(&self, ef_search: usize) {
        self.index.change_expansion_search(ef_search);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::thread;

    /// Helper to create a simple 4D index
    fn create_test_index() -> HnswIndex {
        HnswIndexBuilder::new(4, DistanceMetric::Cosine)
            .initial_capacity(100)
            .build()
            .expect("Failed to create test index")
    }

    // ============================================================================
    // HnswConfig Tests
    // ============================================================================

    #[test]
    fn test_hnsw_config_default() {
        let config = HnswConfig::default();

        assert_eq!(config.dimensions, 0);
        assert_eq!(config.metric, DistanceMetric::Cosine);
        assert_eq!(config.m, 16);
        assert_eq!(config.ef_construction, 128);
        assert_eq!(config.ef_search, 64);
        assert_eq!(config.capacity, 0);
    }

    #[test]
    fn test_hnsw_config_new() {
        let config = HnswConfig::new(384, DistanceMetric::Euclidean);

        assert_eq!(config.dimensions, 384);
        assert_eq!(config.metric, DistanceMetric::Euclidean);
        // Other fields should have defaults
        assert_eq!(config.m, 16);
        assert_eq!(config.ef_construction, 128);
        assert_eq!(config.ef_search, 64);
        assert_eq!(config.capacity, 0);
    }

    #[test]
    fn test_hnsw_config_builder_pattern() {
        let config = HnswConfig::default()
            .with_dimensions(768)
            .with_metric(DistanceMetric::DotProduct)
            .with_m(32)
            .with_ef_construction(200)
            .with_ef_search(100)
            .with_capacity(10_000);

        assert_eq!(config.dimensions, 768);
        assert_eq!(config.metric, DistanceMetric::DotProduct);
        assert_eq!(config.m, 32);
        assert_eq!(config.ef_construction, 200);
        assert_eq!(config.ef_search, 100);
        assert_eq!(config.capacity, 10_000);
    }

    #[test]
    fn test_hnsw_config_clone() {
        let config1 = HnswConfig::new(384, DistanceMetric::Cosine)
            .with_m(24)
            .with_capacity(5000);

        let config2 = config1.clone();

        assert_eq!(config1, config2);
        assert_eq!(config2.dimensions, 384);
        assert_eq!(config2.m, 24);
        assert_eq!(config2.capacity, 5000);
    }

    #[test]
    fn test_hnsw_config_debug() {
        let config = HnswConfig::new(128, DistanceMetric::Cosine);
        let debug_str = format!("{:?}", config);

        // Should contain key information
        assert!(debug_str.contains("128"));
        assert!(debug_str.contains("Cosine"));
    }

    #[test]
    fn test_hnsw_index_builder_from_config() {
        let config = HnswConfig::new(4, DistanceMetric::Cosine)
            .with_m(32)
            .with_ef_construction(200)
            .with_ef_search(100)
            .with_capacity(1000);

        let index = HnswIndexBuilder::from_config(config).build().unwrap();

        assert_eq!(index.dimensions(), 4);
        assert_eq!(index.distance_metric(), DistanceMetric::Cosine);
        assert_eq!(index.m(), 32);
        assert_eq!(index.ef_construction(), 200);
        assert_eq!(index.ef_search(), 100);
    }

    #[test]
    fn test_hnsw_index_builder_from_config_zero_capacity() {
        // Config with capacity = 0 should result in None for initial_capacity
        let config = HnswConfig::new(4, DistanceMetric::Cosine);

        let index = HnswIndexBuilder::from_config(config).build().unwrap();

        assert_eq!(index.dimensions(), 4);
    }

    #[test]
    fn test_hnsw_config_partial_customization() {
        // Only customize some fields
        let config = HnswConfig::default()
            .with_dimensions(512)
            .with_metric(DistanceMetric::Euclidean)
            .with_m(20);

        // Check customized fields
        assert_eq!(config.dimensions, 512);
        assert_eq!(config.metric, DistanceMetric::Euclidean);
        assert_eq!(config.m, 20);

        // Check defaults are preserved
        assert_eq!(config.ef_construction, 128);
        assert_eq!(config.ef_search, 64);
        assert_eq!(config.capacity, 0);
    }

    #[test]
    fn test_hnsw_config_equality() {
        let config1 = HnswConfig::new(384, DistanceMetric::Cosine)
            .with_m(16)
            .with_ef_construction(128);

        let config2 = HnswConfig::new(384, DistanceMetric::Cosine)
            .with_m(16)
            .with_ef_construction(128);

        let config3 = HnswConfig::new(384, DistanceMetric::Cosine).with_m(32); // Different M

        assert_eq!(config1, config2);
        assert_ne!(config1, config3);
    }

    // ============================================================================
    // HnswIndex Tests
    // ============================================================================

    #[test]
    fn test_hnsw_add_and_search() {
        let index = create_test_index();

        // Add vectors
        let node1 = NodeId::new(1);
        let vec1 = vec![1.0, 0.0, 0.0, 0.0];
        index.add(node1, &vec1).unwrap();

        let node2 = NodeId::new(2);
        let vec2 = vec![0.0, 1.0, 0.0, 0.0];
        index.add(node2, &vec2).unwrap();

        // Search for vector similar to vec1
        let query = vec![0.9, 0.1, 0.0, 0.0];
        let results = index.search(&query, 2).unwrap();

        assert_eq!(results.len(), 2);
        assert_eq!(results[0].0, node1); // Most similar should be node1
    }

    #[test]
    fn test_hnsw_remove() {
        let index = create_test_index();

        // Add and remove
        let node1 = NodeId::new(1);
        let vec1 = vec![1.0, 0.0, 0.0, 0.0];
        index.add(node1, &vec1).unwrap();

        assert_eq!(index.len(), 1);

        index.remove(node1).unwrap();
        assert_eq!(index.len(), 0);
    }

    #[test]
    fn test_hnsw_remove_nonexistent() {
        let index = create_test_index();

        // Removing non-existent node should be no-op
        let result = index.remove(NodeId::new(999));
        assert!(result.is_ok());
    }

    #[test]
    fn test_hnsw_search_empty_index() {
        let index = create_test_index();

        let query = vec![1.0, 0.0, 0.0, 0.0];
        let results = index.search(&query, 10).unwrap();

        assert_eq!(results.len(), 0);
    }

    #[test]
    fn test_hnsw_dimensions() {
        let index = HnswIndexBuilder::new(128, DistanceMetric::Euclidean)
            .build()
            .unwrap();

        assert_eq!(index.dimensions(), 128);
    }

    #[test]
    fn test_hnsw_metric() {
        let index = HnswIndexBuilder::new(64, DistanceMetric::DotProduct)
            .build()
            .unwrap();

        assert_eq!(index.distance_metric(), DistanceMetric::DotProduct);
    }

    #[test]
    fn test_hnsw_wrong_dimensions() {
        let index = create_test_index(); // 4D index

        let node1 = NodeId::new(1);
        let wrong_vec = vec![1.0, 0.0]; // Only 2D

        let result = index.add(node1, &wrong_vec);
        assert!(matches!(
            result,
            Err(Error::Vector(VectorError::DimensionMismatch { .. }))
        ));
    }

    #[test]
    fn test_hnsw_nan_vector() {
        let index = create_test_index();

        let node1 = NodeId::new(1);
        let nan_vec = vec![1.0, f32::NAN, 0.0, 0.0];

        let result = index.add(node1, &nan_vec);
        assert!(matches!(
            result,
            Err(Error::Vector(VectorError::ContainsNaN { .. }))
        ));
    }

    #[test]
    fn test_hnsw_infinity_vector() {
        let index = create_test_index();

        let node1 = NodeId::new(1);
        let inf_vec = vec![1.0, f32::INFINITY, 0.0, 0.0];

        let result = index.add(node1, &inf_vec);
        assert!(matches!(
            result,
            Err(Error::Vector(VectorError::ContainsInfinity { .. }))
        ));
    }

    #[test]
    fn test_hnsw_search_wrong_dimensions() {
        let index = create_test_index();

        let query = vec![1.0, 0.0]; // Wrong dimensions
        let result = index.search(&query, 10);

        assert!(matches!(
            result,
            Err(Error::Vector(VectorError::DimensionMismatch { .. }))
        ));
    }

    #[test]
    fn test_hnsw_duplicate_id() {
        let index = create_test_index();

        let node1 = NodeId::new(1);
        let vec1 = vec![1.0, 0.0, 0.0, 0.0];
        index.add(node1, &vec1).unwrap();

        // Add again with different vector (should replace)
        let vec2 = vec![0.0, 1.0, 0.0, 0.0];
        index.add(node1, &vec2).unwrap();

        // Should still have 1 vector
        assert_eq!(index.len(), 1);
    }

    #[test]
    fn test_hnsw_large_k() {
        let index = create_test_index();

        // Add only 2 vectors
        index.add(NodeId::new(1), &[1.0, 0.0, 0.0, 0.0]).unwrap();
        index.add(NodeId::new(2), &[0.0, 1.0, 0.0, 0.0]).unwrap();

        // Search for more than available
        let query = vec![0.5, 0.5, 0.0, 0.0];
        let results = index.search(&query, 100).unwrap();

        // Should return only 2 results
        assert_eq!(results.len(), 2);
    }

    #[test]
    fn test_hnsw_search_with_filter() {
        let index = create_test_index();

        // Add vectors
        index.add(NodeId::new(1), &[1.0, 0.0, 0.0, 0.0]).unwrap();
        index.add(NodeId::new(2), &[0.9, 0.1, 0.0, 0.0]).unwrap();
        index.add(NodeId::new(3), &[0.0, 1.0, 0.0, 0.0]).unwrap();

        // Filter to only allow node 1 and 3
        let allowed = [NodeId::new(1), NodeId::new(3)];
        let query = vec![1.0, 0.0, 0.0, 0.0];
        let results = index
            .search_with_filter(&query, 10, |id| allowed.contains(id))
            .unwrap();

        // Should only return nodes 1 and 3
        assert_eq!(results.len(), 2);
        assert!(results.iter().any(|(id, _)| *id == NodeId::new(1)));
        assert!(results.iter().any(|(id, _)| *id == NodeId::new(3)));
        assert!(!results.iter().any(|(id, _)| *id == NodeId::new(2)));
    }

    #[test]
    fn test_hnsw_search_with_filter_no_matches() {
        let index = create_test_index();

        // Add vectors
        index.add(NodeId::new(1), &[1.0, 0.0, 0.0, 0.0]).unwrap();
        index.add(NodeId::new(2), &[0.0, 1.0, 0.0, 0.0]).unwrap();

        // Filter that rejects everything
        let query = vec![1.0, 0.0, 0.0, 0.0];
        let results = index.search_with_filter(&query, 10, |_| false).unwrap();

        assert_eq!(results.len(), 0);
    }

    #[test]
    fn test_hnsw_cosine_metric() {
        let index = HnswIndexBuilder::new(4, DistanceMetric::Cosine)
            .initial_capacity(100)
            .build()
            .unwrap();

        // Add orthogonal vectors
        index.add(NodeId::new(1), &[1.0, 0.0, 0.0, 0.0]).unwrap();
        index.add(NodeId::new(2), &[0.0, 1.0, 0.0, 0.0]).unwrap();

        // Query should be most similar to node 1
        let query = vec![0.9, 0.1, 0.0, 0.0];
        let results = index.search(&query, 2).unwrap();

        assert_eq!(results[0].0, NodeId::new(1));
    }

    #[test]
    fn test_hnsw_euclidean_metric() {
        let index = HnswIndexBuilder::new(4, DistanceMetric::Euclidean)
            .initial_capacity(100)
            .build()
            .unwrap();

        index.add(NodeId::new(1), &[1.0, 0.0, 0.0, 0.0]).unwrap();
        index.add(NodeId::new(2), &[10.0, 0.0, 0.0, 0.0]).unwrap();

        // Query closest to node 1
        let query = vec![1.5, 0.0, 0.0, 0.0];
        let results = index.search(&query, 2).unwrap();

        assert_eq!(results[0].0, NodeId::new(1));
    }

    #[test]
    #[cfg_attr(
        target_os = "windows",
        ignore = "IP metric crashes on Windows (usearch FFI issue)"
    )]
    fn test_hnsw_dotproduct_metric() {
        let index = HnswIndexBuilder::new(4, DistanceMetric::DotProduct)
            .initial_capacity(100)
            .build()
            .unwrap();

        index.add(NodeId::new(1), &[2.0, 0.0, 0.0, 0.0]).unwrap();
        index.add(NodeId::new(2), &[1.0, 0.0, 0.0, 0.0]).unwrap();

        // Dot product with [1,0,0,0]: node1=2.0, node2=1.0
        let query = vec![1.0, 0.0, 0.0, 0.0];
        let results = index.search(&query, 2).unwrap();

        assert_eq!(results[0].0, NodeId::new(1)); // Higher dot product
    }

    #[test]
    fn test_hnsw_concurrent_add() {
        let index = Arc::new(create_test_index());
        let mut handles = vec![];

        // Spawn 4 threads, each adding 25 vectors
        // (Reduced from 10 threads to avoid exhausting usearch's thread pool on macOS)
        for i in 0..4 {
            let index_clone = Arc::clone(&index);
            let handle = thread::spawn(move || {
                for j in 0..25 {
                    let node_id = NodeId::new((i * 25 + j) as u64);
                    let vec = vec![i as f32, j as f32, 0.0, 0.0];
                    index_clone.add(node_id, &vec).unwrap();
                }
            });
            handles.push(handle);
        }

        // Wait for all threads
        for handle in handles {
            handle.join().unwrap();
        }

        // Should have 100 vectors
        assert_eq!(index.len(), 100);
    }

    #[test]
    fn test_hnsw_concurrent_search() {
        let index = Arc::new(create_test_index());

        // Add some vectors first
        for i in 0..100 {
            index
                .add(NodeId::new(i), &[i as f32 / 100.0, 0.0, 0.0, 0.0])
                .unwrap();
        }

        let mut handles = vec![];

        // Spawn 4 threads, each searching
        // (Reduced from 10 threads for consistency and to avoid thread pool issues)
        for _ in 0..4 {
            let index_clone = Arc::clone(&index);
            let handle = thread::spawn(move || {
                let query = vec![0.5, 0.0, 0.0, 0.0];
                let results = index_clone.search(&query, 10).unwrap();
                assert!(!results.is_empty());
            });
            handles.push(handle);
        }

        // Wait for all threads
        for handle in handles {
            handle.join().unwrap();
        }
    }

    #[test]
    fn test_hnsw_concurrent_add_and_search() {
        let index = Arc::new(create_test_index());
        let mut handles = vec![];

        // Spawn threads that add and search simultaneously
        for i in 0..5 {
            let index_clone = Arc::clone(&index);
            let handle = thread::spawn(move || {
                // Add vectors
                for j in 0..20 {
                    let node_id = NodeId::new((i * 20 + j) as u64);
                    let vec = vec![i as f32, j as f32, 0.0, 0.0];
                    index_clone.add(node_id, &vec).unwrap();
                }

                // Search
                let query = vec![i as f32, 10.0, 0.0, 0.0];
                let _ = index_clone.search(&query, 5);
            });
            handles.push(handle);
        }

        // Wait for all threads
        for handle in handles {
            handle.join().unwrap();
        }

        assert_eq!(index.len(), 100);
    }

    #[test]
    fn test_builder_invalid_dimensions() {
        let result = HnswIndexBuilder::new(0, DistanceMetric::Cosine).build();

        assert!(matches!(
            result,
            Err(Error::Vector(VectorError::InvalidVector { .. }))
        ));
    }

    #[test]
    fn test_builder_invalid_m() {
        let result = HnswIndexBuilder::new(4, DistanceMetric::Cosine)
            .m(0)
            .build();

        assert!(matches!(
            result,
            Err(Error::Vector(VectorError::InvalidVector { .. }))
        ));
    }

    #[test]
    fn test_builder_invalid_ef_construction() {
        let result = HnswIndexBuilder::new(4, DistanceMetric::Cosine)
            .ef_construction(0)
            .build();

        assert!(matches!(
            result,
            Err(Error::Vector(VectorError::InvalidVector { .. }))
        ));
    }

    #[test]
    fn test_builder_invalid_ef_search() {
        let result = HnswIndexBuilder::new(4, DistanceMetric::Cosine)
            .ef_search(0)
            .build();

        assert!(matches!(
            result,
            Err(Error::Vector(VectorError::InvalidVector { .. }))
        ));
    }

    #[test]
    fn test_builder_with_capacity() {
        let index = HnswIndexBuilder::new(4, DistanceMetric::Cosine)
            .initial_capacity(1000)
            .build()
            .unwrap();

        // Index should be created successfully
        assert_eq!(index.dimensions(), 4);
    }

    #[test]
    fn test_set_ef_search_runtime() {
        let index = HnswIndexBuilder::new(4, DistanceMetric::Cosine)
            .ef_search(10)
            .initial_capacity(100)
            .build()
            .unwrap();

        // Add some vectors
        for i in 0..20 {
            let vec = vec![i as f32 / 20.0, 0.0, 0.0, 0.0];
            index.add(NodeId::new(i), &vec).unwrap();
        }

        // Search with low ef_search
        index.set_ef_search(10);
        let query = vec![0.5, 0.0, 0.0, 0.0];
        let results_low = index.search(&query, 5).unwrap();

        // Search with high ef_search (should give same or better recall)
        index.set_ef_search(100);
        let results_high = index.search(&query, 5).unwrap();

        // Both should return results
        assert!(!results_low.is_empty());
        assert!(!results_high.is_empty());
        // High ef_search should return at least as many results with similar quality
        assert_eq!(results_low.len(), results_high.len());
    }

    #[test]
    fn test_parameter_getters() {
        let index = HnswIndexBuilder::new(128, DistanceMetric::Euclidean)
            .m(32)
            .ef_construction(400)
            .ef_search(50)
            .build()
            .unwrap();

        assert_eq!(index.m(), 32);
        assert_eq!(index.ef_construction(), 400);
        assert_eq!(index.ef_search(), 50);
        assert_eq!(index.dimensions(), 128);
        assert_eq!(index.distance_metric(), DistanceMetric::Euclidean);
    }

    #[test]
    fn test_single_vector_index() {
        let index = create_test_index();

        // Add only one vector
        let node1 = NodeId::new(1);
        let vec1 = vec![1.0, 0.0, 0.0, 0.0];
        index.add(node1, &vec1).unwrap();

        assert_eq!(index.len(), 1);

        // Search should return the single vector
        let query = vec![0.9, 0.1, 0.0, 0.0];
        let results = index.search(&query, 10).unwrap();

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].0, node1);
    }

    // ============================================================================
    // Factory Method Tests (HnswIndex::new and HnswIndex::with_capacity)
    // ============================================================================

    #[test]
    fn test_hnsw_index_new_success() {
        // Create config
        let config = HnswConfig::new(128, DistanceMetric::Cosine)
            .with_m(16)
            .with_ef_construction(200)
            .with_ef_search(64);

        // Create index using factory method
        let index = HnswIndex::new(config).unwrap();

        // Verify parameters
        assert_eq!(index.dimensions(), 128);
        assert_eq!(index.distance_metric(), DistanceMetric::Cosine);
        assert_eq!(index.m(), 16);
        assert_eq!(index.ef_construction(), 200);
        assert_eq!(index.ef_search(), 64);
        assert_eq!(index.len(), 0);
    }

    #[test]
    fn test_hnsw_index_new_with_default_config() {
        // Use default config with only required params
        let config = HnswConfig::new(384, DistanceMetric::Euclidean);

        let index = HnswIndex::new(config).unwrap();

        // Should use default values for optional params
        assert_eq!(index.dimensions(), 384);
        assert_eq!(index.distance_metric(), DistanceMetric::Euclidean);
        assert_eq!(index.m(), 16); // Default
        assert_eq!(index.ef_construction(), 128); // Default
        assert_eq!(index.ef_search(), 64); // Default
    }

    #[test]
    fn test_hnsw_index_new_with_zero_dimensions() {
        let config = HnswConfig::new(0, DistanceMetric::Cosine);

        let result = HnswIndex::new(config);

        assert!(matches!(
            result,
            Err(Error::Vector(VectorError::InvalidVector { .. }))
        ));
    }

    #[test]
    fn test_hnsw_index_new_with_zero_m() {
        let config = HnswConfig::new(128, DistanceMetric::Cosine).with_m(0);

        let result = HnswIndex::new(config);

        assert!(matches!(
            result,
            Err(Error::Vector(VectorError::InvalidVector { .. }))
        ));
    }

    #[test]
    fn test_hnsw_index_new_with_zero_ef_construction() {
        let config = HnswConfig::new(128, DistanceMetric::Cosine).with_ef_construction(0);

        let result = HnswIndex::new(config);

        assert!(matches!(
            result,
            Err(Error::Vector(VectorError::InvalidVector { .. }))
        ));
    }

    #[test]
    fn test_hnsw_index_new_with_zero_ef_search() {
        let config = HnswConfig::new(128, DistanceMetric::Cosine).with_ef_search(0);

        let result = HnswIndex::new(config);

        assert!(matches!(
            result,
            Err(Error::Vector(VectorError::InvalidVector { .. }))
        ));
    }

    #[test]
    #[cfg_attr(
        target_os = "windows",
        ignore = "IP metric crashes on Windows (usearch FFI issue)"
    )]
    fn test_hnsw_index_with_capacity_success() {
        let config = HnswConfig::new(256, DistanceMetric::DotProduct);

        // Create with explicit capacity
        let index = HnswIndex::with_capacity(config, 10_000).unwrap();

        assert_eq!(index.dimensions(), 256);
        assert_eq!(index.distance_metric(), DistanceMetric::DotProduct);
        assert_eq!(index.len(), 0);
    }

    #[test]
    fn test_hnsw_index_with_capacity_override() {
        // Config has capacity = 5000
        let config = HnswConfig::new(64, DistanceMetric::Cosine).with_capacity(5000);

        // with_capacity should override config.capacity
        let index = HnswIndex::with_capacity(config, 20_000).unwrap();

        // Should succeed (capacity is pre-allocation hint, not a hard limit)
        assert_eq!(index.dimensions(), 64);
        assert_eq!(index.len(), 0);

        // Add more than config.capacity to verify override worked
        for i in 0..100 {
            let vec = vec![i as f32 / 100.0; 64];
            index.add(NodeId::new(i), &vec).unwrap();
        }

        assert_eq!(index.len(), 100);
    }

    #[test]
    fn test_hnsw_index_with_capacity_zero_capacity() {
        let config = HnswConfig::new(32, DistanceMetric::Cosine);

        // Zero capacity is valid (means no pre-allocation)
        let index = HnswIndex::with_capacity(config, 0).unwrap();

        assert_eq!(index.dimensions(), 32);
        assert_eq!(index.len(), 0);
    }

    #[test]
    fn test_hnsw_index_new_preserves_all_config_parameters() {
        // Create config with custom values for all fields
        let config = HnswConfig::new(512, DistanceMetric::Euclidean)
            .with_m(24)
            .with_ef_construction(300)
            .with_ef_search(80)
            .with_capacity(50_000);

        let index = HnswIndex::new(config).unwrap();

        // Verify all parameters were preserved
        assert_eq!(index.dimensions(), 512);
        assert_eq!(index.distance_metric(), DistanceMetric::Euclidean);
        assert_eq!(index.m(), 24);
        assert_eq!(index.ef_construction(), 300);
        assert_eq!(index.ef_search(), 80);
    }

    #[test]
    #[cfg_attr(
        target_os = "windows",
        ignore = "IP metric crashes on Windows (usearch FFI issue)"
    )]
    fn test_hnsw_index_with_capacity_preserves_config_parameters() {
        // Create config with custom values
        let config = HnswConfig::new(1024, DistanceMetric::DotProduct)
            .with_m(32)
            .with_ef_construction(400)
            .with_ef_search(100);

        let index = HnswIndex::with_capacity(config, 100_000).unwrap();

        // Verify all config parameters preserved (except capacity which is overridden)
        assert_eq!(index.dimensions(), 1024);
        assert_eq!(index.distance_metric(), DistanceMetric::DotProduct);
        assert_eq!(index.m(), 32);
        assert_eq!(index.ef_construction(), 400);
        assert_eq!(index.ef_search(), 100);
    }

    #[test]
    fn test_hnsw_index_new_functional() {
        // Verify index created with new() actually works
        let config = HnswConfig::new(4, DistanceMetric::Cosine).with_capacity(100);
        let index = HnswIndex::new(config).unwrap();

        // Add vectors
        index.add(NodeId::new(1), &[1.0, 0.0, 0.0, 0.0]).unwrap();
        index.add(NodeId::new(2), &[0.0, 1.0, 0.0, 0.0]).unwrap();

        // Search
        let query = vec![0.9, 0.1, 0.0, 0.0];
        let results = index.search(&query, 2).unwrap();

        assert_eq!(results.len(), 2);
        assert_eq!(results[0].0, NodeId::new(1)); // Most similar
    }

    #[test]
    fn test_hnsw_index_with_capacity_functional() {
        // Verify index created with with_capacity() actually works
        let config = HnswConfig::new(4, DistanceMetric::Euclidean);
        let index = HnswIndex::with_capacity(config, 100).unwrap();

        // Add vectors
        index.add(NodeId::new(1), &[1.0, 0.0, 0.0, 0.0]).unwrap();
        index.add(NodeId::new(2), &[10.0, 0.0, 0.0, 0.0]).unwrap();

        // Search
        let query = vec![1.5, 0.0, 0.0, 0.0];
        let results = index.search(&query, 2).unwrap();

        assert_eq!(results.len(), 2);
        assert_eq!(results[0].0, NodeId::new(1)); // Closest
    }

    #[test]
    fn test_hnsw_index_new_different_metrics() {
        // Test each metric with new()
        for metric in [
            DistanceMetric::Cosine,
            DistanceMetric::Euclidean,
            DistanceMetric::DotProduct,
        ] {
            // Skip DotProduct on Windows due to usearch FFI issue
            if metric == DistanceMetric::DotProduct && cfg!(target_os = "windows") {
                continue;
            }

            let config = HnswConfig::new(8, metric);
            let index = HnswIndex::new(config).unwrap();

            assert_eq!(index.distance_metric(), metric);
            assert_eq!(index.dimensions(), 8);
        }
    }

    // ============================================================================
    // Property-Based Tests
    // ============================================================================

    use proptest::prelude::*;

    /// Strategy for generating valid f32 vectors (no NaN/Infinity)
    fn valid_f32_vector(dims: usize) -> impl Strategy<Value = Vec<f32>> {
        prop::collection::vec(
            // Generate finite floats in reasonable range
            prop::num::f32::NORMAL.prop_filter("must be finite", |f| f.is_finite()),
            dims,
        )
    }

    /// Strategy for generating valid NodeId
    fn valid_node_id() -> impl Strategy<Value = NodeId> {
        (1u64..1000u64).prop_map(NodeId::new)
    }

    proptest! {
        /// Property: Search results must be sorted by similarity (descending)
        /// Invariant: For all i < j, results[i].similarity >= results[j].similarity
        #[test]
        fn prop_search_results_sorted(
            vectors in prop::collection::vec((valid_node_id(), valid_f32_vector(4)), 1..20),
            query in valid_f32_vector(4)
        ) {
            let index = HnswIndexBuilder::new(4, DistanceMetric::Cosine)
                .initial_capacity(100)
                .build()
                .unwrap();

            // Add all vectors
            for (id, vec) in &vectors {
                let _ = index.add(*id, vec);
            }

            // Search
            let k = vectors.len().min(10);
            let results = index.search(&query, k).unwrap();

            // Verify results are sorted descending by similarity (higher = more similar)
            for i in 0..results.len().saturating_sub(1) {
                prop_assert!(
                    results[i].1 >= results[i + 1].1,
                    "Results not sorted: similarity[{}]={} < similarity[{}]={}",
                    i, results[i].1, i + 1, results[i + 1].1
                );
            }
        }

        /// Property: len() must match number of unique NodeIds added
        /// Invariant: index.len() == unique_ids_added
        #[test]
        fn prop_len_matches_unique_adds(
            operations in prop::collection::vec(
                (valid_node_id(), valid_f32_vector(4)),
                1..30
            )
        ) {
            let index = HnswIndexBuilder::new(4, DistanceMetric::Cosine)
                .initial_capacity(100)
                .build()
                .unwrap();

            // Track unique IDs
            let mut unique_ids = std::collections::HashSet::new();

            // Perform operations
            for (id, vec) in &operations {
                let _ = index.add(*id, vec);
                unique_ids.insert(*id);
            }

            // Verify len matches unique count
            prop_assert_eq!(
                index.len(),
                unique_ids.len(),
                "Index len ({}) doesn't match unique IDs added ({})",
                index.len(),
                unique_ids.len()
            );
        }

        /// Property: Adding then removing a vector should result in index.len() unchanged
        /// Invariant: len_before_add == len_after_remove
        #[test]
        fn prop_add_remove_invariant(
            initial_ops in prop::collection::vec((valid_node_id(), valid_f32_vector(4)), 1..10),
            test_id in valid_node_id(),
            test_vec in valid_f32_vector(4)
        ) {
            let index = HnswIndexBuilder::new(4, DistanceMetric::Cosine)
                .initial_capacity(100)
                .build()
                .unwrap();

            // Add initial vectors (but not test_id)
            for (id, vec) in &initial_ops {
                if *id != test_id {
                    let _ = index.add(*id, vec);
                }
            }

            let len_before = index.len();

            // Add test vector
            index.add(test_id, &test_vec).unwrap();
            prop_assert_eq!(index.len(), len_before + 1, "Add should increase len by 1");

            // Remove test vector
            index.remove(test_id).unwrap();
            prop_assert_eq!(index.len(), len_before, "Remove should restore original len");
        }

        /// Property: Search with k > index.len() should return all vectors
        /// Invariant: results.len() == min(k, index.len())
        #[test]
        fn prop_search_k_bound(
            vectors in prop::collection::vec((valid_node_id(), valid_f32_vector(4)), 1..15),
            query in valid_f32_vector(4),
            k in 1usize..100
        ) {
            let index = HnswIndexBuilder::new(4, DistanceMetric::Cosine)
                .initial_capacity(100)
                .build()
                .unwrap();

            // Track unique IDs
            let mut unique_count = 0;
            let mut seen = std::collections::HashSet::new();
            for (id, vec) in &vectors {
                let _ = index.add(*id, vec);
                if seen.insert(*id) {
                    unique_count += 1;
                }
            }

            let results = index.search(&query, k).unwrap();
            let expected_len = k.min(unique_count);

            prop_assert_eq!(
                results.len(),
                expected_len,
                "Search should return min(k={}, index.len()={}) = {} results, got {}",
                k, unique_count, expected_len, results.len()
            );
        }

        /// Property: Filtered search should only return results matching predicate
        /// Invariant: All results satisfy the predicate
        #[test]
        fn prop_filtered_search_correctness(
            vectors in prop::collection::vec((valid_node_id(), valid_f32_vector(4)), 1..20),
            query in valid_f32_vector(4),
            allowed_ids in prop::collection::hash_set(valid_node_id(), 1..10)
        ) {
            let index = HnswIndexBuilder::new(4, DistanceMetric::Cosine)
                .initial_capacity(100)
                .build()
                .unwrap();

            // Add vectors
            for (id, vec) in &vectors {
                let _ = index.add(*id, vec);
            }

            // Search with filter
            let results = index
                .search_with_filter(&query, 10, |id| allowed_ids.contains(id))
                .unwrap();

            // Verify all results match predicate
            for (id, _dist) in &results {
                prop_assert!(
                    allowed_ids.contains(id),
                    "Result {:?} not in allowed set",
                    id
                );
            }
        }

        /// Property: Index handles extreme (but finite) values without panicking
        /// Tests numerical stability with very large and very small values
        #[test]
        fn prop_handles_extreme_values(
            vectors in prop::collection::vec(
                (
                    valid_node_id(),
                    prop::collection::vec(
                        (-1e6f32..1e6f32).prop_filter("finite", |f| f.is_finite()),
                        4
                    )
                ),
                1..10
            ),
            query in prop::collection::vec(
                (-1e6f32..1e6f32).prop_filter("finite", |f| f.is_finite()),
                4
            )
        ) {
            let index = HnswIndexBuilder::new(4, DistanceMetric::Cosine)
                .initial_capacity(100)
                .build()
                .unwrap();

            // Add all vectors - should not panic
            for (id, vec) in &vectors {
                let _ = index.add(*id, vec);
            }

            // Search with extreme values - should not panic
            let results = index.search(&query, 5);

            // Either succeeds or returns valid error (not panic)
            match results {
                Ok(res) => {
                    // Results should be sorted
                    for i in 0..res.len().saturating_sub(1) {
                        prop_assert!(
                            res[i].1 >= res[i + 1].1,
                            "Results not sorted with extreme values"
                        );
                    }
                }
                Err(e) => {
                    // If it errors, it should be a proper error (not panic)
                    // This would only happen if usearch has numerical issues
                    prop_assert!(
                        matches!(e, Error::Vector(_)),
                        "Unexpected error type with extreme values: {:?}",
                        e
                    );
                }
            }
        }
    }
}
